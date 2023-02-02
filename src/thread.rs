use std::{
    io::{self, Read, Write},
    sync::Arc,
};

use bytes::BytesMut;
use color_eyre::eyre::{self, eyre, WrapErr as _};
use tokio::sync::mpsc;

const BUFFER_SIZE: usize = 4 * 1024;

#[tracing::instrument(level = "debug", err, ret, skip_all, fields(ty = ty))]
pub fn read(
    ty: &'static str,
    mut input: impl Read,
    tx: mpsc::Sender<Arc<BytesMut>>,
    mut rx: mpsc::Receiver<Result<(), String>>,
) -> eyre::Result<()> {
    let mut bytes = BytesMut::new();
    bytes.resize(BUFFER_SIZE, 0);
    loop {
        match input.read(&mut bytes) {
            Ok(0) => {
                tracing::trace!("terminated");
                break;
            }
            Ok(size) => {
                tracing::trace!("{} bytes read", size);
                let send_bytes = Arc::new(bytes.split_to(size));
                tx.blocking_send(Arc::clone(&send_bytes))
                    .wrap_err("failed to send bytes")?;
                tracing::trace!("bytes sent");
                rx.blocking_recv()
                    .transpose()
                    .map_err(|e| eyre!(e))?
                    .ok_or_else(|| eyre!("failed to receive buffer"))?;
                tracing::trace!("ack received");

                let send_bytes = Arc::try_unwrap(send_bytes).expect("must be un-shared");
                bytes.unsplit(send_bytes);
                assert_eq!(bytes.len(), BUFFER_SIZE);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(eyre!(e).wrap_err("failed to read stdin")),
        }
    }
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
        let res = output.write_all(&bytes);
        let res = match &res {
            Ok(()) => {
                tracing::trace!("bytes written");
                Ok(())
            }
            Err(e) => {
                tracing::error!("failed to write bytes: {}", e);
                Err(e.to_string())
            }
        };
        tx.blocking_send(res).wrap_err("failed to send result")?;
        tracing::trace!("result sent")
    }
    Ok(())
}
