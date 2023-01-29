use std::{io, sync::Arc, thread};

use bytes::BytesMut;
use color_eyre::eyre::{self, WrapErr as _};
use futures::{future, stream, SinkExt as _, StreamExt as _, TryStreamExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};
use tracing::Instrument;

use crate::{
    args::ConnectAddress,
    protocol::{self, ClientMessage, OutputRequest, OutputResponse, ServerMessage, SpawnMessage},
    socket::SocketStream,
};

pub async fn main(
    connect_address: &ConnectAddress,
    command: String,
    args: Vec<String>,
) -> eyre::Result<()> {
    let stream = SocketStream::connect(connect_address)
        .await
        .wrap_err_with(|| format!("failed to connect socket: {connect_address}"))?;
    let (read_stream, write_stream) = stream.into_split();

    let read_stream = FramedRead::new(read_stream, LengthDelimitedCodec::new());
    let mut write_stream = FramedWrite::new(write_stream, LengthDelimitedCodec::new());

    protocol::new_sender(&mut write_stream)
        .send(SpawnMessage { command, args })
        .await
        .wrap_err("failed to send spawn request")?;

    let receiver = protocol::new_receiver::<_, ServerMessage>(read_stream);
    let mut sender = protocol::new_sender::<_, ClientMessage>(write_stream);

    let (stdin_bytes_tx, stdin_bytes_rx) = mpsc::channel(1);
    let (stdin_res_tx, stdin_res_rx) = mpsc::channel(1);
    let _stdin_thread = thread::Builder::new()
        .name("stdin".into())
        .spawn(|| {
            let _span = tracing::info_span!("stdin").entered();
            crate::thread::input(io::stdin(), stdin_bytes_tx, stdin_res_rx)
        })
        .wrap_err("failed to spawn thread")?;

    let (stdout_bytes_tx, stdout_bytes_rx) = mpsc::channel(1);
    let (stdout_res_tx, stdout_res_rx) = mpsc::channel(1);
    let stdout_thread = thread::Builder::new()
        .name("stdout".into())
        .spawn(|| {
            let _span = tracing::info_span!("stdout").entered();
            crate::thread::output(io::stdout(), stdout_res_tx, stdout_bytes_rx)
        })
        .wrap_err("failed to spawn thread")?;

    let (stderr_bytes_tx, stderr_bytes_rx) = mpsc::channel(1);
    let (stderr_res_tx, stderr_res_rx) = mpsc::channel(1);
    let stderr_thread = thread::Builder::new()
        .name("stderr".into())
        .spawn(|| {
            let _span = tracing::info_span!("stderr").entered();
            crate::thread::output(io::stderr(), stderr_res_tx, stderr_bytes_rx)
        })
        .wrap_err("failed to spawn thread")?;

    tokio::spawn(receive(
        receiver,
        stdin_res_tx,
        stdout_bytes_tx,
        stderr_bytes_tx,
    ));
    tokio::spawn(
        async move {
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
                match sender.send(msg).await {
                    Ok(()) => tracing::trace!("message sent"),
                    Err(e) => tracing::error!("failed to send message: {e:?}"),
                }
            }
        }
        .instrument(tracing::info_span!("send")),
    );

    //let _stdin_res = stdin_thread.join();
    let _stdout_res = stdout_thread.join();
    let _stderr_res = stderr_thread.join();

    Ok(())
}

#[tracing::instrument(level = "debug", err, ret, skip_all)]
async fn receive(
    mut receiver: impl TryStreamExt<Ok = ServerMessage, Error = io::Error> + Unpin,
    stdin_tx: mpsc::Sender<Result<(), String>>,
    stdout_tx: mpsc::Sender<Arc<BytesMut>>,
    stderr_tx: mpsc::Sender<Arc<BytesMut>>,
) -> eyre::Result<()> {
    let mut stdout_tx = Some(stdout_tx);
    let mut stderr_tx = Some(stderr_tx);
    while let Some(msg) = receiver
        .try_next()
        .await
        .wrap_err("failed to receive message")?
    {
        tracing::trace!("received message: {:?}", msg);
        match msg {
            ServerMessage::Exit(_code) => {
                // tracing::info!("server exited with code: {}", code);
                // return Ok(());
            }
            ServerMessage::Stdin(msg) => match msg {
                OutputResponse(msg) => {
                    stdin_tx
                        .send(msg)
                        .await
                        .wrap_err("failed to send message")?;
                }
            },
            ServerMessage::Stdout(msg) => match msg {
                OutputRequest::Output(msg) => {
                    stdout_tx
                        .as_mut()
                        .unwrap()
                        .send(msg)
                        .await
                        .wrap_err("failed to send message")?;
                }
                OutputRequest::Terminated => {
                    let _ = stdout_tx.take();
                }
            },
            ServerMessage::Stderr(msg) => match msg {
                OutputRequest::Output(msg) => {
                    stderr_tx
                        .as_mut()
                        .unwrap()
                        .send(msg)
                        .await
                        .wrap_err("failed to send message")?;
                }
                OutputRequest::Terminated => {
                    let _ = stderr_tx.take();
                }
            },
        }
    }

    Ok(())
}
