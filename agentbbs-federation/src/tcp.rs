//! Live peer-to-peer federation over TCP.
//!
//! [`TcpTransport`] is a production [`Transport`]: it dials a peer's `addr` and
//! writes a length-prefixed sealed envelope. [`FederationServer`] is the
//! matching ingress — it accepts TCP connections, reads framed envelopes, and
//! feeds each to [`Federator::ingest`], which verifies the node signature (and,
//! for replicated posts, the author signature) before anything touches the
//! store.
//!
//! Wire framing: a 4-byte big-endian length prefix followed by that many bytes
//! of [`FederationEnvelope::to_bytes`] output. Frames larger than
//! [`MAX_FRAME`] are refused to bound memory.

use std::net::SocketAddr;
use std::sync::Arc;

use agentbbs_core::{Error, Result};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::federator::Federator;
use crate::peer::Peer;
use crate::transport::Transport;

/// Maximum accepted frame size (8 MiB) — a sanity bound against abuse.
pub const MAX_FRAME: u32 = 8 * 1024 * 1024;

/// A [`Transport`] that delivers envelopes to peers over TCP — directly, or
/// through a SOCKS5 proxy (e.g. Tor) so peers can be reached anonymously and
/// `.onion` addresses can be dialled.
#[derive(Clone, Default)]
pub struct TcpTransport {
    timeout: std::time::Duration,
    /// Optional SOCKS5 proxy `host:port` (e.g. Tor's `127.0.0.1:9050`). When
    /// set, all peer connections are tunnelled through it, which both hides the
    /// node's IP and enables `.onion` peers.
    socks5: Option<String>,
}

impl TcpTransport {
    /// A transport with a default 5s connect/write timeout and direct dialling.
    pub fn new() -> Self {
        TcpTransport {
            timeout: std::time::Duration::from_secs(5),
            socks5: None,
        }
    }

    /// Read transport config from the environment: `AGENTBBS_SOCKS5` (e.g.
    /// `127.0.0.1:9050`) routes federation egress through that SOCKS5 proxy.
    pub fn from_env() -> Self {
        let mut t = TcpTransport::new();
        if let Ok(p) = std::env::var("AGENTBBS_SOCKS5") {
            let p = p.trim();
            if !p.is_empty() {
                t.socks5 = Some(p.to_string());
            }
        }
        t
    }

    /// Override the connect/write timeout.
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Route all peer connections through a SOCKS5 proxy (e.g. Tor at
    /// `127.0.0.1:9050`). Pass `None` for direct dialling.
    pub fn with_socks5(mut self, proxy: Option<String>) -> Self {
        self.socks5 = proxy.filter(|p| !p.trim().is_empty());
        self
    }

    /// Whether egress is tunnelled through a SOCKS5 proxy.
    pub fn is_proxied(&self) -> bool {
        self.socks5.is_some()
    }

    async fn deliver(&self, addr: &str, bytes: &[u8]) -> std::io::Result<()> {
        let mut stream = match &self.socks5 {
            Some(proxy) => {
                let (host, port) = split_host_port(addr)?;
                socks5_connect(proxy, &host, port).await?
            }
            None => TcpStream::connect(addr).await?,
        };
        let len = (bytes.len() as u32).to_be_bytes();
        stream.write_all(&len).await?;
        stream.write_all(bytes).await?;
        stream.flush().await?;
        stream.shutdown().await?;
        Ok(())
    }
}

/// Split a `host:port` (or `[v6]:port`) into its parts.
fn split_host_port(addr: &str) -> std::io::Result<(String, u16)> {
    let (host, port) = addr.rsplit_once(':').ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "peer addr needs host:port")
    })?;
    let port: u16 = port
        .parse()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad peer port"))?;
    Ok((host.trim_matches(|c| c == '[' || c == ']').to_string(), port))
}

/// Build the SOCKS5 (RFC 1928) CONNECT request for `host:port` using the
/// domain-name address type (so `.onion` and hostnames pass through to Tor).
/// Factored out so it is unit-testable without a network.
pub(crate) fn socks5_connect_request(host: &str, port: u16) -> std::io::Result<Vec<u8>> {
    let hb = host.as_bytes();
    if hb.len() > 255 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "socks5 host too long",
        ));
    }
    let mut req = Vec::with_capacity(7 + hb.len());
    req.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, hb.len() as u8]); // VER, CONNECT, RSV, ATYP=domain, len
    req.extend_from_slice(hb);
    req.extend_from_slice(&port.to_be_bytes());
    Ok(req)
}

