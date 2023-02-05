use std::{fmt, io};

use async_trait::async_trait;

use crate::socket::{SocketAddr, ToSocketAddrs};

#[derive(Debug, clap::Args)]
pub struct ListenAddresses {
    /// Server endpoint address to listen
    ///
    /// Possible values:
    /// Internet (`<address>:<port>` / `inet:<address>:<port>`) or
    /// UNIX domain socket (`<path>` / `unix:<path>`)
    #[clap(
        long = "listen",
        value_name = "SOCKET_ADDRESS",
        env = "SSH_LOCAL_EXEC_LISTEN_ADDRESS",
        value_delimiter = ','
    )]
    values: Vec<ListenAddress>,
}

impl ListenAddresses {
    pub fn values(&self) -> &[ListenAddress] {
        &self.values
    }
}

#[derive(Debug, Clone)]
pub struct ListenAddress {
    value: String,
}

impl From<String> for ListenAddress {
    fn from(value: String) -> Self {
        Self { value }
    }
}

#[async_trait]
impl ToSocketAddrs for ListenAddress {
    async fn to_socket_addrs(&self) -> io::Result<Box<dyn Iterator<Item = SocketAddr> + '_>> {
        self.as_str().to_socket_addrs().await
    }
}

impl ListenAddress {
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for ListenAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

#[derive(Debug, clap::Args)]
pub struct ConnectAddress {
    /// Server endpoint address to connect
    ///
    /// Possible values:
    /// Internet (`<address>:<port>` / `inet:<address>:<port>`) or
    /// UNIX domain socket (`<path>` / `unix:<path>`)
    #[clap(
        long = "connect",
        value_name = "SOCKET_ADDRESS",
        env = "SSH_LOCAL_EXEC_CONNECT_ADDRESS"
    )]
    value: String,
}

#[async_trait]
impl ToSocketAddrs for ConnectAddress {
    async fn to_socket_addrs(&self) -> io::Result<Box<dyn Iterator<Item = SocketAddr> + '_>> {
        self.as_str().to_socket_addrs().await
    }
}

impl ConnectAddress {
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for ConnectAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}
