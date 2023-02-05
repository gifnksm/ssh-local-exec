use std::{
    io,
    process::ExitCode,
    sync::{Arc, Mutex},
    thread,
};

use bytes::BytesMut;
use color_eyre::eyre::{self, bail, eyre, WrapErr as _};
use futures::{future, stream, Sink, SinkExt as _, StreamExt as _, TryStream, TryStreamExt as _};
use tokio::{
    signal::unix::{signal, SignalKind},
    sync::{mpsc, oneshot, watch},
    task::JoinSet,
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};
use tracing::Instrument;

use crate::{
    args::ConnectAddress,
    protocol::{
        self, ClientMessage, Exit, OutputRequest, OutputResponse, ServerMessage, Signal,
        SpawnRequest, SpawnResponse,
    },
    socket::SocketStream,
};

pub async fn main(
    connect_address: &ConnectAddress,
    command: String,
    args: Vec<String>,
) -> ExitCode {
    let stream = match SocketStream::connect(connect_address)
        .await
        .wrap_err_with(|| format!("failed to connect socket: {connect_address}"))
    {
        Ok(stream) => stream,
        Err(err) => {
            tracing::error!("{err:?}");
            return ExitCode::from(255);
        }
    };

    let (read_stream, write_stream) = stream.into_split();

    let bytes_rx = FramedRead::new(read_stream, LengthDelimitedCodec::new());
    let bytes_tx = FramedWrite::new(write_stream, LengthDelimitedCodec::new());

    let mut server_tx = protocol::new_sender::<_, ClientMessage>(bytes_tx);
    let mut server_rx = protocol::new_receiver::<_, ServerMessage>(bytes_rx);

    let spawn_req = SpawnRequest { command, args };

    if let Err(err) = spawn_request(spawn_req, &mut server_tx, &mut server_rx).await {
        tracing::error!("{err:?}");
        return ExitCode::from(255);
    }

    let code = match communicate(server_tx, server_rx).await {
        Ok(Exit::Code(code)) => {
            tracing::debug!("command exited with code: {}", code);
            u8::try_from(code).ok()
        }
        Ok(Exit::Signal(signal)) => {
            tracing::debug!("command exited with signal: {}", signal);
            u8::try_from(128 + signal).ok()
        }
        Ok(Exit::OtherError(error)) => {
            tracing::error!("command exited with error: {}", error);
            None
        }
        Err(err) => {
            tracing::error!("{err:?}");
            None
        }
    };

    ExitCode::from(code.unwrap_or(255))
}

async fn spawn_request(
    spawn_req: SpawnRequest,
    mut server_tx: impl Sink<ClientMessage, Error = io::Error> + Send + Unpin,
    mut server_rx: impl TryStream<Ok = ServerMessage, Error = io::Error> + Send + Unpin,
) -> eyre::Result<()> {
    let spawn_req = ClientMessage::SpawnRequest(spawn_req);

    server_tx
        .send(spawn_req)
        .await
        .wrap_err("failed to send spawn request")?;

    let resp = server_rx
        .try_next()
        .await
        .wrap_err("failed to receive message from server")?;

    match resp {
        Some(ServerMessage::SpawnResponse(SpawnResponse(Ok(())))) => {}
        Some(ServerMessage::SpawnResponse(SpawnResponse(Err(msg)))) => {
            tracing::error!("{msg}");
            bail!("failed to spawn process");
        }
        Some(msg) => {
            bail!("unexpected message from server: {msg:?}");
        }
        None => {
            bail!("server disconnected");
        }
    }

    Ok(())
}

async fn communicate(
    server_tx: impl Sink<ClientMessage, Error = io::Error> + Send + Unpin + 'static,
    server_rx: impl TryStream<Ok = ServerMessage, Error = io::Error> + Send + Unpin + 'static,
) -> eyre::Result<Exit> {
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

    let (exit_tx, exit_rx) = watch::channel(());

    let (signal_tx, signal_rx) = mpsc::channel(1);
    join_set.spawn(handle_signal(exit_rx, signal_tx).in_current_span());

    let (exit_code_tx, exit_code_rx) = oneshot::channel();
    join_set.spawn(receive_server(
        server_rx,
        exit_code_tx,
        exit_tx,
        stdin_bytes_tx,
        stdin_res_tx,
        stdout_bytes_tx,
        stderr_bytes_tx,
    ));
    join_set.spawn(
        send_client(
            server_tx,
            signal_rx,
            stdin_bytes_rx,
            stdout_res_rx,
            stderr_res_rx,
        )
        .in_current_span(),
    );

    // Wait for output threads to complete to ensure that all outputs are flushed.
    // Don't wait for stdin thread to complete because it will block until stdin is closed.
    let _stdout_res = stdout_thread.join();
    let _stderr_res = stderr_thread.join();

    while let Some(res) = join_set.join_next().await {
        res.wrap_err("task join failure").and_then(|r| r)?;
    }

    let exit = exit_code_rx.await.wrap_err("failed to receive exit code")?;
    Ok(exit)
}

