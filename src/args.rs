use std::{fmt, io};

use async_trait::async_trait;

use crate::socket::{SocketAddr, ToSocketAddrs};

#[derive(Debug, clap::Args)]
pub struct LocalEndpoint {
    /// Server endpoint address on the local host
    ///
    /// Possible values:
    /// Internet (`<address>:<port>` / `inet:<address>:<port>`) or
    /// UNIX domain socket (`<path>` / `unix:<path>`)
    #[clap(
        short = 'l',
        long = "local-endpoint",
        value_name = "SOCKET_ADDRESS",
        env = "SSH_LOCAL_EXEC_LOCAL_ENDPOINT"
    )]
    value: String,
}

#[async_trait]
impl ToSocketAddrs for LocalEndpoint {
    async fn to_socket_addrs(&self) -> io::Result<Box<dyn Iterator<Item = SocketAddr> + '_>> {
        self.as_str().to_socket_addrs().await
    }
}

impl LocalEndpoint {
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for LocalEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

#[derive(Debug, clap::Args)]
pub struct RemoteEndpoint {
    /// Server endpoint address on the remote host
    ///
    /// Possible values:
    /// Internet (`<address>:<port>` / `inet:<address>:<port>`) or
    /// UNIX domain socket (`<path>` / `unix:<path>`)
    #[clap(
        short = 'r',
        long = "remote-endpoint",
        value_name = "SOCKET_ADDRESS",
        env = "SSH_LOCAL_EXEC_REMOTE_ENDPOINT"
    )]
    value: String,
}

#[async_trait]
impl ToSocketAddrs for RemoteEndpoint {
    async fn to_socket_addrs(&self) -> io::Result<Box<dyn Iterator<Item = SocketAddr> + '_>> {
        self.as_str().to_socket_addrs().await
    }
}

impl RemoteEndpoint {
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for RemoteEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}
