use std::{io, mem, ptr, sync::Arc};

use bytes::BytesMut;
use color_eyre::eyre::{self, eyre, WrapErr as _};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    sync::mpsc,
};

const BUFFER_SIZE: usize = 4 * 1024;

#[tracing::instrument(level = "debug", err, ret, skip_all, fields(ty = ty))]
pub async fn read(
    ty: &'static str,
    input: Option<impl AsyncRead + Unpin>,
    tx: mpsc::Sender<Arc<BytesMut>>,
    mut rx: mpsc::Receiver<Result<(), String>>,
) -> eyre::Result<()> {
    let Some(mut input) = input else {
        return Ok(());
    };

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
    mut output: Option<impl AsyncWrite + Unpin>,
    tx: mpsc::Sender<Result<(), String>>,
    mut rx: mpsc::Receiver<Arc<BytesMut>>,
) -> eyre::Result<()> {
    while let Some(bytes) = rx.recv().await {
        tracing::trace!("{} bytes received", bytes.len());

        let res = match &mut output {
            Some(output) => loop {
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
            },
            None => {
                tracing::trace!("bytes discarded");
                Ok(())
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
