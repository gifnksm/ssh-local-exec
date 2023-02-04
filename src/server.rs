use std::{io, os::unix::prelude::ExitStatusExt, process::Stdio, sync::Arc};

use bytes::BytesMut;
use color_eyre::eyre::{self, bail, eyre, WrapErr as _};
use futures::{
    channel::oneshot, future, stream, Sink, SinkExt as _, StreamExt as _, TryStream,
    TryStreamExt as _,
};
use tokio::{
    process::{self, Child},
    sync::mpsc,
    task::JoinSet,
};
use tokio_shutdown::Shutdown;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use crate::{
    args::ListenAddress,
    protocol::{
        self, ClientMessage, Exit, OutputRequest, OutputResponse, ServerMessage, Signal,
        SpawnMessage,
    },
    socket::{SocketListener, SocketStream},
};
use tracing::Instrument;

pub async fn main(listen_address: &ListenAddress) -> eyre::Result<()> {
    let listener = SocketListener::bind(&listen_address)
        .await
        .wrap_err_with(|| format!("failed to bind socket: {listen_address}"))?;

    tracing::info!("listening on {listen_address}");

    let shutdown = Shutdown::new()?;
    listen_client(listener, shutdown)
        .instrument(tracing::info_span!("listen", addr = %listen_address))
        .await?;

    tracing::info!("shutting down");

    Ok(())
}

async fn listen_client(listener: SocketListener, shutdown: Shutdown) -> eyre::Result<()> {
    let mut join_set = JoinSet::new();

    for client_id in 0.. {
        let res = tokio::select! {
            _ = shutdown.handle() => break,
            res = listener.accept() => res,
        };

        let (stream, addr) = match res {
            Ok(res) => res,
            Err(err) => {
                tracing::info!("failed to accept: {err}");
                continue;
            }
        };

        join_set.spawn(
            async move {
                tracing::info!("accepted connection from {}", addr);
                match handle_client_connection(stream).await {
                    Ok(()) => tracing::info!("client handler finished"),
                    Err(err) => {
                        tracing::error!("client handler aborted due to error: {err:?}")
                    }
                }
            }
            .instrument(tracing::info_span!("client", id = client_id)),
        );
    }

    Ok(())
}

async fn handle_client_connection(stream: SocketStream) -> eyre::Result<()> {
    let (read_stream, write_stream) = stream.into_split();

    let mut bytes_rx = FramedRead::new(read_stream, LengthDelimitedCodec::new());
    let bytes_tx = FramedWrite::new(write_stream, LengthDelimitedCodec::new());

    let spawn_msg: SpawnMessage = protocol::new_receiver(&mut bytes_rx)
        .try_next()
        .await
        .wrap_err("failed to receive spawn message")?
        .ok_or_else(|| eyre!("client sent no request"))?;

    let client_tx = protocol::new_sender::<_, ServerMessage>(bytes_tx);
    let client_rx = protocol::new_receiver::<_, ClientMessage>(bytes_rx);

    handle_client_request(spawn_msg, client_tx, client_rx).await?;

    Ok(())
}

