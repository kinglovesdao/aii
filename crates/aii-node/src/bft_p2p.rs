//! TCP transport for BFT gossip (v0.0.34).
//!
//! Bridges [`aii_consensus_bft::BftTransport`] to
//! [`aii_net_p2p::Peer`]: outbound bytes are framed as
//! [`aii_net_p2p::Message::Bft`] and broadcast to every connected
//! peer; inbound `Message::Bft` frames push their payload into a
//! shared inbox the gossip driver drains synchronously.
//!
//! Connection model: each remote peer address gets one writer task
//! that reads from a `tokio::mpsc::UnboundedReceiver<Vec<u8>>` (the
//! shared outbox) and one reader task that pushes inbound Bft bytes
//! into the shared inbox `VecDeque`. The reader/writer tasks share
//! the same `TcpStream` via two halves.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aii_consensus_bft::BftTransport;
use aii_net_p2p::{Message, AII_P2P_VERSION};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// TCP-backed [`BftTransport`].
///
/// Construction is async because we bind a listener immediately. After
/// [`new`](Self::new) succeeds:
/// - [`broadcast`](BftTransport::broadcast) is non-blocking; it fans
///   the payload out to a `broadcast::Sender` whose subscribers are
///   the per-peer writer tasks.
/// - [`try_recv`](BftTransport::try_recv) pops one frame from a
///   `Mutex<VecDeque>` populated by per-peer reader tasks.
pub struct TcpBftTransport {
    /// Outbound fanout. Cloned per peer-writer.
    out_tx: broadcast::Sender<Vec<u8>>,
    /// Inbound queue, drained by `try_recv`.
    inbox: Arc<Mutex<VecDeque<Vec<u8>>>>,
    /// Address the listener bound to.
    local_addr: SocketAddr,
    /// Handles to the spawned acceptor + dialer tasks (kept so the
    /// transport's `Drop` can abort them).
    _tasks: Vec<JoinHandle<()>>,
}

impl TcpBftTransport {
    /// Bind a listener on `bind_addr` and dial each of `peer_addrs`.
    ///
    /// Dial failures are NOT fatal — peers come and go; the driver
    /// will retry on its own cadence.
    ///
    /// Returns the transport plus its actually-bound address (useful
    /// when callers pass port 0).
    pub async fn new(bind_addr: SocketAddr, peer_addrs: Vec<SocketAddr>) -> std::io::Result<Self> {
        let listener = TcpListener::bind(bind_addr).await?;
        let local_addr = listener.local_addr()?;
        let (out_tx, _) = broadcast::channel::<Vec<u8>>(1024);
        let inbox = Arc::new(Mutex::new(VecDeque::new()));

        let mut tasks: Vec<JoinHandle<()>> = Vec::new();

        // Acceptor: every inbound TcpStream gets a reader + writer pair.
        {
            let out_tx = out_tx.clone();
            let inbox = inbox.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => {
                            spawn_peer_tasks(stream, &out_tx, &inbox);
                        }
                        Err(e) => {
                            tracing::warn!(?e, "bft listener accept failed");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            }));
        }

        // Dialer: one per remote address, with infinite retry.
        for addr in peer_addrs {
            let out_tx = out_tx.clone();
            let inbox = inbox.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    match TcpStream::connect(addr).await {
                        Ok(stream) => {
                            let inbox = inbox.clone();
                            let out_tx = out_tx.clone();
                            // Block until this connection dies, then retry.
                            run_peer(stream, out_tx, inbox).await;
                        }
                        Err(_) => {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                    }
                }
            }));
        }

        Ok(Self {
            out_tx,
            inbox,
            local_addr,
            _tasks: tasks,
        })
    }

    /// Address the listener bound to (useful when port 0 was requested).
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

impl BftTransport for TcpBftTransport {
    fn broadcast(&self, bytes: Vec<u8>) {
        // No subscribers (no peers connected yet) → send drops the value.
        // That's expected during startup; the driver will resend on the
        // next tick anyway.
        let _ = self.out_tx.send(bytes);
    }

    fn try_recv(&self) -> Option<Vec<u8>> {
        self.inbox.lock().ok()?.pop_front()
    }
}

fn spawn_peer_tasks(
    stream: TcpStream,
    out_tx: &broadcast::Sender<Vec<u8>>,
    inbox: &Arc<Mutex<VecDeque<Vec<u8>>>>,
) {
    let out_tx = out_tx.clone();
    let inbox = inbox.clone();
    tokio::spawn(async move {
        run_peer(stream, out_tx, inbox).await;
    });
}

/// Owns one TCP connection's read + write halves. Returns when either
/// side fails.
async fn run_peer(
    stream: TcpStream,
    out_tx: broadcast::Sender<Vec<u8>>,
    inbox: Arc<Mutex<VecDeque<Vec<u8>>>>,
) {
    let (mut rx, mut tx) = stream.into_split();
    let mut out_rx = out_tx.subscribe();

    // Hello exchange (best-effort; ignore failures).
    let hello = Message::Hello {
        version: AII_P2P_VERSION,
        name: "aiid".to_string(),
    };
    let _ = write_message(&mut tx, &hello).await;

    let reader = tokio::spawn(async move {
        loop {
            match read_message(&mut rx).await {
                Ok(Message::Bft(payload)) => {
                    if let Ok(mut q) = inbox.lock() {
                        q.push_back(payload);
                    }
                }
                Ok(_) => {
                    // Ignore Hello/Ping/Pong/Disconnect at this layer.
                }
                Err(_) => break,
            }
        }
    });

    let writer = tokio::spawn(async move {
        loop {
            match out_rx.recv().await {
                Ok(bytes) => {
                    let m = Message::Bft(bytes);
                    if write_message(&mut tx, &m).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    });

    let _ = tokio::join!(reader, writer);
}

/// Frame format mirrors `aii_net_p2p::Peer`: 4-byte big-endian length +
/// body. We do it locally here because `Peer` owns the full stream and
/// we want to split it into read/write halves.
async fn write_message(
    tx: &mut tokio::net::tcp::OwnedWriteHalf,
    m: &Message,
) -> std::io::Result<()> {
    let body = encode_message(m);
    let len_be = (body.len() as u32).to_be_bytes();
    tx.write_all(&len_be).await?;
    tx.write_all(&body).await?;
    tx.flush().await
}

async fn read_message(rx: &mut tokio::net::tcp::OwnedReadHalf) -> std::io::Result<Message> {
    let mut len_buf = [0u8; 4];
    rx.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > aii_net_p2p::MAX_FRAME_BYTES as usize {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut body = vec![0u8; len];
    rx.read_exact(&mut body).await?;
    decode_message(&body)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

fn encode_message(m: &Message) -> Vec<u8> {
    m.encode()
}

fn decode_message(bytes: &[u8]) -> Result<Message, aii_net_p2p::P2pError> {
    Message::decode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Two transports exchange a payload over loopback TCP.
    #[tokio::test]
    async fn two_tcp_transports_exchange_payload() {
        let a = TcpBftTransport::new("127.0.0.1:0".parse().unwrap(), vec![])
            .await
            .unwrap();
        let a_addr = a.local_addr();
        let b = TcpBftTransport::new("127.0.0.1:0".parse().unwrap(), vec![a_addr])
            .await
            .unwrap();

        // Give the dialer + acceptor a moment to handshake.
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            b.broadcast(vec![0xde, 0xad, 0xbe, 0xef]);
            if a.try_recv().is_some() {
                return;
            }
        }
        panic!("never received broadcast");
    }
}
