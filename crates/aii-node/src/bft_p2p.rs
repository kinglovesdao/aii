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

use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aii_consensus_bft::BftTransport;
use aii_crypto::keccak256;
use aii_net_p2p::{Message, AII_P2P_VERSION};
use aii_types::H256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinHandle;

/// Idle read timeout for BFT peer connections.
///
/// If no inbound bytes arrive within this window the session is
/// presumed dead (NAT proxy silently dropped it, ISP reset it, peer
/// crashed, …) and the dialer reconnects. BFT-PoS at 3 s block time
/// sends ≥1 message per slot, so 30 s without any traffic is
/// unambiguous evidence the link is gone.
///
/// Added in v0.0.68 for NAT-friendly BFT; previously the read just
/// blocked forever, which let stale connections silently swallow votes
/// after Mihomo / NAT keepalive expiry.
pub const BFT_PEER_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Loopback bind address used by [`TcpBftTransport::new_outbound_only`]
/// and its Noise variant. Picks a kernel-assigned port so multiple
/// outbound-only transports on one host don't collide.
const OUTBOUND_ONLY_BIND: &str = "127.0.0.1:0";

/// Capacity of the gossip relay dedup ring (v0.0.69).
///
/// Holds the most-recent N message hashes seen on the wire or
/// originated locally; any incoming message whose hash is already in
/// the ring is dropped (already-seen). 4096 covers ~22 minutes of
/// 3-validator BFT traffic at 3 s block time + 2 phases + room for
/// timeout retries — well past the window any single message stays
/// relevant.
const GOSSIP_DEDUP_CAPACITY: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WireMode {
    Plain,
    Noise,
}

/// Hash-based dedup for gossip relay.
///
/// Bounded by [`GOSSIP_DEDUP_CAPACITY`]. Insertions return `true` if
/// the hash was novel (caller should forward / consume) and `false`
/// if it was already known (drop). Eviction is FIFO — oldest hash
/// out when capacity is hit. Internal state is sync (uses
/// `std::sync::Mutex`) because the hot path is keccak + HashSet ops
/// which never await.
#[derive(Debug)]
struct GossipDedup {
    set: HashSet<H256>,
    queue: VecDeque<H256>,
}

impl GossipDedup {
    fn new() -> Self {
        Self {
            set: HashSet::with_capacity(GOSSIP_DEDUP_CAPACITY),
            queue: VecDeque::with_capacity(GOSSIP_DEDUP_CAPACITY),
        }
    }

    /// Insert `h` if novel. Returns `true` iff the hash hadn't been
    /// seen before — caller should treat the message as fresh.
    fn observe(&mut self, h: H256) -> bool {
        if !self.set.insert(h) {
            return false;
        }
        if self.queue.len() >= GOSSIP_DEDUP_CAPACITY {
            if let Some(old) = self.queue.pop_front() {
                self.set.remove(&old);
            }
        }
        self.queue.push_back(h);
        true
    }
}

