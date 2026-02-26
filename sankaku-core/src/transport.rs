use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use std::net::SocketAddr;
use tokio::net::UdpSocket;

const MAX_DATAGRAM_SIZE: usize = 65_507;

/// Datagram transport abstraction for unreliable media/control exchange.
///
/// The `send_datagram`/`recv_datagram` APIs provide a codec/socket-agnostic slot
/// for future QUIC/WebRTC/Tailscale transports. Compatibility methods remain so
/// existing UDP session code can migrate incrementally.
#[async_trait]
pub trait SrtTransport: Send + Sync {
    async fn send_datagram(&self, data: Bytes) -> Result<()>;
    async fn recv_datagram(&self) -> Result<Bytes>;

    async fn send(&self, buf: &[u8]) -> std::io::Result<usize>;
    async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize>;
    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> std::io::Result<usize>;
    async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)>;
    fn try_recv(&self, buf: &mut [u8]) -> std::io::Result<usize>;
    fn local_addr(&self) -> std::io::Result<SocketAddr>;
}

pub struct UdpTransport {
    socket: UdpSocket,
}

impl UdpTransport {
    pub fn new(socket: UdpSocket) -> Self {
        Self { socket }
    }
}

#[async_trait]
impl SrtTransport for UdpTransport {
    async fn send_datagram(&self, data: Bytes) -> Result<()> {
        self.socket.send(&data).await?;
        Ok(())
    }

    async fn recv_datagram(&self) -> Result<Bytes> {
        let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
        let size = self.socket.recv(&mut buf).await?;
        buf.truncate(size);
        Ok(Bytes::from(buf))
    }

    async fn send(&self, buf: &[u8]) -> std::io::Result<usize> {
        self.socket.send(buf).await
    }

    async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.socket.recv(buf).await
    }

    async fn send_to(&self, buf: &[u8], target: SocketAddr) -> std::io::Result<usize> {
        self.socket.send_to(buf, target).await
    }

    async fn recv_from(&self, buf: &mut [u8]) -> std::io::Result<(usize, SocketAddr)> {
        self.socket.recv_from(buf).await
    }

    fn try_recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.socket.try_recv(buf)
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}
