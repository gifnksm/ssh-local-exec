use std::{
    fmt::{self, Display},
    io, iter,
    os::unix::prelude::{AsRawFd, RawFd},
    pin::Pin,
    task,
};

use async_trait::async_trait;
use derive_more::From;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{self, tcp, unix, TcpListener, TcpStream, UnixListener, UnixStream},
};

#[async_trait]
pub trait ToSocketAddrs {
    async fn to_socket_addrs(&self) -> io::Result<Box<dyn Iterator<Item = SocketAddr> + '_>>;
}

#[async_trait]
impl<T> ToSocketAddrs for &T
where
    T: ToSocketAddrs + ?Sized + Sync + Send,
{
    async fn to_socket_addrs(&self) -> io::Result<Box<dyn Iterator<Item = SocketAddr> + '_>> {
        (**self).to_socket_addrs().await
    }
}

#[async_trait]
impl ToSocketAddrs for str {
    async fn to_socket_addrs(&self) -> io::Result<Box<dyn Iterator<Item = SocketAddr> + '_>> {
        enum Addr<'a> {
            Inet(&'a str),
            Unix(&'a str),
        }

        // TODO: support @name syntax (abstract socket)
        // blocked by `feature(unix_socket_abstract)` https://github.com/rust-lang/rust/issues/85410

        let addr = if let Some(inet_addr) = self.strip_prefix("inet:") {
            Addr::Inet(inet_addr)
        } else if let Some(unix_addr) = self
            .strip_prefix("unix:")
            .or_else(|| self.contains('/').then_some(self))
        {
            Addr::Unix(unix_addr)
        } else {
            Addr::Inet(self)
        };

        match addr {
            Addr::Inet(addr) => {
                let addrs = net::lookup_host(addr).await?;
                Ok(Box::new(addrs.map(Into::into)))
            }
            Addr::Unix(addr) => {
                let addr = std::os::unix::net::SocketAddr::from_pathname(addr)?;
                Ok(Box::new(iter::once(addr.into())))
            }
        }
    }
}

#[async_trait]
impl ToSocketAddrs for String {
    async fn to_socket_addrs(&self) -> io::Result<Box<dyn Iterator<Item = SocketAddr> + '_>> {
        self.as_str().to_socket_addrs().await
    }
}

#[derive(Debug, From)]
pub enum SocketAddr {
    UnixStd(std::os::unix::net::SocketAddr),
    UnixTokio(net::unix::SocketAddr),
    Inet(std::net::SocketAddr),
}

impl Display for SocketAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnixStd(addr) => match addr.as_pathname() {
                Some(path) => write!(f, "unix:{}", path.display()),
                None if addr.is_unnamed() => write!(f, "unix:<unnamed>"),
                // TODO: support abstract socket
                // blocked by `feature(unix_socket_abstract)` https://github.com/rust-lang/rust/issues/85410
                None => unimplemented!("abstract socket not supported"),
            },
            Self::UnixTokio(addr) => match addr.as_pathname() {
                Some(path) => write!(f, "unix:{}", path.display()),
                None if addr.is_unnamed() => write!(f, "unix:<unnamed>"),
                // TODO: support abstract socket
                // blocked by `feature(unix_socket_abstract)` https://github.com/rust-lang/rust/issues/85410
                None => unimplemented!("abstract socket not supported"),
            },
            Self::Inet(addr) => write!(f, "{addr}"),
        }
    }
}

#[derive(Debug, From)]
pub enum SocketListener {
    Unix(UnixListener),
    Tcp(TcpListener),
}

impl AsRawFd for SocketListener {
    fn as_raw_fd(&self) -> RawFd {
        match self {
            Self::Unix(listener) => listener.as_raw_fd(),
            Self::Tcp(listener) => listener.as_raw_fd(),
        }
    }
}

