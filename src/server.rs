use std::{io, mem, os::unix::prelude::ExitStatusExt, process::Stdio, ptr, sync::Arc};

use bytes::BytesMut;
use color_eyre::eyre::{self, bail, eyre, WrapErr as _};
use futures::{
    channel::oneshot, future, stream, Sink, SinkExt as _, StreamExt as _, TryFutureExt as _,
    TryStream, TryStreamExt as _,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
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
        CborDecoder, CborEncoder, ClientMessage, DecodeError, EncodeError, Exit, OutputRequest,
        OutputResponse, ServerMessage, Signal, SpawnRequest, SpawnResponse,
    },
    socket::{SocketListener, SocketStream},
};
use tracing::Instrument;

pub async fn main(listen_address: &[ListenAddress]) -> eyre::Result<()> {
    // Check that all listen addresses can be bound to sockets
    let listeners = future::join_all(listen_address.iter().map(|addr| {
        SocketListener::bind(addr)
            .map_err(move |err| eyre!(err).wrap_err(format!("failed to bind socket: {addr}")))
    }))
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;

    // Start listening for clients
    let shutdown = Shutdown::new()?;
    let mut join_set = JoinSet::new();
    for listener in listeners {
        let shutdown = shutdown.clone();
        join_set.spawn(listen_client(listener, shutdown));
    }

    // Wait all listeners to finish
    while let Some(res) = join_set.join_next().await {
        res.wrap_err("task join failure").and_then(|r| r)?;
    }
    tracing::info!("shutting down");

    Ok(())
}

async fn listen_client(listener: SocketListener, shutdown: Shutdown) -> eyre::Result<()> {
    let mut join_set = JoinSet::new();

    tracing::info!("listening on {}", listener.local_addr()?);

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

    let client_rx =
        FramedRead::new(read_stream, LengthDelimitedCodec::new()).map_decoder(CborDecoder::new);
    let client_tx =
        FramedWrite::new(write_stream, LengthDelimitedCodec::new()).map_encoder(CborEncoder::new);

    handle_client_request(client_tx, client_rx).await?;

    Ok(())
}

