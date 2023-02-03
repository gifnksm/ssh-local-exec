use std::{io, mem, sync::Arc};

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
    mut input: impl AsyncRead + Unpin,
    tx: mpsc::Sender<Arc<BytesMut>>,
    mut rx: mpsc::Receiver<Result<(), String>>,
) -> eyre::Result<()> {
    let mut bytes = BytesMut::new();
    bytes.resize(BUFFER_SIZE, 0);

    let mut send_bytes = Arc::new(BytesMut::new());

    loop {
        match input.read(&mut bytes).await {
            Ok(0) => {
                tracing::trace!("terminated");
                break;
            }
            Ok(size) => {
                tracing::trace!("{} bytes read", size);

                let dummy_buf =
                    mem::replace(Arc::get_mut(&mut send_bytes).unwrap(), bytes.split_to(size));
                assert!(dummy_buf.is_empty());

                tx.send(Arc::clone(&send_bytes))
                    .await
                    .wrap_err("failed to send bytes")?;
                tracing::trace!("bytes sent");

                rx.recv()
                    .await
                    .transpose()
                    .map_err(|e| eyre!(e))?
                    .ok_or_else(|| eyre!("failed to receive buffer"))?;
                tracing::trace!("ack received");

                bytes.unsplit(mem::replace(
                    Arc::get_mut(&mut send_bytes).unwrap(),
                    BytesMut::new(),
                ));
                assert_eq!(bytes.len(), BUFFER_SIZE);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(eyre!(e).wrap_err("failed to read stdin")),
        }
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
        let res = output.write_all(&bytes).await;
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
        tx.send(res).await.wrap_err("failed to send result")?;
        tracing::trace!("result sent")
    }
    Ok(())
}