async fn handle_client_request(
    spawn_msg: SpawnMessage,
    mut client_tx: impl Sink<ServerMessage, Error = io::Error> + Send + Unpin + 'static,
    client_rx: impl TryStream<Ok = ClientMessage, Error = io::Error> + Send + Unpin + 'static,
) -> eyre::Result<()> {
    let SpawnMessage { command, args } = spawn_msg;

    tracing::debug!("received spawn_msg: {:?}", command);

    let mut cmd = process::Command::new(&command);
    cmd.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match cmd.spawn().wrap_err("failed to spawn child process") {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!("{e:?}");
            // notify error exit status to client
            client_tx
                .send(ServerMessage::Exit(Exit::OtherError(format!("{e:?}"))))
                .await?;
            // close output channels
            client_tx
                .send(ServerMessage::Stdout(OutputRequest::Terminated))
                .await?;
            client_tx
                .send(ServerMessage::Stderr(OutputRequest::Terminated))
                .await?;
            return Ok(());
        }
    };
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    tracing::debug!("spawned child process: {:?}", child.id());

    let mut join_set = JoinSet::new();

    let (exit_tx, exit_rx) = oneshot::channel();
    let (signal_tx, signal_rx) = mpsc::channel(1);
    join_set.spawn(wait_child(child, exit_tx, signal_rx).in_current_span());

    let (stdin_bytes_tx, stdin_bytes_rx) = mpsc::channel(1);
    let (stdin_res_tx, stdin_res_rx) = mpsc::channel(1);
    join_set
        .spawn(crate::task::write("stdin", stdin, stdin_res_tx, stdin_bytes_rx).in_current_span());

    let (stdout_bytes_tx, stdout_bytes_rx) = mpsc::channel(1);
    let (stdout_res_tx, stdout_res_rx) = mpsc::channel(1);
    join_set.spawn(
        crate::task::read("stdout", stdout, stdout_bytes_tx, stdout_res_rx).in_current_span(),
    );

    let (stderr_bytes_tx, stderr_bytes_rx) = mpsc::channel(1);
    let (stderr_res_tx, stderr_res_rx) = mpsc::channel(1);
    join_set.spawn(
        crate::task::read("stderr", stderr, stderr_bytes_tx, stderr_res_rx).in_current_span(),
    );

    join_set.spawn(
        receive_client(
            client_rx,
            signal_tx,
            stdin_bytes_tx,
            stdout_res_tx,
            stderr_res_tx,
        )
        .in_current_span(),
    );

    join_set.spawn(
        send_client(
            client_tx,
            exit_rx,
            stdin_res_rx,
            stdout_bytes_rx,
            stderr_bytes_rx,
        )
        .in_current_span(),
    );

    while let Some(res) = join_set.join_next().await {
        res.wrap_err("task join failure").and_then(|r| r)?;
    }

    tracing::debug!("all tasks finished");

    Ok(())
}

#[tracing::instrument(level = "debug", err, ret, skip_all)]
async fn wait_child(
    mut child: Child,
    exit_tx: oneshot::Sender<Exit>,
    mut signal_rx: mpsc::Receiver<Signal>,
) -> eyre::Result<()> {
    // If waiting for the child process failed, some unexpected system error may have occurred.
    // In this case, all related server tasks should be aborted.
    // To abort all tasks, return an error here.
    let exit_status = loop {
        tokio::select! {
            status = child.wait() => break status.wrap_err("failed to wait child process")?,
            signal = signal_rx.recv() => {
                tracing::debug!("received kill signal");
                if let (Some(pid), Some(signal)) = (child.id(), signal) {
                    // ignore error
                    let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), signal);
                }
            }
        }
    };

    let exit = if let Some(code) = exit_status.code() {
        tracing::debug!("child process exited with code: {}", code);
        Exit::Code(code)
    } else if let Some(signal) = exit_status.signal() {
        tracing::debug!("child process exited with signal: {}", signal);
        Exit::Signal(signal)
    } else {
        // As wait() failure, this error is unexpected.
        bail!("child process exited with unknown status");
    };

    if let Err(err) = exit_tx
        .send(exit)
        .map_err(|_| eyre!("failed to send message to exit_tx"))
    {
        // The only reason for failure in sending a message to another task is that the task has already been terminated.
        // In this case, the program has already started to abort, so it simply terminates without generating any errors in this task.
        tracing::debug!("{err}");
        return Ok(());
    }

    Ok(())
}