fn hash_payload(payload: &[u8]) -> H256 {
    keccak256(payload)
}

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
    /// Gossip relay dedup (v0.0.69). Shared across every peer task
    /// AND the local broadcast path so:
    /// - Locally-originated messages are pre-seeded into the ring,
    ///   suppressing echoes when a relayer bounces them back.
    /// - Each incoming message is checked exactly once; novel ones
    ///   are pushed to `inbox` AND re-broadcast via `out_tx` for
    ///   relay to other peers (cycles die at the next hop).
    dedup: Arc<Mutex<GossipDedup>>,
    /// Address the listener bound to.
    local_addr: SocketAddr,
    /// Whether outbound dialers wrap sockets in Noise.
    wire_mode: WireMode,
    /// Remote addresses with active retry dialers.
    dial_targets: Arc<Mutex<HashSet<SocketAddr>>>,
    /// Handles to the spawned acceptor + dialer tasks (kept so the
    /// transport's `Drop` can abort them).
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
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
        let dedup = Arc::new(Mutex::new(GossipDedup::new()));
        let dial_targets = Arc::new(Mutex::new(HashSet::new()));
        let tasks = Arc::new(Mutex::new(Vec::<JoinHandle<()>>::new()));

        // Acceptor: every inbound TcpStream gets a reader + writer pair.
        {
            let out_tx = out_tx.clone();
            let inbox = inbox.clone();
            let dedup = dedup.clone();
            let task = tokio::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => {
                            spawn_peer_tasks(stream, &out_tx, &inbox, &dedup);
                        }
                        Err(e) => {
                            tracing::warn!(?e, "bft listener accept failed");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            });
            if let Ok(mut guard) = tasks.lock() {
                guard.push(task);
            }
        }

        let transport = Self {
            out_tx,
            inbox,
            dedup,
            local_addr,
            wire_mode: WireMode::Plain,
            dial_targets,
            tasks,
        };
        for addr in peer_addrs {
            let _ = transport.add_peer(addr);
        }
        Ok(transport)
    }

    /// Like [`Self::new`] but every accepted / dialed connection is
    /// wrapped in a Noise XX handshake (roadmap C.4 wire-up); after
    /// the handshake completes, BFT bytes flow through an AEAD-
    /// encrypted session (ChaCha20-Poly1305).
    ///
    /// The single-task design (one async task per peer, owning both
    /// the stream and the `EncryptedSession`) sidesteps Noise's
    /// non-`Sync` `TransportState`. Outbound messages are polled
    /// non-blocking, then a 20 ms `select` window listens for
    /// inbound. BFT timing budget is in seconds, so the 20 ms cadence
    /// is invisible.
    pub async fn new_encrypted(
        bind_addr: SocketAddr,
        peer_addrs: Vec<SocketAddr>,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(bind_addr).await?;
        let local_addr = listener.local_addr()?;
        let (out_tx, _) = broadcast::channel::<Vec<u8>>(1024);
        let inbox = Arc::new(Mutex::new(VecDeque::new()));
        let dedup = Arc::new(Mutex::new(GossipDedup::new()));
        let dial_targets = Arc::new(Mutex::new(HashSet::new()));
        let tasks = Arc::new(Mutex::new(Vec::<JoinHandle<()>>::new()));

        {
            let out_tx = out_tx.clone();
            let inbox = inbox.clone();
            let dedup = dedup.clone();
            let task = tokio::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => {
                            let out_tx = out_tx.clone();
                            let inbox = inbox.clone();
                            let dedup = dedup.clone();
                            tokio::spawn(async move {
                                run_peer_noise(stream, false, out_tx, inbox, dedup).await;
                            });
                        }
                        Err(e) => {
                            tracing::warn!(?e, "bft noise listener accept failed");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            });
            if let Ok(mut guard) = tasks.lock() {
                guard.push(task);
            }
        }

        let transport = Self {
            out_tx,
            inbox,
            dedup,
            local_addr,
            wire_mode: WireMode::Noise,
            dial_targets,
            tasks,
        };
        for addr in peer_addrs {
            let _ = transport.add_peer(addr);
        }
        Ok(transport)
    }

    /// BTC-style outbound-only constructor (v0.0.68).
    ///
    /// Binds the listener to `127.0.0.1:0` so no public BFT port is
    /// exposed; dial each entry in `peer_addrs` and conduct **all**
    /// BFT traffic over the resulting outbound TCP sockets. Works for
    /// validators behind home NAT or HTTP-only proxy chains (Mihomo /
    /// Clash, Cloudflare WARP, corporate VPN) where the public port
    /// 30311 is not reachable from the rest of the validator set.
    ///
    /// The transport is symmetric: once an outbound TCP is up the
    /// remote validator can write votes/proposals back through the
    /// same socket, so no relay is required for the two-validator and
    /// three-validator cases. Larger validator sets still benefit
    /// from relay (deferred to v0.0.69).
    pub async fn new_outbound_only(peer_addrs: Vec<SocketAddr>) -> std::io::Result<Self> {
        let bind_addr: SocketAddr = OUTBOUND_ONLY_BIND
            .parse()
            .expect("OUTBOUND_ONLY_BIND is a valid SocketAddr");
        Self::new(bind_addr, peer_addrs).await
    }

    /// Like [`Self::new_outbound_only`] but every connection is Noise
    /// XX encrypted. Use this for any validator that wants both NAT-
    /// friendliness *and* on-the-wire confidentiality.
    pub async fn new_outbound_only_encrypted(peer_addrs: Vec<SocketAddr>) -> std::io::Result<Self> {
        let bind_addr: SocketAddr = OUTBOUND_ONLY_BIND
            .parse()
            .expect("OUTBOUND_ONLY_BIND is a valid SocketAddr");
        Self::new_encrypted(bind_addr, peer_addrs).await
    }

    /// Address the listener bound to (useful when port 0 was requested).
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Start a retrying outbound dialer for `addr` if one is not
    /// already active.
    ///
    /// Returns `true` when a new dialer was spawned. This is used by
    /// runtime Discovery v4 refreshes so newly found peers join the
    /// live BFT fanout immediately instead of waiting for a process
    /// restart.
    #[must_use]
    pub fn add_peer(&self, addr: SocketAddr) -> bool {
        if addr == self.local_addr {
            return false;
        }
        {
            let Ok(mut targets) = self.dial_targets.lock() else {
                return false;
            };
            if !targets.insert(addr) {
                return false;
            }
        }
        let task = match self.wire_mode {
            WireMode::Plain => spawn_plain_dialer(
                addr,
                self.out_tx.clone(),
                self.inbox.clone(),
                self.dedup.clone(),
            ),
            WireMode::Noise => spawn_noise_dialer(
                addr,
                self.out_tx.clone(),
                self.inbox.clone(),
                self.dedup.clone(),
            ),
        };
        if let Ok(mut guard) = self.tasks.lock() {
            guard.push(task);
        }
        true
    }
}

