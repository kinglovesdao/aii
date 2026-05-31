//! Startup peer discovery for `aiid`.
//!
//! This is the node-level glue over `aii-net-p2p`'s devp2p Discovery
//! v4 primitives. It runs before the BFT transport is constructed:
//! ping configured UDP discovery seeds, ask them for neighbours, then
//! feed any returned TCP endpoints into the existing peer-cache merge.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aii_crypto::keccak256;
use aii_crypto::secp::SecretKey;
use aii_net_p2p::discovery::{
    expiration_in, Endpoint, FindNode, Neighbours, Packet, Ping, Pong, UdpDiscovery,
    DISCOVERY_VERSION,
};
use tokio::task::JoinHandle;

/// Filename inside the node data directory for the persistent discovery key.
pub const DISCOVERY_KEY_FILENAME: &str = "discovery.key";
/// Environment variable for comma-separated Discovery v4 seed addresses.
pub const DISCOVERY_SEEDS_ENV: &str = "AII_DISCOVERY_SEEDS";
/// Mainnet DNS seed. Resolves to one or more UDP Discovery v4 bootnodes.
pub const MAINNET_DISCOVERY_SEEDS: &[&str] = &["bootnodes.aii.network:30310"];
/// Live AII testnet Discovery v4 bootnodes.
pub const TESTNET_DISCOVERY_SEEDS: &[&str] = &["8.211.135.234:30310", "106.14.223.128:30310"];

/// Shared peer view served to Discovery v4 `FindNode` callers.
pub type SharedDiscoveryPeers = Arc<RwLock<Vec<SocketAddr>>>;

/// Peer addresses learned from one Discovery v4 query window.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryUpdate {
    /// BFT TCP endpoints that should be dialed by the consensus transport.
    pub bft_peers: Vec<SocketAddr>,
    /// UDP Discovery v4 endpoints that should be queried in later refreshes.
    pub discovery_peers: Vec<SocketAddr>,
    /// UDP endpoint a responder observed for this node, learned from
    /// `Pong.to`. This is useful when the node bound `0.0.0.0` or sits
    /// behind NAT and did not provide an explicit advertised address.
    pub observed_discovery: Option<SocketAddr>,
}

/// Operator-provided public addresses to advertise through Discovery v4.
///
/// Discovery v4 endpoints carry one IP with separate UDP and TCP ports.
/// `bft` wins for the advertised IP because discovered peers ultimately
/// dial the returned TCP endpoint for consensus gossip.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiscoveryAdvertisement {
    /// Public UDP Discovery v4 address, when it differs from the bind address.
    pub discovery: Option<SocketAddr>,
    /// Public BFT TCP address, when it differs from the bind address.
    pub bft: Option<SocketAddr>,
}

/// Default Discovery v4 seed specs for the selected network.
#[must_use]
pub const fn default_seed_specs(testnet: bool) -> &'static [&'static str] {
    if testnet {
        TESTNET_DISCOVERY_SEEDS
    } else {
        MAINNET_DISCOVERY_SEEDS
    }
}

/// Resolve `<data_dir>/discovery.key`.
#[must_use]
pub fn key_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DISCOVERY_KEY_FILENAME)
}

/// Load or create the node's Discovery v4 secp256k1 identity.
///
/// The key is independent from validator BLS/VRF keys. Its only job is
/// to give discovery packets a stable peer identity across restarts.
///
/// # Errors
/// Returns I/O errors or malformed-key parse errors.
pub fn load_or_create_key(path: &Path) -> io::Result<SecretKey> {
    match fs::read_to_string(path) {
        Ok(s) => {
            let trimmed = s.trim();
            let body = trimmed.strip_prefix("0x").unwrap_or(trimmed);
            let raw =
                hex::decode(body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let arr: [u8; 32] = raw.try_into().map_err(|v: Vec<u8>| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("discovery key: expected 32 bytes, got {}", v.len()),
                )
            })?;
            SecretKey::from_bytes(&arr).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let sk = generate_local_key(path);
            let tmp = path.with_extension("key.tmp");
            fs::write(&tmp, format!("0x{}\n", hex::encode(sk.to_bytes())))?;
            fs::rename(&tmp, path)?;
            Ok(sk)
        }
        Err(e) => Err(e),
    }
}