impl SocketListener {
    pub async fn bind(addrs: impl ToSocketAddrs) -> io::Result<Self> {
        let mut last_err = None;
        for addr in addrs.to_socket_addrs().await? {
            let res = match addr {
                SocketAddr::UnixStd(addr) => match addr.as_pathname() {
                    Some(path) => UnixListener::bind(path).map(Into::into),
                    None if addr.is_unnamed() => panic!("cannot bind to unnamed socket"),
                    // TODO: support abstract socket
                    // blocked by https://github.com/tokio-rs/tokio/issues/4610
                    None => panic!("abstract socket not supported"),
                },
                SocketAddr::UnixTokio(addr) => match addr.as_pathname() {
                    Some(path) => UnixListener::bind(path).map(Into::into),
                    None if addr.is_unnamed() => panic!("cannot bind to unnamed socket"),
                    // TODO: support abstract socket
                    // blocked by https://github.com/tokio-rs/tokio/issues/4610
                    None => panic!("abstract socket not supported"),
                },
                SocketAddr::Inet(addr) => TcpListener::bind(addr).await.map(Into::into),
            };
            match res {
                Ok(listener) => return Ok(listener),
                Err(e) => last_err = Some(e),
            }
        }

        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve to any address",
            )
        }))
    }

    pub async fn accept(&self) -> io::Result<(SocketStream, SocketAddr)> {
        match self {
            Self::Unix(listener) => listener
                .accept()
                .await
                .map(|(stream, addr)| (stream.into(), addr.into())),
            Self::Tcp(listener) => listener
                .accept()
                .await
                .map(|(stream, addr)| (stream.into(), addr.into())),
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        match self {
            Self::Unix(listener) => listener.local_addr().map(Into::into),
            Self::Tcp(listener) => listener.local_addr().map(Into::into),
        }
    }

    pub fn as_tcp(&self) -> Option<&TcpListener> {
        if let Self::Tcp(listener) = self {
            Some(listener)
        } else {
            None
        }
    }

    pub fn as_unix(&self) -> Option<&UnixListener> {
        if let Self::Unix(listener) = self {
            Some(listener)
        } else {
            None
        }
    }

    pub fn as_tcp_mut(&mut self) -> Option<&mut TcpListener> {
        if let Self::Tcp(listener) = self {
            Some(listener)
        } else {
            None
        }
    }

    pub fn as_unix_mut(&mut self) -> Option<&mut UnixListener> {
        if let Self::Unix(listener) = self {
            Some(listener)
        } else {
            None
        }
    }
}

#[derive(Debug, From)]
pub enum SocketStream {
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl AsRawFd for SocketStream {
    fn as_raw_fd(&self) -> RawFd {
        match self {
            Self::Unix(stream) => stream.as_raw_fd(),
            Self::Tcp(stream) => stream.as_raw_fd(),
        }
    }
}

impl AsyncRead for SocketStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> task::Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Unix(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for SocketStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &[u8],
    ) -> task::Poll<Result<usize, io::Error>> {
        match self.get_mut() {
            Self::Unix(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> task::Poll<Result<(), io::Error>> {
        match self.get_mut() {
            Self::Unix(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> task::Poll<Result<(), io::Error>> {
        match self.get_mut() {
            Self::Unix(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

impl SocketStream {
    pub async fn connect(addrs: impl ToSocketAddrs) -> io::Result<Self> {
        let mut last_err = None;
        for addr in addrs.to_socket_addrs().await? {
            let res = match addr {
                SocketAddr::UnixStd(addr) => {
                    // TODO: support abstract socket
                    // blocked by https://github.com/tokio-rs/tokio/issues/4610
                    let path = addr.as_pathname().expect("abstract socket not supported");
                    UnixStream::connect(path).await.map(Into::into)
                }
                SocketAddr::UnixTokio(addr) => {
                    // TODO: support abstract socket
                    // blocked by https://github.com/tokio-rs/tokio/issues/4610
                    let path = addr.as_pathname().expect("abstract socket not supported");
                    UnixStream::connect(path).await.map(Into::into)
                }
                SocketAddr::Inet(addr) => TcpStream::connect(addr).await.map(Into::into),
            };
            match res {
                Ok(listener) => return Ok(listener),
                Err(e) => last_err = Some(e),
            }
        }

        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve to any address",
            )
        }))
    }

    pub fn into_split(self) -> (OwnedReadHalf, OwnedWriteHalf) {
        match self {
            Self::Unix(stream) => {
                let (read, write) = stream.into_split();
                (read.into(), write.into())
            }
            Self::Tcp(stream) => {
                let (read, write) = stream.into_split();
                (read.into(), write.into())
            }
        }
    }

    pub fn as_tcp(&self) -> Option<&TcpStream> {
        if let Self::Tcp(stream) = self {
            Some(stream)
        } else {
            None
        }
    }

    pub fn as_unix(&self) -> Option<&UnixStream> {
        if let Self::Unix(stream) = self {
            Some(stream)
        } else {
            None
        }
    }

    pub fn as_tcp_mut(&mut self) -> Option<&mut TcpStream> {
        if let Self::Tcp(stream) = self {
            Some(stream)
        } else {
            None
        }
    }

    pub fn as_unix_mut(&mut self) -> Option<&mut UnixStream> {
        if let Self::Unix(stream) = self {
            Some(stream)
        } else {
            None
        }
    }
}

#[derive(Debug, From)]
pub enum OwnedReadHalf {
    Tcp(tcp::OwnedReadHalf),
    Unix(unix::OwnedReadHalf),
}

impl AsyncRead for OwnedReadHalf {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> task::Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Unix(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

#[derive(Debug, From)]
pub enum OwnedWriteHalf {
    Tcp(tcp::OwnedWriteHalf),
    Unix(unix::OwnedWriteHalf),
}

impl AsyncWrite for OwnedWriteHalf {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &[u8],
    ) -> task::Poll<Result<usize, io::Error>> {
        match self.get_mut() {
            Self::Unix(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> task::Poll<Result<(), io::Error>> {
        match self.get_mut() {
            Self::Unix(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> task::Poll<Result<(), io::Error>> {
        match self.get_mut() {
            Self::Unix(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}