/// Perform a SOCKS5 (no-auth) CONNECT to `host:port` via `proxy`, returning the
/// tunnelled stream. Works for `.onion` via the domain-name address type.
async fn socks5_connect(proxy: &str, host: &str, port: u16) -> std::io::Result<TcpStream> {
    let mut s = TcpStream::connect(proxy).await?;
    // Greeting: VER=5, NMETHODS=1, METHOD=0 (no auth).
    s.write_all(&[0x05, 0x01, 0x00]).await?;
    let mut method = [0u8; 2];
    s.read_exact(&mut method).await?;
    if method[0] != 0x05 || method[1] != 0x00 {
        return Err(std::io::Error::other("socks5: no acceptable auth method"));
    }
    s.write_all(&socks5_connect_request(host, port)?).await?;
    let mut head = [0u8; 4];
    s.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        return Err(std::io::Error::other(format!("socks5 connect failed (reply {})", head[1])));
    }
    // Consume the bound address per ATYP, then the 2-byte bound port.
    match head[3] {
        0x01 => {
            let mut a = [0u8; 4];
            s.read_exact(&mut a).await?;
        }
        0x03 => {
            let mut l = [0u8; 1];
            s.read_exact(&mut l).await?;
            let mut a = vec![0u8; l[0] as usize];
            s.read_exact(&mut a).await?;
        }
        0x04 => {
            let mut a = [0u8; 16];
            s.read_exact(&mut a).await?;
        }
        _ => {
            return Err(std::io::Error::other("socks5: unknown bound ATYP"))
        }
    }
    let mut p = [0u8; 2];
    s.read_exact(&mut p).await?;
    Ok(s)
}

#[async_trait]
impl Transport for TcpTransport {
    async fn send(&self, peer: &Peer, bytes: Vec<u8>) -> Result<()> {
        // Federation egress is best-effort and idempotent: an unreachable peer
        // is a no-op, not a failure (it can re-sync later). We only surface a
        // hard error for clearly invalid framing.
        if bytes.len() as u32 > MAX_FRAME {
            return Err(Error::Other("federation frame exceeds MAX_FRAME".into()));
        }
        let _ = tokio::time::timeout(self.timeout, self.deliver(&peer.addr, &bytes)).await;
        Ok(())
    }
}

/// A TCP ingress server that applies inbound envelopes to a [`Federator`].
pub struct FederationServer {
    federator: Arc<Federator>,
}

impl FederationServer {
    /// Build a server over a shared federator.
    pub fn new(federator: Arc<Federator>) -> Self {
        FederationServer { federator }
    }

    /// Bind a listener, returning it and the resolved local address (use
    /// `127.0.0.1:0` to get an ephemeral port).
    pub async fn bind(addr: &str) -> Result<(TcpListener, SocketAddr)> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| Error::Other(format!("federation bind {addr}: {e}")))?;
        let local = listener
            .local_addr()
            .map_err(|e| Error::Other(format!("federation local_addr: {e}")))?;
        Ok((listener, local))
    }

    /// Accept connections forever, applying every framed envelope. Each
    /// connection is handled on its own task; a malformed frame ends only that
    /// connection.
    pub async fn serve(self, listener: TcpListener) -> Result<()> {
        loop {
            let (stream, _peer_addr) = listener
                .accept()
                .await
                .map_err(|e| Error::Other(format!("federation accept: {e}")))?;
            let federator = self.federator.clone();
            tokio::spawn(async move {
                let _ = handle_connection(stream, federator).await;
            });
        }
    }
}