/// Build an ordered, deduplicated seed-spec list from CLI, env, and
/// network defaults.
///
/// CLI values are first so operator-provided seed nodes are tried
/// before public defaults. The env var lets release bundles and mobile
/// shells inject region-local seeds without changing argv.
#[must_use]
pub fn seed_specs(
    cli_seeds: &[SocketAddr],
    env_value: Option<&str>,
    default_specs: &[&str],
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for seed in cli_seeds {
        push_seed_spec(&mut out, &mut seen, &seed.to_string());
    }
    if let Some(env) = env_value {
        for part in env.split(',') {
            push_seed_spec(&mut out, &mut seen, part);
        }
    }
    for seed in default_specs {
        push_seed_spec(&mut out, &mut seen, seed);
    }
    out
}

fn push_seed_spec(out: &mut Vec<String>, seen: &mut BTreeSet<String>, candidate: &str) {
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return;
    }
    if seen.insert(trimmed.to_string()) {
        out.push(trimmed.to_string());
    }
}

/// Resolve seed specs into socket addresses, tolerating malformed or
/// currently-unresolvable entries.
#[must_use]
pub fn resolve_seed_specs(specs: &[String]) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for spec in specs {
        if let Ok(addr) = spec.parse::<SocketAddr>() {
            if seen.insert(addr) {
                out.push(addr);
            }
            continue;
        }
        if let Ok(addrs) = spec.to_socket_addrs() {
            for addr in addrs {
                if seen.insert(addr) {
                    out.push(addr);
                }
            }
        }
    }
    out
}

fn generate_local_key(path: &Path) -> SecretKey {
    let base = format!(
        "{}:{}:{}",
        path.display(),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    );
    for counter in 0u64.. {
        let mut input = base.as_bytes().to_vec();
        input.extend_from_slice(&counter.to_be_bytes());
        let bytes = *keccak256(&input).as_bytes();
        if let Ok(sk) = SecretKey::from_bytes(&bytes) {
            return sk;
        }
    }
    unreachable!("secp256k1 key generation loop should eventually find a valid scalar")
}

/// Convert discovery endpoints into BFT TCP peer addresses.
#[must_use]
pub fn endpoints_to_peers(nodes: &[Endpoint]) -> Vec<SocketAddr> {
    let mut out = BTreeSet::new();
    for n in nodes {
        if let Some(peer) = endpoint_to_peer(n) {
            out.insert(peer);
        }
    }
    out.into_iter().collect()
}

/// Convert discovery endpoints into UDP Discovery v4 peer addresses.
#[must_use]
pub fn endpoints_to_discovery_peers(nodes: &[Endpoint]) -> Vec<SocketAddr> {
    let mut out = BTreeSet::new();
    for n in nodes {
        if let Some(peer) = endpoint_to_discovery_peer(n) {
            out.insert(peer);
        }
    }
    out.into_iter().collect()
}

/// Construct a shared discovery peer view.
#[must_use]
pub fn shared_peers(peers: &[SocketAddr]) -> SharedDiscoveryPeers {
    Arc::new(RwLock::new(normalize_peers(peers)))
}

/// Replace the peers served by the long-running discovery responder.
pub fn set_shared_peers(peers: &SharedDiscoveryPeers, next: &[SocketAddr]) {
    if let Ok(mut guard) = peers.write() {
        *guard = normalize_peers(next);
    }
}

/// Add one peer to the responder view.
///
/// Returns `true` when the peer was newly inserted. Unspecified
/// addresses and zero ports are ignored because advertising them
/// poisons later discovery responses with undialable endpoints.
pub fn insert_shared_peer(peers: &SharedDiscoveryPeers, peer: SocketAddr) -> bool {
    if !peer_is_advertisable(peer) {
        return false;
    }
    let Ok(mut guard) = peers.write() else {
        return false;
    };
    match guard.binary_search(&peer) {
        Ok(_) => false,
        Err(idx) => {
            guard.insert(idx, peer);
            true
        }
    }
}

const fn peer_to_endpoint(peer: SocketAddr) -> Endpoint {
    Endpoint {
        ip: peer.ip(),
        udp_port: peer.port(),
        tcp_port: peer.port(),
    }
}

fn endpoint_to_peer(endpoint: &Endpoint) -> Option<SocketAddr> {
    let peer = SocketAddr::new(endpoint.ip, endpoint.tcp_port);
    peer_is_advertisable(peer).then_some(peer)
}

fn endpoint_to_discovery_peer(endpoint: &Endpoint) -> Option<SocketAddr> {
    let peer = SocketAddr::new(endpoint.ip, endpoint.udp_port);
    peer_is_advertisable(peer).then_some(peer)
}

const fn observed_pong_endpoint(src: SocketAddr, from: &Endpoint) -> Endpoint {
    Endpoint {
        ip: src.ip(),
        udp_port: src.port(),
        tcp_port: from.tcp_port,
    }
}