#[tracing::instrument(level = "debug", err, ret, skip_all)]
async fn receive_client(
    mut client_rx: impl TryStream<Ok = ClientMessage, Error = io::Error> + Unpin,
    signal_tx: mpsc::Sender<Signal>,
    stdin_tx: mpsc::Sender<Arc<BytesMut>>,
    stdout_tx: mpsc::Sender<Result<(), String>>,
    stderr_tx: mpsc::Sender<Result<(), String>>,
) -> eyre::Result<()> {
    let mut shutdown = false;
    let mut stdin_tx = Some(stdin_tx);
    let mut stdout_tx = Some(stdout_tx);
    let mut stderr_tx = Some(stderr_tx);

    // If receiving messages from the client failed, there may have been a problem with the connection to the client.
    // In this case, all related server tasks should be aborted.
    // To abort all tasks, return an error here.
    while let Some(msg) = client_rx
        .try_next()
        .await
        .wrap_err("failed to receive message from client")?
    {
        tracing::trace!("received message: {:?}", msg);

        let res = match msg {
            ClientMessage::Signal(msg) => {
                // ignore error
                let _ = signal_tx.send(msg).await;
                Ok(())
            }
            ClientMessage::Stdin(msg) => match msg {
                OutputRequest::Output(msg) => {
                    // ignore error
                    let _ = stdin_tx.as_mut().unwrap().send(msg).await;
                    Ok(())
                }
                OutputRequest::Terminated => {
                    tracing::debug!("client stdin task terminated");
                    let _ = stdin_tx.take();
                    Ok(())
                }
            },
            ClientMessage::Stdout(msg) => match msg {
                OutputResponse::Output(msg) => stdout_tx
                    .as_mut()
                    .unwrap()
                    .send(msg)
                    .await
                    .map_err(|_| eyre!("failed to send message to stdout task")),
                OutputResponse::Terminated => {
                    tracing::debug!("server and client stdout tasks terminated");
                    let _ = stdout_tx.take();
                    Ok(())
                }
            },
            ClientMessage::Stderr(msg) => match msg {
                OutputResponse::Output(msg) => stderr_tx
                    .as_mut()
                    .unwrap()
                    .send(msg)
                    .await
                    .map_err(|_| eyre!("failed to send message to stderr task")),
                OutputResponse::Terminated => {
                    tracing::debug!("server and client stderr tasks terminated");
                    let _ = stderr_tx.take();
                    Ok(())
                }
            },
            ClientMessage::Shutdown => {
                tracing::debug!("client send task shutdown");
                shutdown = true;
                break;
            }
        };

        if let Err(err) = res {
            // The only reason for failure in sending a message to another task is that the task has already been terminated.
            // In this case, the program has already started to abort, so it simply terminates without generating any errors in this task.
            tracing::debug!("{err}");
            break;
        }
    }

    if let Ok(()) = signal_tx.send(Signal::Kill).await {
        // If the process has already been terminated, the message will fail to be sent and will not be logged even if an error occurs.
        tracing::debug!("sent kill signal, maybe client disconnected unexpectedly");
    }

    if !shutdown {
        bail!("client disconnected unexpectedly");
    }

    Ok(())
}

#[tracing::instrument(level = "debug", err, ret, skip_all)]
async fn send_client(
    mut client_tx: impl Sink<ServerMessage, Error = io::Error> + Send + Unpin + 'static,
    exit_rx: oneshot::Receiver<Exit>,
    stdin_res_rx: mpsc::Receiver<Result<(), String>>,
    stdout_bytes_rx: mpsc::Receiver<Arc<BytesMut>>,
    stderr_bytes_rx: mpsc::Receiver<Arc<BytesMut>>,
) -> eyre::Result<()> {
    let exit = stream::once(exit_rx).map(|res| res.map(ServerMessage::Exit));
    let stdin = ReceiverStream::new(stdin_res_rx)
        .map(OutputResponse::Output)
        .chain(stream::once(future::ready(OutputResponse::Terminated)))
        .map(ServerMessage::Stdin)
        .map(Ok);
    let stdout = ReceiverStream::new(stdout_bytes_rx)
        .map(OutputRequest::Output)
        .chain(stream::once(future::ready(OutputRequest::Terminated)))
        .map(ServerMessage::Stdout)
        .map(Ok);
    let stderr = ReceiverStream::new(stderr_bytes_rx)
        .map(OutputRequest::Output)
        .chain(stream::once(future::ready(OutputRequest::Terminated)))
        .map(ServerMessage::Stderr)
        .map(Ok);

    let mut stream = stream::select(exit, stream::select(stdin, stream::select(stdout, stderr)))
        .chain(stream::once(future::ready(Ok(ServerMessage::Shutdown))));
    while let Some(msg) = stream.next().await {
        let msg = match msg {
            Ok(msg) => msg,
            Err(err) => {
                // The only reason for failure in receiving a message from another task is that the task has already been terminated.
                // In this case, the program has already started to abort, so it simply terminates without generating any errors in this task.
                tracing::debug!("{err}");
                break;
            }
        };

        tracing::trace!("sending message: {msg:?}");
        // If sending message to the client failed, there may have been a problem with the connection to the client.
        // In this case, all related server tasks should be aborted.
        // To abort all tasks, return an error here.
        client_tx
            .send(msg)
            .await
            .wrap_err("failed to send message to client")?;
        tracing::trace!("message sent to server");
    }

    Ok(())
}