async fn handle_connection(mut stream: TcpStream, federator: Arc<Federator>) -> std::io::Result<()> {
    loop {
        let mut len_buf = [0u8; 4];
        // Clean EOF between frames ends the connection.
        match stream.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(_) => return Ok(()),
        }
        let len = u32::from_be_bytes(len_buf);
        if len == 0 || len > MAX_FRAME {
            return Ok(());
        }
        let mut buf = vec![0u8; len as usize];
        stream.read_exact(&mut buf).await?;
        // Zero-trust ingest verifies before persisting; a bad frame is dropped.
        let _ = federator.ingest(&buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::{PeerBook, TrustLevel};
    use crate::transport::Transport;
    use agentbbs_core::{Board, Identity, Message, MessageBody, MemoryStore, NullReporter, Store};
    use chrono::Utc;

    fn author_message(author: &Identity, board: &str, text: &str) -> Message {
        let body = MessageBody {
            board: board.into(),
            parent: None,
            subject: "s".into(),
            body: text.into(),
            author: author.id(),
            handle: "h".into(),
            created_at: Utc::now(),
        };
        Message::sign(author, body).unwrap()
    }

    #[tokio::test]
    async fn two_node_tcp_replication() {
        // Receiver node B with a TCP ingress server.
        let b_store: Arc<dyn Store> = Arc::new(MemoryStore::new());
        let b_fed = Arc::new(Federator::new(
            Identity::generate(),
            b_store.clone(),
            Arc::new(NullReporter),
            Arc::new(TcpTransport::new()),
            PeerBook::new(),
        ));
        let (listener, addr) = FederationServer::bind("127.0.0.1:0").await.unwrap();
        let server = FederationServer::new(b_fed.clone());
        tokio::spawn(async move { server.serve(listener).await.ok() });

        // Sender node A trusts B at its bound address.
        let mut peers = PeerBook::new();
        peers.add(Peer::new(b_fed.node_id(), addr.to_string(), TrustLevel::Trusted));
        let a_store: Arc<dyn Store> = Arc::new(MemoryStore::new());
        let a_fed = Federator::new(
            Identity::generate(),
            a_store.clone(),
            Arc::new(NullReporter),
            Arc::new(TcpTransport::new()),
            peers,
        );

        // An ordinary author (distinct from the relaying node) signs content.
        let author = Identity::generate();
        let board = Board::new("general", "General", author.id());
        let msg = author_message(&author, "general", "hello over tcp");

        a_fed.announce_board(&board).await.unwrap();
        a_fed.replicate_message(&msg).await.unwrap();

        // Poll B's store until the replicated content lands (or time out).
        let mut got = false;
        for _ in 0..50 {
            if b_store.get_board("general").unwrap().is_some()
                && b_store.get_message(&msg.id).unwrap().is_some()
            {
                got = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(got, "message did not replicate over TCP");
        let replicated = b_store.get_message(&msg.id).unwrap().unwrap();
        assert_eq!(replicated.body.body, "hello over tcp");
        assert!(replicated.verify().is_ok());
    }

    #[test]
    fn socks5_config_builder_and_env() {
        assert!(!TcpTransport::new().is_proxied());
        assert!(TcpTransport::new()
            .with_socks5(Some("127.0.0.1:9050".into()))
            .is_proxied());
        // Empty proxy string is treated as "no proxy".
        assert!(!TcpTransport::new().with_socks5(Some("  ".into())).is_proxied());
    }

    #[test]
    fn socks5_connect_request_encodes_onion_host() {
        let host = "exampleonionaddressxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx.onion";
        let req = socks5_connect_request(host, 7420).unwrap();
        assert_eq!(&req[..4], &[0x05, 0x01, 0x00, 0x03]); // CONNECT, domain ATYP
        assert_eq!(req[4] as usize, host.len()); // domain length
        assert_eq!(&req[5..5 + host.len()], host.as_bytes());
        // Trailing 2 bytes = big-endian port 7420.
        assert_eq!(&req[5 + host.len()..], &7420u16.to_be_bytes());
    }

    #[test]
    fn split_host_port_parses() {
        assert_eq!(
            split_host_port("abc.onion:7420").unwrap(),
            ("abc.onion".to_string(), 7420)
        );
        assert!(split_host_port("no-port").is_err());
    }

    #[tokio::test]
    async fn unreachable_peer_is_best_effort() {
        // Sending to a closed port must not error (best-effort egress).
        let t = TcpTransport::new().with_timeout(std::time::Duration::from_millis(200));
        let peer = Peer::new(Identity::generate().id(), "127.0.0.1:9", TrustLevel::Trusted);
        assert!(t.send(&peer, vec![1, 2, 3]).await.is_ok());
    }
}