fn normalize_peers(peers: &[SocketAddr]) -> Vec<SocketAddr> {
    peers
        .iter()
        .copied()
        .filter(|peer| peer_is_advertisable(*peer))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

const fn peer_is_advertisable(peer: SocketAddr) -> bool {
    !peer.ip().is_unspecified() && peer.port() != 0
}

const fn endpoint_is_advertisable(endpoint: &Endpoint) -> bool {
    !endpoint.ip.is_unspecified() && endpoint.tcp_port != 0
}

/// Build the endpoint this node advertises to discovery peers.
///
/// Without explicit advertisement, this falls back to the actual UDP
/// bind address and the configured BFT listener. Startup code can
/// replace missing fields with a seed-observed public endpoint before
/// this is used for the long-running responder.
#[must_use]
pub fn advertised_endpoint(
    discovery_bound: SocketAddr,
    bft_listen: SocketAddr,
    advertise: DiscoveryAdvertisement,
) -> Endpoint {
    let ip = advertise
        .bft
        .map(|addr| addr.ip())
        .or_else(|| advertise.discovery.map(|addr| addr.ip()))
        .unwrap_or_else(|| {
            if bft_listen.ip().is_unspecified() {
                discovery_bound.ip()
            } else {
                bft_listen.ip()
            }
        });
    let udp_port = advertise
        .discovery
        .map_or_else(|| discovery_bound.port(), |addr| addr.port());
    let tcp_port = advertise
        .bft
        .map_or_else(|| bft_listen.port(), |addr| addr.port());
    Endpoint {
        ip,
        udp_port,
        tcp_port,
    }
}

/// Fill missing advertised addresses from a Discovery v4 responder's
/// observed UDP endpoint.
///
/// This is not a replacement for explicit port-forward/NAT config: it
/// can learn the public UDP source address and can infer the BFT public
/// IP, but it cannot prove that the inferred TCP port is reachable. It
/// is still a better default than advertising wildcard/private bind
/// addresses on public nodes.
#[must_use]
pub fn advertisement_with_observed_endpoint(
    advertise: DiscoveryAdvertisement,
    bft_listen: SocketAddr,
    observed_discovery: Option<SocketAddr>,
    advertise_bft: bool,
) -> DiscoveryAdvertisement {
    let Some(observed) = observed_discovery.filter(|addr| peer_is_advertisable(*addr)) else {
        return advertise;
    };
    let discovery = advertise.discovery.or(Some(observed));
    let bft = if advertise.bft.is_some()
        || !advertise_bft
        || bft_listen.port() == 0
        || bft_listen.ip().is_loopback()
    {
        advertise.bft
    } else {
        Some(SocketAddr::new(observed.ip(), bft_listen.port()))
    };
    DiscoveryAdvertisement { discovery, bft }
}

/// Bind a long-running Discovery v4 responder.
///
/// The responder makes this node discoverable outside the short
/// `discover_once` query windows: it replies to `Ping` with `Pong` and
/// to `FindNode` with the current peer cache. The peer view is shared
/// with the BFT loop so newly learned peers are advertised without
/// restarting the UDP socket.
///
/// # Errors
/// Returns bind or packet-signing errors from the discovery layer.
pub async fn spawn_responder(
    listen: SocketAddr,
    secret: SecretKey,
    bft_listen: SocketAddr,
    advertise: DiscoveryAdvertisement,
    peers: SharedDiscoveryPeers,
) -> Result<(SocketAddr, JoinHandle<()>), Box<dyn std::error::Error + Send + Sync>> {
    let driver = UdpDiscovery::bind(listen, secret).await?;
    let bound_addr = driver.local_addr();
    let local = advertised_endpoint(bound_addr, bft_listen, advertise);
    let local = endpoint_is_advertisable(&local).then_some(local);
    let handle = tokio::spawn(async move {
        loop {
            let Ok((decoded, src)) = driver.recv(Duration::from_secs(1)).await else {
                continue;
            };
            match decoded.packet {
                Packet::Ping(p) => {
                    if let Some(peer) = endpoint_to_peer(&p.from) {
                        let _ = insert_shared_peer(&peers, peer);
                    }
                    let observed = observed_pong_endpoint(src, &p.from);
                    let pong = Packet::Pong(Pong {
                        to: observed,
                        ping_hash: decoded.packet_hash,
                        expiration: expiration_in(60),
                    });
                    let _ = driver.send(src, &pong).await;
                }
                Packet::FindNode(_) => {
                    let mut nodes = local.clone().into_iter().collect::<Vec<_>>();
                    nodes.extend(peers.read().map_or_else(
                        |_| Vec::new(),
                        |guard| {
                            guard
                                .iter()
                                .copied()
                                .filter(|peer| peer_is_advertisable(*peer))
                                .map(peer_to_endpoint)
                                .collect()
                        },
                    ));
                    let neighbours = Neighbours {
                        nodes,
                        expiration: expiration_in(60),
                    };
                    let _ = driver.send(src, &Packet::Neighbours(neighbours)).await;
                }
                Packet::Neighbours(_) | Packet::Pong(_) => {}
            }
        }
    });
    Ok((bound_addr, handle))
}

/// Query discovery `seeds` once and return discovered BFT + Discovery peers.
///
/// # Errors
/// Returns socket, packet, or crypto errors from the discovery layer.
pub async fn discover_once_full(
    listen: SocketAddr,
    secret: SecretKey,
    seeds: &[SocketAddr],
    bft_listen: SocketAddr,
    advertise: DiscoveryAdvertisement,
    known_peers: &[SocketAddr],
    timeout: Duration,
) -> Result<DiscoveryUpdate, Box<dyn std::error::Error + Send + Sync>> {
    if seeds.is_empty() {
        return Ok(DiscoveryUpdate::default());
    }

    let driver = UdpDiscovery::bind(listen, secret).await?;
    let local = advertised_endpoint(driver.local_addr(), bft_listen, advertise);
    let target = keccak256(&driver.local_addr().to_string().into_bytes());
    let neighbours = Neighbours {
        nodes: known_peers
            .iter()
            .copied()
            .filter(|peer| peer_is_advertisable(*peer))
            .map(peer_to_endpoint)
            .collect(),
        expiration: expiration_in(60),
    };

    for seed in seeds {
        let seed_ep = Endpoint {
            ip: seed.ip(),
            udp_port: seed.port(),
            tcp_port: 0,
        };
        let ping = Packet::Ping(Ping {
            version: DISCOVERY_VERSION,
            from: local.clone(),
            to: seed_ep,
            expiration: expiration_in(60),
        });
        let _ = driver.send(*seed, &ping).await;
        let find = Packet::FindNode(FindNode {
            target,
            expiration: expiration_in(60),
        });
        let _ = driver.send(*seed, &find).await;
    }

    let deadline = tokio::time::Instant::now() + timeout;
    let mut found_bft = BTreeSet::new();
    let mut found_discovery = BTreeSet::new();
    let mut observed_discovery = None;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let slice = remaining.min(Duration::from_millis(100));
        let Ok((decoded, src)) = driver.recv(slice).await else {
            continue;
        };
        match decoded.packet {
            Packet::Ping(p) => {
                let observed = observed_pong_endpoint(src, &p.from);
                let pong = Packet::Pong(Pong {
                    to: observed,
                    ping_hash: decoded.packet_hash,
                    expiration: expiration_in(60),
                });
                let _ = driver.send(src, &pong).await;
            }
            Packet::FindNode(_) => {
                let _ = driver
                    .send(src, &Packet::Neighbours(neighbours.clone()))
                    .await;
            }
            Packet::Neighbours(n) => {
                found_bft.extend(endpoints_to_peers(&n.nodes));
                found_discovery.extend(endpoints_to_discovery_peers(&n.nodes));
            }
            Packet::Pong(p) => {
                if observed_discovery.is_none() {
                    observed_discovery = endpoint_to_discovery_peer(&p.to);
                }
            }
        }
    }

    Ok(DiscoveryUpdate {
        bft_peers: found_bft.into_iter().collect(),
        discovery_peers: found_discovery.into_iter().collect(),
        observed_discovery,
    })
}

