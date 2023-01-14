use std::{io, os::unix::prelude::ExitStatusExt, process::Stdio, sync::Arc};

use bytes::BytesMut;
use clap::Parser as _;
use color_eyre::eyre::{self, eyre, WrapErr as _};
use futures::{channel::oneshot, future, stream, SinkExt, StreamExt as _, TryStreamExt};
use ssh_local_exec::{
    self as sle,
    protocol::{
        self, ClientMessage, Exit, OutputRequest, OutputResponse, ServerMessage, SpawnMessage,
    },
    socket::{SocketListener, SocketStream},
};
use tokio::{process, sync::mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};
use tracing::Instrument;

/// Server for executing commands on the SSH local host
#[derive(Debug, clap::Parser)]
#[clap(author, version, about)]
struct Args {
    /// internet socket address (address:port) or Unix socket address (path)
    #[clap(
        short,
        long = "bind",
        value_name = "ADDRESS",
        env = "SSH_LOCAL_EXEC_SERVER_BIND_ADDR"
    )]
    bind_addr: String,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    let Args { bind_addr } = Args::parse();

    let listener = SocketListener::bind(&bind_addr)
        .await
        .wrap_err_with(|| format!("failed to bind socket: {bind_addr}"))?;

    for client_id in 0.. {
        match listener.accept().await {
            Ok((stream, addr)) => {
                tokio::spawn(
                    async move {
                        tracing::info!("accepted connection from {}", addr);
                        if let Err(e) = handle_client(stream).await {
                            tracing::error!("{e:?}");
                        }
                    }
                    .instrument(tracing::info_span!("client", id = client_id)),
                );
            }
            Err(e) => tracing::info!("failed to accept: {e}"),
        }
    }

    unreachable!()
}

#[tracing::instrument(level = "info", err, ret, skip_all)]
async fn handle_client(stream: SocketStream) -> eyre::Result<()> {
    let (read_stream, write_stream) = stream.into_split();
    let mut read_stream = FramedRead::new(read_stream, LengthDelimitedCodec::new());
    let write_stream = FramedWrite::new(write_stream, LengthDelimitedCodec::new());

    let SpawnMessage { command, args } = protocol::new_receiver(&mut read_stream)
        .try_next()
        .await
        .wrap_err("failed to receive message")?
        .ok_or_else(|| eyre!("client sent no request"))?;

    tracing::debug!("received request: {:?}", command);

    let mut cmd = process::Command::new(&command);
    cmd.args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().wrap_err("failed to spawn child process")?;
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();

    tracing::debug!("spawned child process: {:?}", child.id());

    let receiver = protocol::new_receiver::<_, ClientMessage>(read_stream);
    let mut sender = protocol::new_sender::<_, ServerMessage>(write_stream);

    let (exit_tx, exit_rx) = oneshot::channel();
    tokio::spawn(
        async move {
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
        }
        .in_current_span()
        .instrument(tracing::info_span!("exit")),
    );

    let (stdin_bytes_tx, stdin_bytes_rx) = mpsc::channel(1);
    let (stdin_res_tx, stdin_res_rx) = mpsc::channel(1);
    tokio::spawn(
        sle::task::output(stdin, stdin_res_tx, stdin_bytes_rx)
            .in_current_span()
            .instrument(tracing::info_span!("stdin")),
    );

    let (stdout_bytes_tx, stdout_bytes_rx) = mpsc::channel(1);
    let (stdout_res_tx, stdout_res_rx) = mpsc::channel(1);
    tokio::spawn(
        sle::task::input(stdout, stdout_bytes_tx, stdout_res_rx)
            .in_current_span()
            .instrument(tracing::info_span!("stdout")),
    );

    let (stderr_bytes_tx, stderr_bytes_rx) = mpsc::channel(1);
    let (stderr_res_tx, stderr_res_rx) = mpsc::channel(1);
    tokio::spawn(
        sle::task::input(stderr, stderr_bytes_tx, stderr_res_rx)
            .in_current_span()
            .instrument(tracing::info_span!("stderr")),
    );

    tokio::spawn(receive(receiver, stdin_bytes_tx, stdout_res_tx, stderr_res_tx).in_current_span());
    tokio::spawn(
        async move {
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
            let mut stream =
                stream::select(exit, stream::select(stdin, stream::select(stdout, stderr)));
            while let Some(msg) = stream.next().await {
                tracing::trace!("sending message: {msg:?}");
                match sender.send(msg).await {
                    Ok(()) => tracing::trace!("message sent"),
                    Err(e) => tracing::error!("failed to send message: {e:?}"),
                }
            }
        }
        .in_current_span()
        .instrument(tracing::info_span!("send")),
    );

    Ok(())
}

#[tracing::instrument(level = "debug", err, ret, skip_all)]
async fn receive(
    mut receiver: impl TryStreamExt<Ok = ClientMessage, Error = io::Error> + Unpin,
    stdin_tx: mpsc::Sender<Arc<BytesMut>>,
    stdout_tx: mpsc::Sender<Result<(), String>>,
    stderr_tx: mpsc::Sender<Result<(), String>>,
) -> eyre::Result<()> {
    let mut stdin_tx = Some(stdin_tx);
    while let Some(msg) = receiver
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
