use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use quinn::Connection;
use std::io::{Error, ErrorKind};
use std::net::SocketAddr;

/// Transport abstraction for Sankaku/RT media datagrams + reliable control signaling.
///
/// `send_datagram`/`recv_datagram` carry MTU-limited media packets.
/// `send_control`/`recv_control` carry reliable control packets.
/// Compatibility methods remain so legacy call sites can migrate incrementally.
#[async_trait]
pub trait SrtTransport: Send + Sync {
    async fn send_datagram(&self, data: Bytes) -> Result<()>;
    async fn recv_datagram(&self) -> Result<Bytes>;
    async fn send_control(&self, _data: Bytes) -> Result<()> {
        Err(unsupported_socket_op("send_control").into())
    }
    async fn recv_control(&self) -> Result<Bytes> {
        Err(unsupported_socket_op("recv_control").into())
    }
    fn max_datagram_size(&self) -> Option<usize> {
        None
    }
    fn quic_stats(&self) -> Option<quinn::ConnectionStats> {
        None
    }

    async fn send(&self, buf: &[u8]) -> std::io::Result<usize>;
    async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize>;
    async fn send_to(&self, _buf: &[u8], _target: SocketAddr) -> std::io::Result<usize> {
        Err(unsupported_socket_op("send_to"))
    }
    async fn recv_from(&self, _buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        Err(unsupported_socket_op("recv_from"))
    }
    fn try_recv(&self, buf: &mut [u8]) -> std::io::Result<usize>;
    fn local_addr(&self) -> std::io::Result<SocketAddr>;
}

fn unsupported_socket_op(op: &'static str) -> Error {
    Error::new(
        ErrorKind::Unsupported,
        format!("{op} is unsupported for connection-oriented QUIC transports"),
    )
}

fn io_other(err: impl std::fmt::Display) -> Error {
    Error::other(err.to_string())
}

pub struct QuicTransport {
    connection: Connection,
}

impl QuicTransport {
    pub fn new(connection: Connection) -> Self {
        Self { connection }
    }
}

#[async_trait]
impl SrtTransport for QuicTransport {
    async fn send_datagram(&self, data: Bytes) -> Result<()> {
        self.connection.send_datagram(data)?;
        Ok(())
    }

    async fn recv_datagram(&self) -> Result<Bytes> {
        Ok(self.connection.read_datagram().await?)
    }

    async fn send_control(&self, data: Bytes) -> Result<()> {
        let (mut send, _recv) = self.connection.open_bi().await?;
        let len = u32::try_from(data.len())
            .map_err(|_| io_other("control payload too large for 32-bit stream frame"))?;
        send.write_all(&len.to_le_bytes()).await.map_err(io_other)?;
        send.write_all(&data).await.map_err(io_other)?;
        send.finish().map_err(io_other)?;
        Ok(())
    }

    async fn recv_control(&self) -> Result<Bytes> {
        let (_send, mut recv) = self.connection.accept_bi().await?;
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf).await.map_err(io_other)?;
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut payload = vec![0u8; len];
        recv.read_exact(&mut payload).await.map_err(io_other)?;
        Ok(Bytes::from(payload))
    }

    fn max_datagram_size(&self) -> Option<usize> {
        self.connection.max_datagram_size()
    }

    fn quic_stats(&self) -> Option<quinn::ConnectionStats> {
        Some(self.connection.stats())
    }

    async fn send(&self, buf: &[u8]) -> std::io::Result<usize> {
        self.connection
            .send_datagram(Bytes::copy_from_slice(buf))
            .map_err(io_other)?;
        Ok(buf.len())
    }

    async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let datagram = self.connection.read_datagram().await.map_err(io_other)?;
        let copied = datagram.len().min(buf.len());
        buf[..copied].copy_from_slice(&datagram[..copied]);
        Ok(copied)
    }

    fn try_recv(&self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(unsupported_socket_op("try_recv"))
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Err(unsupported_socket_op("local_addr"))
    }
}
