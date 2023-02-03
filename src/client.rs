use std::{
    io,
    process::ExitCode,
    sync::{Arc, Mutex},
    thread,
};

use bytes::BytesMut;
use color_eyre::eyre::{self, eyre, WrapErr as _};
use futures::{future, stream, Sink, SinkExt as _, StreamExt as _, TryStream, TryStreamExt as _};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};
use tracing::Instrument;

use crate::{
    args::ConnectAddress,
    protocol::{
        self, ClientMessage, Exit, OutputRequest, OutputResponse, ServerMessage, SpawnMessage,
    },
    socket::SocketStream,
};

pub async fn main(
    connect_address: &ConnectAddress,
    command: String,
    args: Vec<String>,
) -> eyre::Result<ExitCode> {
    let stream = SocketStream::connect(connect_address)
        .await
        .wrap_err_with(|| format!("failed to connect socket: {connect_address}"))?;

    let (read_stream, write_stream) = stream.into_split();

    let bytes_rx = FramedRead::new(read_stream, LengthDelimitedCodec::new());
    let mut bytes_tx = FramedWrite::new(write_stream, LengthDelimitedCodec::new());

    protocol::new_sender(&mut bytes_tx)
        .send(SpawnMessage { command, args })
        .await
        .wrap_err("failed to send spawn request")?;

    let server_rx = protocol::new_receiver::<_, ServerMessage>(bytes_rx);
    let server_tx = protocol::new_sender::<_, ClientMessage>(bytes_tx);

    let mut join_set = JoinSet::new();

    let (stdin_bytes_tx, stdin_bytes_rx) = mpsc::channel(1);
    let (stdin_res_tx, stdin_res_rx) = mpsc::channel(1);
    let stdin_bytes_tx = Arc::new(Mutex::new(Some(stdin_bytes_tx)));
    let _stdin_thread = {
        let stdin_bytes_tx = Arc::clone(&stdin_bytes_tx);
        thread::Builder::new()
            .name("stdin".into())
            .spawn(|| {
                crate::thread::read("stdin", io::stdin(), stdin_bytes_tx, stdin_res_rx)
                    .in_current_span()
            })
            .wrap_err("failed to spawn thread")?
    };

    let (stdout_bytes_tx, stdout_bytes_rx) = mpsc::channel(1);
    let (stdout_res_tx, stdout_res_rx) = mpsc::channel(1);
    let stdout_thread = thread::Builder::new()
        .name("stdout".into())
        .spawn(|| {
            crate::thread::write("stdout", io::stdout(), stdout_res_tx, stdout_bytes_rx)
                .in_current_span()
        })
        .wrap_err("failed to spawn thread")?;

    let (stderr_bytes_tx, stderr_bytes_rx) = mpsc::channel(1);
    let (stderr_res_tx, stderr_res_rx) = mpsc::channel(1);
    let stderr_thread = thread::Builder::new()
        .name("stderr".into())
        .spawn(|| {
            crate::thread::write("stderr", io::stderr(), stderr_res_tx, stderr_bytes_rx)
                .in_current_span()
        })
        .wrap_err("failed to spawn thread")?;

    let (exit_code_tx, exit_code_rx) = oneshot::channel();
    join_set.spawn(receive_server(
        server_rx,
        exit_code_tx,
        stdin_bytes_tx,
        stdin_res_tx,
        stdout_bytes_tx,
        stderr_bytes_tx,
    ));
    join_set.spawn(
        send_client(server_tx, stdin_bytes_rx, stdout_res_rx, stderr_res_rx).in_current_span(),
    );

    // Wait for output threads to complete to ensure that all outputs are flushed.
    // Don't wait for stdin thread to complete because it will block until stdin is closed.
    let _stdout_res = stdout_thread.join();
    let _stderr_res = stderr_thread.join();

    while let Some(res) = join_set.join_next().await {
        res.wrap_err("task join failure").and_then(|r| r)?;
    }

    let code = match exit_code_rx.await {
        Ok(Exit::Code(code)) => u8::try_from(code).ok(),
        Ok(Exit::Signal(signal)) => u8::try_from(128 + signal).ok(),
        Ok(Exit::OtherError(_)) => None,
        Err(e) => {
            tracing::error!("failed to receive exit code: {e:?}");
            None
        }
    };
    Ok(ExitCode::from(code.unwrap_or(255)))
}