async fn handle_client_request(
    mut client_tx: impl Sink<ServerMessage, Error = EncodeError> + Send + Unpin + 'static,
    mut client_rx: impl TryStream<Ok = ClientMessage, Error = DecodeError> + Send + Unpin + 'static,
) -> eyre::Result<()> {
    let spawn_req = client_rx
        .try_next()
        .await
        .wrap_err("failed to receive message from client")?;

    let spawn_req = match spawn_req {
        Some(ClientMessage::SpawnRequest(spawn_req)) => spawn_req,
        Some(_) => bail!("unexpected message from client"),
        None => bail!("client disconnected"),
    };

    let SpawnRequest { command, args } = spawn_req;

    tracing::debug!("received spawn_msg: {:?}", command);

    let mut cmd = process::Command::new(&command);
    cmd.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let child = cmd.spawn().wrap_err("failed to spawn child process");

    let spawn_resp = match &child {
        Ok(child) => {
            tracing::debug!("spawned child process: {:?}", child.id());
            SpawnResponse(Ok(()))
        }
        Err(err) => {
            tracing::warn!("{err}");
            SpawnResponse(Err(format!("{err:?}")))
        }
    };
    let spawn_resp = ServerMessage::SpawnResponse(spawn_resp);

    client_tx
        .send(spawn_resp)
        .await
        .wrap_err("failed to send spawn response")?;

    let mut child = match child {
        Ok(child) => child,
        Err(_) => return Ok(()),
    };
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    let mut join_set = JoinSet::new();

    let (exit_tx, exit_rx) = oneshot::channel();
    let (signal_tx, signal_rx) = mpsc::channel(1);
    join_set.spawn(wait_child(child, exit_tx, signal_rx).in_current_span());

    let (stdin_bytes_tx, stdin_bytes_rx) = mpsc::channel(1);
    let (stdin_res_tx, stdin_res_rx) = mpsc::channel(1);
    join_set.spawn(write("stdin", stdin, stdin_res_tx, stdin_bytes_rx).in_current_span());

    let (stdout_bytes_tx, stdout_bytes_rx) = mpsc::channel(1);
    let (stdout_res_tx, stdout_res_rx) = mpsc::channel(1);
    join_set.spawn(read("stdout", stdout, stdout_bytes_tx, stdout_res_rx).in_current_span());

    let (stderr_bytes_tx, stderr_bytes_rx) = mpsc::channel(1);
    let (stderr_res_tx, stderr_res_rx) = mpsc::channel(1);
    join_set.spawn(read("stderr", stderr, stderr_bytes_tx, stderr_res_rx).in_current_span());

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
    mut client_rx: impl TryStream<Ok = ClientMessage, Error = DecodeError> + Unpin,
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
            ClientMessage::SpawnRequest(msg) => {
                tracing::debug!("invalid mesage received: {msg:?}");
                Err(eyre!("invalid message received: {msg:?}"))
            }
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
    mut client_tx: impl Sink<ServerMessage, Error = EncodeError> + Send + Unpin + 'static,
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

const BUFFER_SIZE: usize = 4 * 1024;

#[tracing::instrument(level = "debug", err, ret, skip_all, fields(ty = ty))]
pub async fn read(
    ty: &'static str,
    mut input: impl AsyncRead + Unpin,
    tx: mpsc::Sender<Arc<BytesMut>>,
    mut rx: mpsc::Receiver<Result<(), String>>,
) -> eyre::Result<()> {
    let mut bytes = BytesMut::new();
    bytes.resize(BUFFER_SIZE, 0);

    let mut send_bytes = Arc::new(BytesMut::new());

    loop {
        let read_size = match input.read(&mut bytes).await {
            Ok(0) => {
                tracing::trace!("terminated");
                break;
            }
            Ok(size) => size,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            // If reading from input failed, some unexpected system error may have occurred.
            // In this case, all related server tasks should be aborted.
            // To abort all tasks, return an error here.
            Err(err) => return Err(eyre!(err)).wrap_err("failed to read {ty}"),
        };

        tracing::trace!("{} bytes read", read_size);

        let dummy_buf = mem::replace(
            Arc::get_mut(&mut send_bytes).unwrap(),
            bytes.split_to(read_size),
        );
        assert!(dummy_buf.is_empty());

        // The only reason for failure in sending a message to another task is that the task has already been terminated.
        // In this case, the program has already started to abort, so it simply terminates without generating any errors in this task.
        if let Err(err) = tx
            .send(Arc::clone(&send_bytes))
            .await
            .wrap_err("failed to send bytes")
        {
            tracing::debug!("{err}");
            break;
        }
        tracing::trace!("bytes sent");

        match rx.recv().await {
            Some(Ok(())) => tracing::trace!("ack received"),
            Some(Err(err)) => {
                // If output end returned an error, some unexpected system error may have occurred.
                // In this case, all related server tasks should be aborted.
                // To abort all tasks, return an error here.
                return Err(eyre!(err)).wrap_err_with(|| format!("failed to write {ty}"));
            }
            None => {
                // The only reason for channel to be closed is that the another task has already been terminated.
                // In this case, the program has already started to abort, so it simply terminates without generating any errors in this task.
                tracing::debug!("failed to receive ack");
                break;
            }
        }

        let unsend_bytes = bytes;
        bytes = mem::replace(Arc::get_mut(&mut send_bytes).unwrap(), BytesMut::new());
        let bytes_ptr = bytes.as_ptr();
        bytes.unsplit(unsend_bytes);
        assert_eq!(bytes.len(), BUFFER_SIZE);
        // ensure that the pointer is not changed (allocation is not performed)
        assert!(ptr::eq(bytes.as_ptr(), bytes_ptr));
    }

    Ok(())
}

#[tracing::instrument(level = "debug", err, ret, skip_all, fields(ty = ty))]
pub async fn write(
    ty: &'static str,
    mut output: impl AsyncWrite + Unpin,
    tx: mpsc::Sender<Result<(), String>>,
    mut rx: mpsc::Receiver<Arc<BytesMut>>,
) -> eyre::Result<()> {
    while let Some(bytes) = rx.recv().await {
        tracing::trace!("{} bytes received", bytes.len());

        let res = loop {
            match output.write_all(&bytes).await {
                Ok(()) => {
                    tracing::trace!("bytes written");
                    break Ok(());
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                // In this case, send an error response to the input end.
                // Input end will trigger the abortion of all related server tasks.
                Err(err) => {
                    tracing::error!("failed to write bytes: {err}");
                    break Err(err.to_string());
                }
            }
        };

        if let Err(err) = tx.send(res).await {
            // The only reason for failure in sending a message to another task is that the task has already been terminated.
            // In this case, the program has already started to abort, so it simply terminates without generating any errors in this task.
            tracing::debug!("{err}");
            break;
        }
        tracing::trace!("result sent");
    }

    Ok(())
}