fn spawn_plain_dialer(
    addr: SocketAddr,
    out_tx: broadcast::Sender<Vec<u8>>,
    inbox: Arc<Mutex<VecDeque<Vec<u8>>>>,
    dedup: Arc<Mutex<GossipDedup>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match TcpStream::connect(addr).await {
                Ok(stream) => {
                    run_peer(stream, out_tx.clone(), inbox.clone(), dedup.clone()).await;
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    })
}

fn spawn_noise_dialer(
    addr: SocketAddr,
    out_tx: broadcast::Sender<Vec<u8>>,
    inbox: Arc<Mutex<VecDeque<Vec<u8>>>>,
    dedup: Arc<Mutex<GossipDedup>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match TcpStream::connect(addr).await {
                Ok(stream) => {
                    run_peer_noise(stream, true, out_tx.clone(), inbox.clone(), dedup.clone())
                        .await;
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    })
}

impl BftTransport for TcpBftTransport {
    fn broadcast(&self, bytes: Vec<u8>) {
        // Pre-seed the dedup ring with the hash of this outbound
        // message (v0.0.69). When a relayer bounces the message back
        // to us, the peer reader will find the hash already in the
        // ring and silently drop the echo. Without this pre-seed,
        // local BFT messages would loop A→B→A every relay hop.
        let h = hash_payload(&bytes);
        if let Ok(mut d) = self.dedup.lock() {
            d.observe(h);
        }
        // No subscribers (no peers connected yet) → send drops the value.
        // That's expected during startup; the driver will resend on the
        // next tick anyway.
        let _ = self.out_tx.send(bytes);
    }

    fn try_recv(&self) -> Option<Vec<u8>> {
        self.inbox.lock().ok()?.pop_front()
    }
}

