use std::{
    io::{self, Read, Write},
    mem,
    sync::{Arc, Mutex},
};

use bytes::BytesMut;
use color_eyre::eyre::{self, eyre, WrapErr as _};
use tokio::sync::mpsc;

const BUFFER_SIZE: usize = 4 * 1024;

#[tracing::instrument(level = "debug", err, ret, skip_all, fields(ty = ty))]
pub fn read(
    ty: &'static str,
    mut input: impl Read,
    tx: Arc<Mutex<Option<mpsc::Sender<Arc<BytesMut>>>>>,
    mut rx: mpsc::Receiver<Result<(), String>>,
) -> eyre::Result<()> {
    let mut bytes = BytesMut::new();
    bytes.resize(BUFFER_SIZE, 0);

    let mut send_bytes = Arc::new(BytesMut::new());

    loop {
        let read_size = match input.read(&mut bytes) {
            Ok(0) => {
                tracing::trace!("terminated");
                break;
            }
            Ok(size) => size,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            // If reading from input failed, some unexpected system error may have occurred.
            // In this case, all related server tasks should be aborted.
            // To abort all tasks, return an error here.
            Err(e) => return Err(eyre!(e)).wrap_err("failed to read stdin"),
        };

        tracing::trace!("{} bytes read", read_size);

        let dummy_buf = mem::replace(
            Arc::get_mut(&mut send_bytes).unwrap(),
            bytes.split_to(read_size),
        );
        assert!(dummy_buf.is_empty());

        {
            match &*tx.lock().unwrap() {
                Some(tx) => {
                    // The only reason for failure in sending a message to another task is that the task has already been terminated.
                    // In this case, the program has already started to abort, so it simply terminates without generating any errors in this task.
                    if let Err(err) = tx
                        .blocking_send(Arc::clone(&send_bytes))
                        .wrap_err("failed to send bytes")
                    {
                        tracing::debug!("{err}");
                        break;
                    }
                }
                None => break,
            }
        }

        match rx.blocking_recv() {
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

        bytes.unsplit(mem::replace(
            Arc::get_mut(&mut send_bytes).unwrap(),
            BytesMut::new(),
        ));
        assert_eq!(bytes.len(), BUFFER_SIZE);
    }

    let _ = tx.lock().unwrap().take();

    Ok(())
}

#[tracing::instrument(level = "debug", err, ret, skip_all, fields(ty = ty))]
pub fn write(
    ty: &'static str,
    mut output: impl Write,
    tx: mpsc::Sender<Result<(), String>>,
    mut rx: mpsc::Receiver<Arc<BytesMut>>,
) -> eyre::Result<()> {
    while let Some(bytes) = rx.blocking_recv() {
        tracing::trace!("{} bytes received", bytes.len());

        let res = loop {
            match output.write_all(&bytes) {
                Ok(()) => {
                    tracing::trace!("bytes written");
                    break Ok(());
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                // In this case, send an error response to the input end.
                // Input end will trigger the abortion of all related server tasks.
                Err(e) => {
                    tracing::error!("failed to write bytes: {}", e);
                    break Err(e.to_string());
                }
            }
        };

        if let Err(err) = tx.blocking_send(res) {
            // The only reason for failure in sending a message to another task is that the task has already been terminated.
            // In this case, the program has already started to abort, so it simply terminates without generating any errors in this task.
            tracing::debug!("{err}");
            break;
        }
        tracing::trace!("result sent")
    }

    Ok(())
}