/// Query discovery `seeds` once and return newly-discovered BFT TCP peers.
///
/// # Errors
/// Returns socket, packet, or crypto errors from the discovery layer.
pub async fn discover_once(
    listen: SocketAddr,
    secret: SecretKey,
    seeds: &[SocketAddr],
    bft_listen: SocketAddr,
    advertise: DiscoveryAdvertisement,
    known_peers: &[SocketAddr],
    timeout: Duration,
) -> Result<Vec<SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
    discover_once_full(
        listen,
        secret,
        seeds,
        bft_listen,
        advertise,
        known_peers,
        timeout,
    )
    .await
    .map(|update| update.bft_peers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_secret(byte: u8) -> SecretKey {
        let mut bytes = [0u8; 32];
        bytes[31] = byte;
        SecretKey::from_bytes(&bytes).unwrap()
    }

    #[test]
    fn endpoints_to_peers_filters_zero_tcp_and_dedupes() {
        let a = Endpoint::loopback(30310, 30311);
        let b = Endpoint::loopback(30312, 0);
        let wildcard = Endpoint {
            ip: "0.0.0.0".parse().unwrap(),
            udp_port: 30310,
            tcp_port: 30311,
        };
        let peers = endpoints_to_peers(&[a.clone(), a, b, wildcard]);
        assert_eq!(peers, vec!["127.0.0.1:30311".parse().unwrap()]);
    }

    #[test]
    fn endpoints_to_discovery_peers_preserves_udp_port() {
        let node = Endpoint {
            ip: "127.0.0.1".parse().unwrap(),
            udp_port: 30310,
            tcp_port: 30311,
        };
        let zero_udp = Endpoint {
            ip: "127.0.0.1".parse().unwrap(),
            udp_port: 0,
            tcp_port: 30312,
        };
        let peers = endpoints_to_discovery_peers(&[node, zero_udp]);
        assert_eq!(peers, vec!["127.0.0.1:30310".parse().unwrap()]);
    }

    #[test]
    fn load_or_create_key_persists_stable_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = key_path(dir.path());
        let a = load_or_create_key(&path).unwrap();
        let b = load_or_create_key(&path).unwrap();
        assert_eq!(a.to_bytes(), b.to_bytes());
    }

    #[test]
    fn seed_specs_merge_cli_env_defaults_in_order() {
        let cli = vec!["127.0.0.1:30310".parse().unwrap()];
        let specs = seed_specs(
            &cli,
            Some("127.0.0.2:30310, 127.0.0.1:30310"),
            &["127.0.0.3:30310"],
        );
        assert_eq!(
            specs,
            vec!["127.0.0.1:30310", "127.0.0.2:30310", "127.0.0.3:30310"]
        );
    }

    #[test]
    fn resolve_seed_specs_skips_invalid_and_dedupes() {
        let specs = vec![
            "not a seed".to_string(),
            "127.0.0.1:30310".to_string(),
            "127.0.0.1:30310".to_string(),
        ];
        let addrs = resolve_seed_specs(&specs);
        assert_eq!(addrs, vec!["127.0.0.1:30310".parse().unwrap()]);
    }

    #[test]
    fn testnet_has_builtin_discovery_seeds() {
        assert!(!default_seed_specs(true).is_empty());
    }

    #[test]
    fn shared_peers_filters_and_orders_advertisable_peers() {
        let shared = shared_peers(&[
            "127.0.0.1:30312".parse().unwrap(),
            "0.0.0.0:30311".parse().unwrap(),
            "127.0.0.1:30310".parse().unwrap(),
            "127.0.0.1:30312".parse().unwrap(),
        ]);
        assert_eq!(
            shared.read().unwrap().as_slice(),
            &[
                "127.0.0.1:30310".parse::<SocketAddr>().unwrap(),
                "127.0.0.1:30312".parse().unwrap(),
            ],
        );
    }

    #[test]
    fn insert_shared_peer_dedupes_and_rejects_wildcard() {
        let shared = shared_peers(&[]);
        assert!(!insert_shared_peer(
            &shared,
            "0.0.0.0:30311".parse().unwrap(),
        ));
        assert!(insert_shared_peer(
            &shared,
            "127.0.0.1:30311".parse().unwrap(),
        ));
        assert!(!insert_shared_peer(
            &shared,
            "127.0.0.1:30311".parse().unwrap(),
        ));
        assert_eq!(
            shared.read().unwrap().as_slice(),
            &["127.0.0.1:30311".parse::<SocketAddr>().unwrap()],
        );
    }

    #[test]
    fn advertised_endpoint_uses_public_bft_ip_and_ports() {
        let endpoint = advertised_endpoint(
            "0.0.0.0:30310".parse().unwrap(),
            "0.0.0.0:30311".parse().unwrap(),
            DiscoveryAdvertisement {
                discovery: Some("198.51.100.7:31310".parse().unwrap()),
                bft: Some("203.0.113.9:31311".parse().unwrap()),
            },
        );
        assert_eq!(endpoint.ip.to_string(), "203.0.113.9");
        assert_eq!(endpoint.udp_port, 31310);
        assert_eq!(endpoint.tcp_port, 31311);
    }

    #[test]
    fn advertised_endpoint_falls_back_to_bound_addresses() {
        let endpoint = advertised_endpoint(
            "127.0.0.1:30310".parse().unwrap(),
            "127.0.0.1:30311".parse().unwrap(),
            DiscoveryAdvertisement::default(),
        );
        assert_eq!(endpoint.ip.to_string(), "127.0.0.1");
        assert_eq!(endpoint.udp_port, 30310);
        assert_eq!(endpoint.tcp_port, 30311);
    }

    #[test]
    fn observed_endpoint_fills_missing_advertisement() {
        let advertise = advertisement_with_observed_endpoint(
            DiscoveryAdvertisement::default(),
            "0.0.0.0:30311".parse().unwrap(),
            Some("198.51.100.44:41000".parse().unwrap()),
            true,
        );
        assert_eq!(
            advertise.discovery,
            Some("198.51.100.44:41000".parse().unwrap()),
        );
        assert_eq!(advertise.bft, Some("198.51.100.44:30311".parse().unwrap()));
    }

    #[test]
    fn observed_endpoint_respects_manual_and_outbound_only() {
        let manual = DiscoveryAdvertisement {
            discovery: Some("203.0.113.10:30310".parse().unwrap()),
            bft: Some("203.0.113.10:30311".parse().unwrap()),
        };
        assert_eq!(
            advertisement_with_observed_endpoint(
                manual,
                "0.0.0.0:30311".parse().unwrap(),
                Some("198.51.100.44:41000".parse().unwrap()),
                true,
            ),
            manual,
        );

        let outbound_only = advertisement_with_observed_endpoint(
            DiscoveryAdvertisement::default(),
            "0.0.0.0:30311".parse().unwrap(),
            Some("198.51.100.44:41000".parse().unwrap()),
            false,
        );
        assert_eq!(
            outbound_only.discovery,
            Some("198.51.100.44:41000".parse().unwrap()),
        );
        assert_eq!(outbound_only.bft, None);
    }

    #[tokio::test]
    async fn discover_once_imports_neighbours_from_seed() {
        let seed = UdpDiscovery::bind("127.0.0.1:0".parse().unwrap(), fixed_secret(1))
            .await
            .unwrap();
        let seed_addr = seed.local_addr();
        let advertised_bft = "127.0.0.1:30311".parse::<SocketAddr>().unwrap();
        let advertised_discovery = "127.0.0.1:30310".parse::<SocketAddr>().unwrap();

        let responder = tokio::spawn(async move {
            for _ in 0..2 {
                let Ok((decoded, src)) = seed.recv(Duration::from_secs(2)).await else {
                    continue;
                };
                match decoded.packet {
                    Packet::Ping(p) => {
                        let pong = Packet::Pong(Pong {
                            to: p.from,
                            ping_hash: decoded.packet_hash,
                            expiration: expiration_in(60),
                        });
                        let _ = seed.send(src, &pong).await;
                    }
                    Packet::FindNode(_) => {
                        let nodes = vec![Endpoint {
                            ip: advertised_bft.ip(),
                            udp_port: advertised_discovery.port(),
                            tcp_port: advertised_bft.port(),
                        }];
                        let _ = seed
                            .send(
                                src,
                                &Packet::Neighbours(Neighbours {
                                    nodes,
                                    expiration: expiration_in(60),
                                }),
                            )
                            .await;
                    }
                    Packet::Pong(_) | Packet::Neighbours(_) => {}
                }
            }
        });

        let update = discover_once_full(
            "127.0.0.1:0".parse().unwrap(),
            fixed_secret(2),
            &[seed_addr],
            "127.0.0.1:30312".parse().unwrap(),
            DiscoveryAdvertisement::default(),
            &[],
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        responder.await.unwrap();
        assert_eq!(update.bft_peers, vec![advertised_bft]);
        assert_eq!(update.discovery_peers, vec![advertised_discovery]);
    }

    #[tokio::test]
    async fn discover_once_reports_observed_endpoint_from_pong() {
        let seed = UdpDiscovery::bind("127.0.0.1:0".parse().unwrap(), fixed_secret(16))
            .await
            .unwrap();
        let seed_addr = seed.local_addr();
        let observed = "198.51.100.77:42000".parse::<SocketAddr>().unwrap();

        let responder = tokio::spawn(async move {
            for _ in 0..2 {
                let Ok((decoded, src)) = seed.recv(Duration::from_secs(2)).await else {
                    continue;
                };
                match decoded.packet {
                    Packet::Ping(p) => {
                        let pong = Packet::Pong(Pong {
                            to: Endpoint {
                                ip: observed.ip(),
                                udp_port: observed.port(),
                                tcp_port: p.from.tcp_port,
                            },
                            ping_hash: decoded.packet_hash,
                            expiration: expiration_in(60),
                        });
                        let _ = seed.send(src, &pong).await;
                    }
                    Packet::FindNode(_) => {
                        let _ = seed
                            .send(
                                src,
                                &Packet::Neighbours(Neighbours {
                                    nodes: Vec::new(),
                                    expiration: expiration_in(60),
                                }),
                            )
                            .await;
                    }
                    Packet::Pong(_) | Packet::Neighbours(_) => {}
                }
            }
        });

        let update = discover_once_full(
            "127.0.0.1:0".parse().unwrap(),
            fixed_secret(17),
            &[seed_addr],
            "0.0.0.0:30311".parse().unwrap(),
            DiscoveryAdvertisement::default(),
            &[],
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        responder.await.unwrap();
        assert_eq!(update.observed_discovery, Some(observed));
    }

    #[tokio::test]
    async fn discover_once_serves_only_advertisable_known_peers() {
        let seed = UdpDiscovery::bind("127.0.0.1:0".parse().unwrap(), fixed_secret(14))
            .await
            .unwrap();
        let seed_addr = seed.local_addr();
        let expected = "127.0.0.1:30331".parse::<SocketAddr>().unwrap();

        let responder = tokio::spawn(async move {
            let mut query_src = None;
            for _ in 0..5 {
                let Ok((decoded, src)) = seed.recv(Duration::from_secs(2)).await else {
                    continue;
                };
                match decoded.packet {
                    Packet::Ping(p) => {
                        query_src = Some(src);
                        let pong = Packet::Pong(Pong {
                            to: p.from,
                            ping_hash: decoded.packet_hash,
                            expiration: expiration_in(60),
                        });
                        let _ = seed.send(src, &pong).await;
                        let find = Packet::FindNode(FindNode {
                            target: keccak256(b"aii-known-peer-filter-test"),
                            expiration: expiration_in(60),
                        });
                        let _ = seed.send(src, &find).await;
                    }
                    Packet::Neighbours(n) if Some(src) == query_src => {
                        return endpoints_to_peers(&n.nodes);
                    }
                    Packet::FindNode(_) | Packet::Pong(_) | Packet::Neighbours(_) => {}
                }
            }
            Vec::new()
        });

        let _ = discover_once(
            "127.0.0.1:0".parse().unwrap(),
            fixed_secret(15),
            &[seed_addr],
            "127.0.0.1:30332".parse().unwrap(),
            DiscoveryAdvertisement::default(),
            &[
                "0.0.0.0:30330".parse().unwrap(),
                expected,
                "127.0.0.1:0".parse().unwrap(),
            ],
            Duration::from_millis(500),
        )
        .await
        .unwrap();

        assert_eq!(responder.await.unwrap(), vec![expected]);
    }

    #[tokio::test]
    async fn responder_serves_find_node_until_aborted() {
        let advertised_tcp = 30311;
        let known_peer = "127.0.0.1:30312".parse::<SocketAddr>().unwrap();
        let shared = shared_peers(&[known_peer]);
        let (responder_addr, handle) = spawn_responder(
            "127.0.0.1:0".parse().unwrap(),
            fixed_secret(3),
            "127.0.0.1:30311".parse().unwrap(),
            DiscoveryAdvertisement::default(),
            shared.clone(),
        )
        .await
        .unwrap();

        let client = UdpDiscovery::bind("127.0.0.1:0".parse().unwrap(), fixed_secret(6))
            .await
            .unwrap();
        let find = Packet::FindNode(FindNode {
            target: keccak256(b"aii-discovery-test"),
            expiration: expiration_in(60),
        });
        client.send(responder_addr, &find).await.unwrap();

        let mut endpoints = Vec::new();
        for _ in 0..5 {
            let Ok((decoded, _src)) = client.recv(Duration::from_secs(1)).await else {
                continue;
            };
            if let Packet::Neighbours(n) = decoded.packet {
                endpoints = n.nodes;
                break;
            }
        }
        handle.abort();

        let peers = endpoints_to_peers(&endpoints);
        assert!(
            peers.contains(&known_peer),
            "responder should advertise shared peers",
        );
        assert!(
            peers.iter().any(|peer| peer.port() == advertised_tcp),
            "responder should advertise its own BFT TCP port",
        );
    }

    #[tokio::test]
    async fn responder_serves_configured_public_endpoint() {
        let advertised_bft = "203.0.113.20:31311".parse::<SocketAddr>().unwrap();
        let shared = shared_peers(&[]);
        let (responder_addr, handle) = spawn_responder(
            "127.0.0.1:0".parse().unwrap(),
            fixed_secret(7),
            "0.0.0.0:30311".parse().unwrap(),
            DiscoveryAdvertisement {
                discovery: Some("203.0.113.20:31310".parse().unwrap()),
                bft: Some(advertised_bft),
            },
            shared,
        )
        .await
        .unwrap();

        let client = UdpDiscovery::bind("127.0.0.1:0".parse().unwrap(), fixed_secret(8))
            .await
            .unwrap();
        client
            .send(
                responder_addr,
                &Packet::FindNode(FindNode {
                    target: keccak256(b"aii-public-advertise-test"),
                    expiration: expiration_in(60),
                }),
            )
            .await
            .unwrap();

        let mut endpoints = Vec::new();
        for _ in 0..5 {
            let Ok((decoded, _src)) = client.recv(Duration::from_secs(1)).await else {
                continue;
            };
            if let Packet::Neighbours(n) = decoded.packet {
                endpoints = n.nodes;
                break;
            }
        }
        handle.abort();

        let peers = endpoints_to_peers(&endpoints);
        assert_eq!(peers, vec![advertised_bft]);
        assert!(
            endpoints
                .iter()
                .any(|endpoint| endpoint.udp_port == 31310 && endpoint.tcp_port == 31311),
            "responder should advertise configured public Discovery and BFT ports",
        );
    }

    #[tokio::test]
    async fn responder_does_not_advertise_wildcard_self_or_peers() {
        let shared = shared_peers(&[
            "0.0.0.0:30311".parse().unwrap(),
            "127.0.0.1:30312".parse().unwrap(),
        ]);
        let (responder_addr, handle) = spawn_responder(
            "0.0.0.0:0".parse().unwrap(),
            fixed_secret(9),
            "0.0.0.0:30311".parse().unwrap(),
            DiscoveryAdvertisement::default(),
            shared,
        )
        .await
        .unwrap();

        let client = UdpDiscovery::bind("127.0.0.1:0".parse().unwrap(), fixed_secret(10))
            .await
            .unwrap();
        client
            .send(
                responder_addr,
                &Packet::FindNode(FindNode {
                    target: keccak256(b"aii-no-wildcard-test"),
                    expiration: expiration_in(60),
                }),
            )
            .await
            .unwrap();

        let mut endpoints = Vec::new();
        for _ in 0..5 {
            let Ok((decoded, _src)) = client.recv(Duration::from_secs(1)).await else {
                continue;
            };
            if let Packet::Neighbours(n) = decoded.packet {
                endpoints = n.nodes;
                break;
            }
        }
        handle.abort();

        let peers = endpoints_to_peers(&endpoints);
        assert_eq!(peers, vec!["127.0.0.1:30312".parse().unwrap()]);
        assert!(
            endpoints
                .iter()
                .all(|endpoint| !endpoint.ip.is_unspecified()),
            "responder must not advertise wildcard addresses",
        );
    }

    #[tokio::test]
    async fn responder_learns_ping_sender_and_serves_it_to_later_queries() {
        let learned_peer = "127.0.0.1:30321".parse::<SocketAddr>().unwrap();
        let shared = shared_peers(&[]);
        let (responder_addr, handle) = spawn_responder(
            "127.0.0.1:0".parse().unwrap(),
            fixed_secret(11),
            "127.0.0.1:30311".parse().unwrap(),
            DiscoveryAdvertisement::default(),
            shared,
        )
        .await
        .unwrap();

        let joiner = UdpDiscovery::bind("127.0.0.1:0".parse().unwrap(), fixed_secret(12))
            .await
            .unwrap();
        joiner
            .send(
                responder_addr,
                &Packet::Ping(Ping {
                    version: DISCOVERY_VERSION,
                    from: Endpoint {
                        ip: learned_peer.ip(),
                        udp_port: 30320,
                        tcp_port: learned_peer.port(),
                    },
                    to: Endpoint {
                        ip: responder_addr.ip(),
                        udp_port: responder_addr.port(),
                        tcp_port: 0,
                    },
                    expiration: expiration_in(60),
                }),
            )
            .await
            .unwrap();

        for _ in 0..5 {
            let Ok((decoded, _src)) = joiner.recv(Duration::from_secs(1)).await else {
                continue;
            };
            if matches!(decoded.packet, Packet::Pong(_)) {
                break;
            }
        }

        let seeker = UdpDiscovery::bind("127.0.0.1:0".parse().unwrap(), fixed_secret(13))
            .await
            .unwrap();
        seeker
            .send(
                responder_addr,
                &Packet::FindNode(FindNode {
                    target: keccak256(b"aii-learned-peer-test"),
                    expiration: expiration_in(60),
                }),
            )
            .await
            .unwrap();

        let mut endpoints = Vec::new();
        for _ in 0..5 {
            let Ok((decoded, _src)) = seeker.recv(Duration::from_secs(1)).await else {
                continue;
            };
            if let Packet::Neighbours(n) = decoded.packet {
                endpoints = n.nodes;
                break;
            }
        }
        handle.abort();

        let peers = endpoints_to_peers(&endpoints);
        assert!(
            peers.contains(&learned_peer),
            "responder should serve a peer learned from an earlier Ping",
        );
    }
}