#[tracing::instrument(level = "debug", err, ret, skip_all)]
async fn receive_server(
    mut receiver: impl TryStream<Ok = ServerMessage, Error = io::Error> + Unpin,
    exit_code_tx: oneshot::Sender<Exit>,
    stdin_bytes_tx: Arc<Mutex<Option<mpsc::Sender<Arc<BytesMut>>>>>,
    stdin_tx: mpsc::Sender<Result<(), String>>,
    stdout_tx: mpsc::Sender<Arc<BytesMut>>,
    stderr_tx: mpsc::Sender<Arc<BytesMut>>,
) -> eyre::Result<()> {
    let mut exit_code_tx = Some(exit_code_tx);
    let mut stdout_tx = Some(stdout_tx);
    let mut stderr_tx = Some(stderr_tx);

    while let Some(msg) = receiver
        .try_next()
        .await
        .wrap_err("failed to receive message from server")?
    {
        tracing::trace!("received message: {:?}", msg);
        let res = match msg {
            ServerMessage::Exit(exit) => {
                // close stdin task output channel if not cloed.
                // this signals the sender thread to stdin thread exit
                let _ = stdin_bytes_tx.lock().unwrap().take();
                match &exit {
                    Exit::Code(code) => {
                        tracing::info!("command exited with code: {}", code);
                    }
                    Exit::Signal(signal) => {
                        tracing::info!("command exited with signal: {}", signal);
                    }
                    Exit::OtherError(ref error) => {
                        tracing::error!("command exited with error: {}", error);
                    }
                }
                exit_code_tx
                    .take()
                    .unwrap()
                    .send(exit)
                    .map_err(|_| eyre!("failed to send message to exit_tx"))
            }
            ServerMessage::Stdin(msg) => match msg {
                OutputResponse(msg) => stdin_tx
                    .send(msg)
                    .await
                    .wrap_err("failed to send message to stdin task"),
            },
            ServerMessage::Stdout(msg) => match msg {
                OutputRequest::Output(msg) => stdout_tx
                    .as_mut()
                    .unwrap()
                    .send(msg)
                    .await
                    .wrap_err("failed to send message to stdout task"),
                OutputRequest::Terminated => {
                    let _ = stdout_tx.take();
                    Ok(())
                }
            },
            ServerMessage::Stderr(msg) => match msg {
                OutputRequest::Output(msg) => stderr_tx
                    .as_mut()
                    .unwrap()
                    .send(msg)
                    .await
                    .wrap_err("failed to send message to sederr task"),
                OutputRequest::Terminated => {
                    let _ = stderr_tx.take();
                    Ok(())
                }
            },
        };

        if let Err(err) = res {
            // The only reason for failure in sending a message to another task is that the task has already been terminated.
            // In this case, the program has already started to abort, so it simply terminates without generating any errors in this task.
            tracing::debug!("{err}");
            break;
        }
    }

    Ok(())
}

#[tracing::instrument(level = "debug", err, ret, skip_all)]
async fn send_client(
    mut server_tx: impl Sink<ClientMessage, Error = io::Error> + Send + Unpin + 'static,
    stdin_bytes_rx: mpsc::Receiver<Arc<BytesMut>>,
    stdout_res_rx: mpsc::Receiver<Result<(), String>>,
    stderr_res_rx: mpsc::Receiver<Result<(), String>>,
) -> eyre::Result<()> {
    let stdin = ReceiverStream::new(stdin_bytes_rx)
        .map(OutputRequest::Output)
        .chain(stream::once(future::ready(OutputRequest::Terminated)))
        .map(ClientMessage::Stdin);
    let stdout = ReceiverStream::new(stdout_res_rx)
        .map(OutputResponse)
        .map(ClientMessage::Stdout);
    let stderr = ReceiverStream::new(stderr_res_rx)
        .map(OutputResponse)
        .map(ClientMessage::Stderr);

    let mut stream = stream::select(stdin, stream::select(stdout, stderr));
    while let Some(msg) = stream.next().await {
        tracing::trace!("sending message: {msg:?}");
        // If sending message to the client failed, there may have been a problem with the connection to the client.
        // In this case, all related server tasks should be aborted.
        // To abort all tasks, return an error here.
        server_tx
            .send(msg)
            .await
            .wrap_err("failed to send message to server")?;
        tracing::trace!("message sent to server");
    }

    Ok(())
}
