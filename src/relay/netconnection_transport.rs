// Contains the stream abstraction to be used for different networking transports
// Implements AsyncRead + AsyncWrite to act as Stream which are forwarded
// to inner stream, this makes it possible to use with Framed and such
use std::fmt;
use std::net::SocketAddr;
use pin_project_lite::pin_project;
use tokio::net::tcp;
use crate::relay::netconnection_codec::{NetConnectionCodecDecoder, NetConnectionCodecEncoder};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_util::codec::{FramedRead, FramedWrite};
use crate::{implement_async_read_proxy,implement_async_write_proxy};

pub type NetConnectionFramedRead = FramedRead<NetTransportRead, NetConnectionCodecDecoder>;
pub type NetConnectionFramedWrite = FramedWrite<NetTransportWrite, NetConnectionCodecEncoder>;

pin_project! {
    #[project = NetTransportWriteProj]
    
    #[derive(Debug)]
    pub enum NetTransportWrite {
        TCP {
           addr: SocketAddr,
           #[pin] write: tcp::OwnedWriteHalf,
        },
    }
}

pin_project! {
    #[project = NetTransportReadProj]
    
    #[derive(Debug)]
    pub enum NetTransportRead {
        TCP {
           addr: SocketAddr,
           #[pin] read: tcp::OwnedReadHalf,
        },
    }
}

implement_async_write_proxy!(
    NetTransportWrite,
    NetTransportWriteProj,
    match { 
        TCP { write, .. } => write
    }
);

implement_async_read_proxy!(
    NetTransportRead,
    NetTransportReadProj,
    match { 
        TCP { read, .. } => read
    }
);

impl NetTransportWrite {
    pub fn into_framed(self) -> NetConnectionFramedWrite {
        NetConnectionCodecEncoder::new_framed(self)
    }
}

impl NetTransportRead {
    pub fn into_framed(self) -> NetConnectionFramedRead {
        NetConnectionCodecDecoder::new_framed(self)
    }
}

impl fmt::Display for NetTransportWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self {
            Self::TCP { addr, .. } => write!(f, "TCP({})", addr),
        }
    }
}

impl fmt::Display for NetTransportRead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self {
            Self::TCP { addr, .. } => write!(f, "TCP({})", addr),
        }
    }
}