#[tracing::instrument(level = "debug", err, ret, skip_all)]
async fn handle_signal(
    mut exit: watch::Receiver<()>,
    tx: mpsc::Sender<Signal>,
) -> eyre::Result<()> {
    let mut alarm = signal(SignalKind::alarm())?;
    let mut child = signal(SignalKind::child())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut io = signal(SignalKind::io())?;
    let mut pipe = signal(SignalKind::pipe())?;
    let mut quit = signal(SignalKind::quit())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut user_defined1 = signal(SignalKind::user_defined1())?;
    let mut user_defined2 = signal(SignalKind::user_defined2())?;
    let mut window_change = signal(SignalKind::window_change())?;

    loop {
        let msg = tokio::select! {
            _ = exit.changed() => break,
            _ = alarm.recv() => Signal::Alarm,
            _ = child.recv() => Signal::Child,
            _ = hangup.recv() => Signal::Hangup,
            _ = interrupt.recv() => Signal::Interrupt,
            _ = io.recv() => Signal::Io,
            _ = pipe.recv() => Signal::Pipe,
            _ = quit.recv() => Signal::Quit,
            _ = terminate.recv() => Signal::Terminate,
            _ = user_defined1.recv() => Signal::UserDefined1,
            _ = user_defined2.recv() => Signal::UserDefined2,
            _ = window_change.recv() => Signal::WindowChange,
        };

        // The only reason for failure in sending a message to another task is that the task has already been terminated.
        // In this case, the program has already started to abort, so it simply terminates without generating any errors in this task.
        if let Err(err) = tx.send(msg).await.wrap_err("failed to send signal") {
            tracing::debug!("{err}");
            break;
        }
    }

    Ok(())
}

#[tracing::instrument(level = "debug", err, ret, skip_all)]
async fn receive_server(
    mut receiver: impl TryStream<Ok = ServerMessage, Error = io::Error> + Unpin,
    exit_code_tx: oneshot::Sender<Exit>,
    exit_tx: watch::Sender<()>,
    stdin_bytes_tx: Arc<Mutex<Option<mpsc::Sender<Arc<BytesMut>>>>>,
    stdin_tx: mpsc::Sender<Result<(), String>>,
    stdout_tx: mpsc::Sender<Arc<BytesMut>>,
    stderr_tx: mpsc::Sender<Arc<BytesMut>>,
) -> eyre::Result<()> {
    let mut shutdown = false;
    let mut exit_code_tx = Some(exit_code_tx);
    let mut stdin_tx = Some(stdin_tx);
    let mut stdout_tx = Some(stdout_tx);
    let mut stderr_tx = Some(stderr_tx);

    while let Some(msg) = receiver
        .try_next()
        .await
        .wrap_err("failed to receive message from server")?
    {
        tracing::trace!("received message: {:?}", msg);
        let res = match msg {
            ServerMessage::SpawnResponse(msg) => {
                tracing::debug!("invalid mesage received: {msg:?}");
                Err(eyre!("invalid message received: {msg:?}"))
            }
            ServerMessage::Exit(exit) => {
                // close stdin task output channel if not cloed.
                // this signals the sender thread to stdin thread exit
                let _ = stdin_bytes_tx.lock().unwrap().take();
                exit_tx.send_modify(|_| ());
                exit_code_tx
                    .take()
                    .unwrap()
                    .send(exit)
                    .map_err(|_| eyre!("failed to send message to exit_tx"))
            }
            ServerMessage::Stdin(msg) => match msg {
                OutputResponse::Output(msg) => stdin_tx
                    .as_mut()
                    .unwrap()
                    .send(msg)
                    .await
                    .wrap_err("failed to send message to stdin task"),
                OutputResponse::Terminated => {
                    tracing::debug!("server and client stdin tasks terminated");
                    let _ = stdin_tx.take();
                    Ok(())
                }
            },
            ServerMessage::Stdout(msg) => match msg {
                OutputRequest::Output(msg) => stdout_tx
                    .as_mut()
                    .unwrap()
                    .send(msg)
                    .await
                    .wrap_err("failed to send message to stdout task"),
                OutputRequest::Terminated => {
                    tracing::debug!("server stdout task terminated");
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
                    tracing::debug!("server stderr task terminated");
                    let _ = stderr_tx.take();
                    Ok(())
                }
            },
            ServerMessage::Shutdown => {
                tracing::debug!("server send task shutdown");
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

    if !shutdown {
        bail!("server terminated unexpectedly");
    }

    Ok(())
}

#[tracing::instrument(level = "debug", err, ret, skip_all)]
async fn send_client(
    mut server_tx: impl Sink<ClientMessage, Error = io::Error> + Send + Unpin + 'static,
    signal_rx: mpsc::Receiver<Signal>,
    stdin_bytes_rx: mpsc::Receiver<Arc<BytesMut>>,
    stdout_res_rx: mpsc::Receiver<Result<(), String>>,
    stderr_res_rx: mpsc::Receiver<Result<(), String>>,
) -> eyre::Result<()> {
    let signal = ReceiverStream::new(signal_rx).map(ClientMessage::Signal);
    let stdin = ReceiverStream::new(stdin_bytes_rx)
        .map(OutputRequest::Output)
        .chain(stream::once(future::ready(OutputRequest::Terminated)))
        .map(ClientMessage::Stdin);
    let stdout = ReceiverStream::new(stdout_res_rx)
        .map(OutputResponse::Output)
        .chain(stream::once(future::ready(OutputResponse::Terminated)))
        .map(ClientMessage::Stdout);
    let stderr = ReceiverStream::new(stderr_res_rx)
        .map(OutputResponse::Output)
        .chain(stream::once(future::ready(OutputResponse::Terminated)))
        .map(ClientMessage::Stderr);

    let mut stream = stream::select(
        signal,
        stream::select(stdin, stream::select(stdout, stderr)),
    )
    .chain(stream::once(future::ready(ClientMessage::Shutdown)));
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
