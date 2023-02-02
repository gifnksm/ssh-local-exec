use std::{io, os::unix::prelude::ExitStatusExt, process::Stdio, sync::Arc};

use bytes::BytesMut;
use color_eyre::eyre::{self, eyre, WrapErr as _};
use futures::{
    channel::oneshot, future, stream, Sink, SinkExt as _, StreamExt as _, TryStream,
    TryStreamExt as _,
};
use tokio::{
    process::{self, Child},
    sync::mpsc,
};
use tokio_shutdown::Shutdown;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use crate::{
    args::ListenAddress,
    protocol::{
        self, ClientMessage, Exit, OutputRequest, OutputResponse, ServerMessage, SpawnMessage,
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
    for client_id in 0.. {
        tokio::select! {
            _ = shutdown.handle() => {
                break
            },
            res = listener.accept() => {
                match res {
                    Ok((stream, addr)) => {
                        tokio::spawn(
                            async move {
                                tracing::info!("accepted connection from {}", addr);
                                if let Err(e) = handle_client_connection(stream).await {
                                    tracing::error!("{e:?}");
                                }
                            }
                            .instrument(tracing::info_span!("client", id = client_id)),
                        );
                    }
                    Err(e) => tracing::info!("failed to accept: {e}"),
                }
            }
        }
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

    let (exit_tx, exit_rx) = oneshot::channel();
    tokio::spawn(wait_child(child, exit_tx).in_current_span());

    let (stdin_bytes_tx, stdin_bytes_rx) = mpsc::channel(1);
    let (stdin_res_tx, stdin_res_rx) = mpsc::channel(1);
    tokio::spawn(
        crate::task::write("stdin", stdin, stdin_res_tx, stdin_bytes_rx).in_current_span(),
    );

    let (stdout_bytes_tx, stdout_bytes_rx) = mpsc::channel(1);
    let (stdout_res_tx, stdout_res_rx) = mpsc::channel(1);
    tokio::spawn(
        crate::task::read("stdout", stdout, stdout_bytes_tx, stdout_res_rx).in_current_span(),
    );

    let (stderr_bytes_tx, stderr_bytes_rx) = mpsc::channel(1);
    let (stderr_res_tx, stderr_res_rx) = mpsc::channel(1);
    tokio::spawn(
        crate::task::read("stderr", stderr, stderr_bytes_tx, stderr_res_rx).in_current_span(),
    );

    tokio::spawn(
        receive_client(client_rx, stdin_bytes_tx, stdout_res_tx, stderr_res_tx).in_current_span(),
    );

    tokio::spawn(
        send_client(
            client_tx,
            exit_rx,
            stdin_res_rx,
            stdout_bytes_rx,
            stderr_bytes_rx,
        )
        .in_current_span(),
    );

    Ok(())
}

#[tracing::instrument(level = "debug", err, ret, skip_all)]
async fn wait_child(mut child: Child, exit_tx: oneshot::Sender<Exit>) -> eyre::Result<()> {
    let exit = match child.wait().await {
        Ok(status) => {
            if let Some(code) = status.code() {
                tracing::debug!("child process exited with code: {}", code);
                Exit::Code(code)
            } else if let Some(signal) = status.signal() {
                tracing::debug!("child process exited with signal: {}", signal);
                Exit::Signal(signal)
            } else {
                Exit::OtherError("child process exited with unknown status".into())
            }
        }
        Err(e) => {
            tracing::error!("child process exited with error: {e}", e = e);
            Exit::OtherError(e.to_string())
        }
    };

    exit_tx.send(exit).unwrap();

    Ok(())
}

#[tracing::instrument(level = "debug", err, ret, skip_all)]
async fn receive_client(
    mut client_rx: impl TryStream<Ok = ClientMessage, Error = io::Error> + Unpin,
    stdin_tx: mpsc::Sender<Arc<BytesMut>>,
    stdout_tx: mpsc::Sender<Result<(), String>>,
    stderr_tx: mpsc::Sender<Result<(), String>>,
) -> eyre::Result<()> {
    let mut stdin_tx = Some(stdin_tx);

    while let Some(msg) = client_rx
        .try_next()
        .await
        .wrap_err("failed to receive message")?
    {
        tracing::trace!("received message: {:?}", msg);
        match msg {
            ClientMessage::Stdin(msg) => match msg {
                OutputRequest::Output(msg) => {
                    stdin_tx
                        .as_mut()
                        .unwrap()
                        .send(msg)
                        .await
                        .wrap_err("failed to send message")?;
                }
                OutputRequest::Terminated => {
                    let _ = stdin_tx.take();
                }
            },
            ClientMessage::Stdout(msg) => match msg {
                OutputResponse(msg) => {
                    stdout_tx
                        .send(msg)
                        .await
                        .wrap_err("failed to send message")?;
                }
            },
            ClientMessage::Stderr(msg) => match msg {
                OutputResponse(msg) => {
                    stderr_tx
                        .send(msg)
                        .await
                        .wrap_err("failed to send message")?;
                }
            },
        }
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
    let exit = stream::once(exit_rx).map(|res| res.map(ServerMessage::Exit).unwrap());
    let stdin = ReceiverStream::new(stdin_res_rx)
        .map(OutputResponse)
        .map(ServerMessage::Stdin);
    let stdout = ReceiverStream::new(stdout_bytes_rx)
        .map(OutputRequest::Output)
        .chain(stream::once(future::ready(OutputRequest::Terminated)))
        .map(ServerMessage::Stdout);
    let stderr = ReceiverStream::new(stderr_bytes_rx)
        .map(OutputRequest::Output)
        .chain(stream::once(future::ready(OutputRequest::Terminated)))
        .map(ServerMessage::Stderr);

    let mut stream = stream::select(exit, stream::select(stdin, stream::select(stdout, stderr)));
    while let Some(msg) = stream.next().await {
        tracing::trace!("sending message: {msg:?}");
        match client_tx.send(msg).await {
            Ok(()) => tracing::trace!("message sent"),
            Err(e) => tracing::error!("failed to send message: {e:?}"),
        }
    }

    Ok(())
}
