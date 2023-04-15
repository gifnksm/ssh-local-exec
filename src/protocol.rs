use std::{io, marker::PhantomData, sync::Arc};

use bytes::{Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use tokio_util::codec::{Decoder, Encoder};

pub type DecodeError = ciborium::de::Error<io::Error>;

#[derive(Debug)]
pub struct CborDecoder<Item, D> {
    inner: D,
    _item: PhantomData<Item>,
}

impl<Item, D> CborDecoder<Item, D> {
    pub fn new(inner: D) -> Self {
        Self {
            inner,
            _item: PhantomData,
        }
    }
}

impl<Item, D> Decoder for CborDecoder<Item, D>
where
    D: Decoder<Item = BytesMut, Error = io::Error>,
    Item: for<'de> Deserialize<'de>,
{
    type Item = Item;
    type Error = DecodeError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let bytes = self.inner.decode(src)?;
        let bytes = match bytes {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        let item = ciborium::de::from_reader(&*bytes)?;
        Ok(Some(item))
    }
}

pub type EncodeError = ciborium::ser::Error<io::Error>;

#[derive(Debug)]
pub struct CborEncoder<E> {
    inner: E,
}

impl<E> CborEncoder<E> {
    pub fn new(inner: E) -> Self {
        Self { inner }
    }
}

impl<Item, E> Encoder<Item> for CborEncoder<E>
where
    E: Encoder<Bytes, Error = io::Error>,
    Item: Serialize,
{
    type Error = EncodeError;

    fn encode(&mut self, item: Item, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let mut bytes = Vec::new();
        ciborium::ser::into_writer(&item, &mut bytes)?;
        self.inner.encode(Bytes::from(bytes), dst)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnResponse(pub Result<(), String>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Signal {
    Alarm,
    Child,
    Hangup,
    Interrupt,
    Io,
    Pipe,
    Quit,
    Terminate,
    UserDefined1,
    UserDefined2,
    WindowChange,
    Kill,
}

impl From<Signal> for Option<nix::sys::signal::Signal> {
    fn from(signal: Signal) -> Self {
        use nix::sys::signal::Signal as NixSignal;
        let signal = match signal {
            Signal::Alarm => NixSignal::SIGALRM,
            Signal::Child => NixSignal::SIGCHLD,
            Signal::Hangup => NixSignal::SIGHUP,
            Signal::Interrupt => NixSignal::SIGINT,
            Signal::Io => NixSignal::SIGINT,
            Signal::Pipe => NixSignal::SIGPIPE,
            Signal::Quit => NixSignal::SIGQUIT,
            Signal::Terminate => NixSignal::SIGTERM,
            Signal::UserDefined1 => NixSignal::SIGUSR1,
            Signal::UserDefined2 => NixSignal::SIGUSR2,
            Signal::WindowChange => NixSignal::SIGWINCH,
            Signal::Kill => NixSignal::SIGKILL,
        };
        Some(signal)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    OtherError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub enum OutputResponse {
    Output(Result<(), String>),
    Terminated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutputRequest {
    Output(Arc<BytesMut>),
    Terminated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    SpawnResponse(SpawnResponse),
    Exit(Exit),
    Stdin(OutputResponse),
    Stdout(OutputRequest),
    Stderr(OutputRequest),
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    SpawnRequest(SpawnRequest),
    Signal(Signal),
    Stdin(OutputRequest),
    Stdout(OutputResponse),
    Stderr(OutputResponse),
    Shutdown,
}
