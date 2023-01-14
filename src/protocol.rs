use std::sync::Arc;

use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use tokio_serde::{formats::MessagePack, Framed};

pub type Sender<Transport, SinkItem> = Framed<Transport, (), SinkItem, MessagePack<(), SinkItem>>;
pub fn new_sender<Transport, SinkItem>(stream: Transport) -> Sender<Transport, SinkItem> {
    Sender::new(stream, MessagePack::default())
}

pub type Receiver<Transport, Item> = Framed<Transport, Item, (), MessagePack<Item, ()>>;
pub fn new_receiver<Transport, Item>(stream: Transport) -> Receiver<Transport, Item> {
    Receiver::new(stream, MessagePack::default())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnMessage {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    OtherError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct OutputResponse(pub Result<(), String>);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutputRequest {
    Output(Arc<BytesMut>),
    Terminated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    Exit(Exit),
    Stdin(OutputResponse),
    Stdout(OutputRequest),
    Stderr(OutputRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    Stdin(OutputRequest),
    Stdout(OutputResponse),
    Stderr(OutputResponse),
}