/// Single-task per-peer runner over a Noise-encrypted session.
///
/// Owns the `TcpStream` + `EncryptedSession` for the lifetime of the
/// connection. Polls the outbound broadcast (non-blocking) then
/// listens 20 ms for inbound. Exits cleanly on handshake failure,
/// EOF, or framing error — the caller's dial loop will reconnect.
async fn run_peer_noise(
    mut stream: TcpStream,
    is_initiator: bool,
    out_tx: broadcast::Sender<Vec<u8>>,
    inbox: Arc<Mutex<VecDeque<Vec<u8>>>>,
    dedup: Arc<Mutex<GossipDedup>>,
) {
    let hs = if is_initiator {
        match aii_net_p2p::noise::initiator() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(?e, "noise initiator init failed");
                return;
            }
        }
    } else {
        match aii_net_p2p::noise::responder() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(?e, "noise responder init failed");
                return;
            }
        }
    };
    let mut session = if is_initiator {
        match aii_net_p2p::noise::handshake_initiator(hs, &mut stream).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(?e, "noise initiator handshake failed");
                return;
            }
        }
    } else {
        match aii_net_p2p::noise::handshake_responder(hs, &mut stream).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(?e, "noise responder handshake failed");
                return;
            }
        }
    };
    let mut out_rx = out_tx.subscribe();
    let mut last_recv = tokio::time::Instant::now();
    loop {
        // Non-blocking outbound poll: drain everything pending.
        loop {
            match out_rx.try_recv() {
                Ok(payload) => {
                    if session.send_msg(&mut stream, &payload).await.is_err() {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return,
            }
        }
        // Bounded inbound wait so we don't starve the outbound side.
        match tokio::time::timeout(Duration::from_millis(20), session.recv_msg(&mut stream)).await {
            Ok(Ok(payload)) => {
                last_recv = tokio::time::Instant::now();
                let h = hash_payload(&payload);
                let novel = dedup.lock().map(|mut d| d.observe(h)).unwrap_or(true);
                if novel {
                    if let Ok(mut q) = inbox.lock() {
                        q.push_back(payload.clone());
                    }
                    // v0.0.69 gossip relay — same flow as the
                    // plaintext path: fan novel messages out so a
                    // direct-link failure between two non-adjacent
                    // peers can be bridged.
                    let _ = out_tx.send(payload);
                }
            }
            Ok(Err(_)) => return,
            Err(_) => {
                // Inner 20 ms poll timed out — loop and re-poll outbound.
                // If nothing has arrived in BFT_PEER_IDLE_TIMEOUT total,
                // the link is presumed dead (v0.0.68 NAT-friendly).
                if last_recv.elapsed() >= BFT_PEER_IDLE_TIMEOUT {
                    return;
                }
            }
        }
    }
}

fn spawn_peer_tasks(
    stream: TcpStream,
    out_tx: &broadcast::Sender<Vec<u8>>,
    inbox: &Arc<Mutex<VecDeque<Vec<u8>>>>,
    dedup: &Arc<Mutex<GossipDedup>>,
) {
    let out_tx = out_tx.clone();
    let inbox = inbox.clone();
    let dedup = dedup.clone();
    tokio::spawn(async move {
        run_peer(stream, out_tx, inbox, dedup).await;
    });
}

/// Owns one TCP connection's read + write halves. Returns when either
/// side fails or no inbound bytes arrive within
/// [`BFT_PEER_IDLE_TIMEOUT`] (v0.0.68: NAT-friendly idle detection).
///
/// v0.0.69: every novel Bft payload is also fanned out via `out_tx`
/// to relay to the other peers (gossip relay), letting A↔B↔C
/// topologies survive any single direct-link failure.
async fn run_peer(
    stream: TcpStream,
    out_tx: broadcast::Sender<Vec<u8>>,
    inbox: Arc<Mutex<VecDeque<Vec<u8>>>>,
    dedup: Arc<Mutex<GossipDedup>>,
) {
    let (mut rx, mut tx) = stream.into_split();
    let mut out_rx = out_tx.subscribe();
    let dead = Arc::new(Notify::new());
    let dead_for_reader = dead.clone();
    let dead_for_writer = dead.clone();
    let out_tx_for_reader = out_tx.clone();

    // Hello exchange (best-effort; ignore failures).
    let hello = Message::Hello {
        version: AII_P2P_VERSION,
        name: "aiid".to_string(),
    };
    let _ = write_message(&mut tx, &hello).await;

    let reader = tokio::spawn(async move {
        loop {
            match tokio::time::timeout(BFT_PEER_IDLE_TIMEOUT, read_message(&mut rx)).await {
                Ok(Ok(Message::Bft(payload))) => {
                    let h = hash_payload(&payload);
                    let novel = dedup.lock().map(|mut d| d.observe(h)).unwrap_or(true);
                    if novel {
                        if let Ok(mut q) = inbox.lock() {
                            q.push_back(payload.clone());
                        }
                        // Relay to other peers. Echoes back to the
                        // source connection are dropped by the
                        // source-node's own dedup on next receipt.
                        let _ = out_tx_for_reader.send(payload);
                    }
                }
                Ok(Ok(_)) => {
                    // Ignore Hello/Ping/Pong/Disconnect at this layer.
                }
                Ok(Err(_)) | Err(_) => break,
            }
        }
        // Wake the writer so it exits its `recv().await` and the dialer
        // can reconnect.
        dead_for_reader.notify_waiters();
    });

    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                () = dead_for_writer.notified() => break,
                msg = out_rx.recv() => match msg {
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

    /// Runtime peer addition: a node can learn a peer after startup
    /// (via Discovery v4) and begin dialing it without rebuilding the
    /// transport.
    #[tokio::test]
    async fn add_peer_dials_runtime_plaintext_peer() {
        let a = TcpBftTransport::new("127.0.0.1:0".parse().unwrap(), vec![])
            .await
            .unwrap();
        let b = TcpBftTransport::new("127.0.0.1:0".parse().unwrap(), vec![])
            .await
            .unwrap();

        assert!(b.add_peer(a.local_addr()));
        assert!(
            !b.add_peer(a.local_addr()),
            "duplicate peer must be ignored"
        );

        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            b.broadcast(vec![0x11, 0x22, 0x33]);
            if a.try_recv().is_some() {
                return;
            }
        }
        panic!("runtime-added plaintext peer never received broadcast");
    }

    /// Two transports exchange a payload over a Noise XX handshake
    /// (encrypted BFT gossip — v0.0.64). Same shape as the plaintext
    /// test, just with `new_encrypted`.
    #[tokio::test]
    async fn two_encrypted_transports_exchange_payload() {
        let a = TcpBftTransport::new_encrypted("127.0.0.1:0".parse().unwrap(), vec![])
            .await
            .unwrap();
        let a_addr = a.local_addr();
        let b = TcpBftTransport::new_encrypted("127.0.0.1:0".parse().unwrap(), vec![a_addr])
            .await
            .unwrap();
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            b.broadcast(vec![0xfe, 0xed, 0xfa, 0xce]);
            if a.try_recv().is_some() {
                return;
            }
        }
        panic!("never received broadcast over Noise transport");
    }

    #[tokio::test]
    async fn add_peer_dials_runtime_noise_peer() {
        let a = TcpBftTransport::new_encrypted("127.0.0.1:0".parse().unwrap(), vec![])
            .await
            .unwrap();
        let b = TcpBftTransport::new_encrypted("127.0.0.1:0".parse().unwrap(), vec![])
            .await
            .unwrap();

        assert!(b.add_peer(a.local_addr()));

        for _ in 0..60 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            b.broadcast(vec![0x44, 0x55, 0x66]);
            if a.try_recv().is_some() {
                return;
            }
        }
        panic!("runtime-added noise peer never received broadcast");
    }

    /// Outbound-only mode (v0.0.68) — B dials A using
    /// `new_outbound_only`. B does not bind a public port; once the
    /// outbound TCP is established, both directions of BFT traffic
    /// flow over the same socket. A acts as the "listener-side"
    /// validator (real testnet role); B as the NAT-bound validator.
    ///
    /// Asserts both directions: A→B and B→A.
    #[tokio::test]
    async fn outbound_only_round_trip_both_directions() {
        let a = TcpBftTransport::new("127.0.0.1:0".parse().unwrap(), vec![])
            .await
            .unwrap();
        let a_addr = a.local_addr();
        let b = TcpBftTransport::new_outbound_only(vec![a_addr])
            .await
            .unwrap();
        // B's listener should bind to loopback only.
        assert_eq!(
            b.local_addr().ip(),
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            "outbound-only transport must bind loopback"
        );

        let mut a_got = false;
        let mut b_got = false;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if !a_got {
                b.broadcast(vec![0xb1, 0xb2, 0xb3]);
                if a.try_recv().is_some() {
                    a_got = true;
                }
            }
            if !b_got {
                a.broadcast(vec![0xa1, 0xa2, 0xa3]);
                if b.try_recv().is_some() {
                    b_got = true;
                }
            }
            if a_got && b_got {
                return;
            }
        }
        panic!("outbound-only didn't round-trip both directions: a_got={a_got} b_got={b_got}");
    }

    /// Gossip relay (v0.0.69) — 3-node line A↔B↔C topology.
    ///
    /// A and C have NO direct connection; B is in the middle. When A
    /// broadcasts a message, B relays it to C, which would otherwise
    /// be unreachable. This is the fundamental BFT property we need
    /// for behind-NAT validators in any topology.
    #[tokio::test]
    async fn gossip_relay_three_node_line_topology() {
        // B has no peers configured — A and C will dial in.
        let b = TcpBftTransport::new("127.0.0.1:0".parse().unwrap(), vec![])
            .await
            .unwrap();
        let b_addr = b.local_addr();
        // A dials B.
        let a = TcpBftTransport::new("127.0.0.1:0".parse().unwrap(), vec![b_addr])
            .await
            .unwrap();
        // C dials B. A and C are NOT directly connected.
        let c = TcpBftTransport::new("127.0.0.1:0".parse().unwrap(), vec![b_addr])
            .await
            .unwrap();

        // Allow handshakes to complete.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // A broadcasts a unique payload. C should see it (relayed via B).
        let payload = vec![0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8];
        for _ in 0..40 {
            a.broadcast(payload.clone());
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some(got) = c.try_recv() {
                assert_eq!(got, payload, "C must receive A's payload via relay");
                return;
            }
        }
        panic!("C never received A's broadcast through B's relay");
    }

    /// Echo suppression: A broadcasts, B relays. A's reader on the
    /// A↔B socket must drop the echo (originator dedup).
    #[tokio::test]
    async fn gossip_relay_suppresses_echo_to_originator() {
        let a = TcpBftTransport::new("127.0.0.1:0".parse().unwrap(), vec![])
            .await
            .unwrap();
        let a_addr = a.local_addr();
        let b = TcpBftTransport::new("127.0.0.1:0".parse().unwrap(), vec![a_addr])
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;

        let payload = vec![0xde, 0xad, 0xbe, 0xef];
        a.broadcast(payload.clone());

        // Give B time to receive + relay back.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // B sees the payload exactly once.
        let mut b_count = 0;
        while b.try_recv().is_some() {
            b_count += 1;
        }
        assert_eq!(b_count, 1, "B must receive the payload exactly once");

        // A must NOT receive its own echo: B's relay will fan back via
        // the broadcast channel to its A↔B writer, sending it to A.
        // A's reader hashes it, finds it in its own dedup (pre-seeded
        // by `broadcast()`), and drops it.
        let a_got = a.try_recv();
        assert!(
            a_got.is_none(),
            "A must NOT see its own broadcast echo (got {a_got:?})"
        );
    }

    /// Outbound-only over the Noise XX path — same shape, encrypted.
    #[tokio::test]
    async fn outbound_only_encrypted_round_trip() {
        let a = TcpBftTransport::new_encrypted("127.0.0.1:0".parse().unwrap(), vec![])
            .await
            .unwrap();
        let a_addr = a.local_addr();
        let b = TcpBftTransport::new_outbound_only_encrypted(vec![a_addr])
            .await
            .unwrap();
        assert_eq!(
            b.local_addr().ip(),
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        );
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            b.broadcast(vec![0xc0, 0xff, 0xee]);
            if a.try_recv().is_some() {
                return;
            }
        }
        panic!("outbound-only encrypted didn't deliver");
    }
}
