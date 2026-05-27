//! devp2p **Discovery v4** transport.
//!
//! Wire format per the Ethereum spec
//! (<https://github.com/ethereum/devp2p/blob/master/discv4.md>):
//!
//! ```text
//! packet = packet-header || packet-data
//! packet-header = hash || signature || packet-type
//! hash         = keccak256(signature || packet-type || packet-data)  (32 B)
//! signature    = sign(keccak256(packet-type || packet-data))         (65 B)
//! packet-type  = 0x01..=0x04                                          (1 B)
//! packet-data  = RLP-encoded body
//! ```
//!
//! v0.0.17 implements **Ping (0x01)** and **Pong (0x02)** plus a
//! minimal [`UdpDiscovery`] driver that signs / verifies packets and
//! drives the request-response loop. `FindNode` (0x03) and
//! `Neighbours` (0x04) land in v0.0.18 along with a Kademlia routing
//! table.

use aii_crypto::keccak::keccak256;
use aii_crypto::secp::{self, PublicKey, SecretKey, Signature};
use aii_types::{Address, H256};
use alloy_rlp::{Decodable, Encodable, Header as RlpHeader};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::net::UdpSocket;

/// Discovery v4 packet types.
pub const TYPE_PING: u8 = 0x01;
/// Pong.
pub const TYPE_PONG: u8 = 0x02;
/// FindNode (0x03) — ask peer to return the K closest nodes to `target`.
pub const TYPE_FIND_NODE: u8 = 0x03;
/// Neighbours (0x04) — reply listing up to K peer endpoints.
pub const TYPE_NEIGHBOURS: u8 = 0x04;

/// devp2p Discovery v4 protocol version.
pub const DISCOVERY_VERSION: u32 = 4;

/// Maximum UDP packet size we send / accept (devp2p spec: ≤ 1280 B).
pub const MAX_DISCOVERY_PACKET: usize = 1280;

/// Wire endpoint: IP + udp_port + tcp_port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Routable IP (v4 or v6).
    pub ip: IpAddr,
    /// UDP port (discovery).
    pub udp_port: u16,
    /// TCP port (RLPx).
    pub tcp_port: u16,
}

impl Endpoint {
    /// Convenience constructor for the common IPv4-loopback case.
    #[must_use]
    pub const fn loopback(udp_port: u16, tcp_port: u16) -> Self {
        Self {
            ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            udp_port,
            tcp_port,
        }
    }

    fn encode_ip_bytes(&self) -> Vec<u8> {
        match self.ip {
            IpAddr::V4(v4) => v4.octets().to_vec(),
            IpAddr::V6(v6) => v6.octets().to_vec(),
        }
    }

    fn rlp_payload_length(&self) -> usize {
        let ip = self.encode_ip_bytes();
        ip.as_slice().length() + self.udp_port.length() + self.tcp_port.length()
    }

    fn encode_to(&self, out: &mut dyn alloy_rlp::BufMut) {
        let payload = self.rlp_payload_length();
        RlpHeader {
            list: true,
            payload_length: payload,
        }
        .encode(out);
        self.encode_ip_bytes().as_slice().encode(out);
        self.udp_port.encode(out);
        self.tcp_port.encode(out);
    }

    fn decode_from(buf: &mut &[u8]) -> Result<Self, alloy_rlp::Error> {
        let h = RlpHeader::decode(buf)?;
        if !h.list {
            return Err(alloy_rlp::Error::UnexpectedString);
        }
        let ip_bytes = <alloy_rlp::bytes::Bytes as Decodable>::decode(buf)?;
        let udp_port = u16::decode(buf)?;
        let tcp_port = u16::decode(buf)?;
        let ip = match ip_bytes.len() {
            4 => {
                let arr: [u8; 4] = ip_bytes[..].try_into().unwrap();
                IpAddr::V4(Ipv4Addr::from(arr))
            }
            16 => {
                let arr: [u8; 16] = ip_bytes[..].try_into().unwrap();
                IpAddr::V6(std::net::Ipv6Addr::from(arr))
            }
            other => {
                return Err(alloy_rlp::Error::Custom(if other == 0 {
                    "empty ip"
                } else {
                    "unexpected ip length"
                }))
            }
        };
        Ok(Self {
            ip,
            udp_port,
            tcp_port,
        })
    }
}

/// Ping packet (type 0x01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ping {
    /// Protocol version (currently 4).
    pub version: u32,
    /// Sender endpoint.
    pub from: Endpoint,
    /// Target endpoint.
    pub to: Endpoint,
    /// Unix-seconds expiration time.
    pub expiration: u64,
}

/// Pong packet (type 0x02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pong {
    /// Target endpoint (echoes the Ping `from`).
    pub to: Endpoint,
    /// Keccak hash of the Ping packet being replied to.
    pub ping_hash: H256,
    /// Unix-seconds expiration.
    pub expiration: u64,
}

/// FindNode packet (type 0x03) — request the K nodes closest to `target`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindNode {
    /// 32-byte target node id (typically `keccak256(remote_pubkey)`).
    pub target: H256,
    /// Unix-seconds expiration.
    pub expiration: u64,
}

/// Neighbours packet (type 0x04) — reply to a FindNode listing up to
/// K candidate endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Neighbours {
    /// Candidate endpoints. Empty list is valid (means "I know nothing
    /// closer than myself").
    pub nodes: Vec<Endpoint>,
    /// Unix-seconds expiration.
    pub expiration: u64,
}

/// Top-level packet enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    /// Ping (0x01).
    Ping(Ping),
    /// Pong (0x02).
    Pong(Pong),
    /// FindNode (0x03).
    FindNode(FindNode),
    /// Neighbours (0x04).
    Neighbours(Neighbours),
}

impl Packet {
    /// 1-byte type code.
    #[must_use]
    pub const fn type_byte(&self) -> u8 {
        match self {
            Self::Ping(_) => TYPE_PING,
            Self::Pong(_) => TYPE_PONG,
            Self::FindNode(_) => TYPE_FIND_NODE,
            Self::Neighbours(_) => TYPE_NEIGHBOURS,
        }
    }

    /// Encode just the RLP packet-data (without header/signature).
    pub fn encode_data(&self) -> Vec<u8> {
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        match self {
            Self::Ping(p) => {
                let payload = p.version.length()
                    + p.from.rlp_payload_length()
                    + p.to.rlp_payload_length()
                    + alloy_rlp::length_of_length(p.from.rlp_payload_length())
                    + alloy_rlp::length_of_length(p.to.rlp_payload_length())
                    + p.expiration.length();
                RlpHeader {
                    list: true,
                    payload_length: payload,
                }
                .encode(&mut buf);
                p.version.encode(&mut buf);
                p.from.encode_to(&mut buf);
                p.to.encode_to(&mut buf);
                p.expiration.encode(&mut buf);
            }
            Self::Pong(p) => {
                let payload = p.to.rlp_payload_length()
                    + alloy_rlp::length_of_length(p.to.rlp_payload_length())
                    + p.ping_hash.length()
                    + p.expiration.length();
                RlpHeader {
                    list: true,
                    payload_length: payload,
                }
                .encode(&mut buf);
                p.to.encode_to(&mut buf);
                p.ping_hash.encode(&mut buf);
                p.expiration.encode(&mut buf);
            }
            Self::FindNode(p) => {
                let payload = p.target.length() + p.expiration.length();
                RlpHeader {
                    list: true,
                    payload_length: payload,
                }
                .encode(&mut buf);
                p.target.encode(&mut buf);
                p.expiration.encode(&mut buf);
            }
            Self::Neighbours(p) => {
                let nodes_inner: usize = p
                    .nodes
                    .iter()
                    .map(|n| {
                        n.rlp_payload_length() + alloy_rlp::length_of_length(n.rlp_payload_length())
                    })
                    .sum();
                let nodes_field = nodes_inner + alloy_rlp::length_of_length(nodes_inner);
                let payload = nodes_field + p.expiration.length();
                RlpHeader {
                    list: true,
                    payload_length: payload,
                }
                .encode(&mut buf);
                RlpHeader {
                    list: true,
                    payload_length: nodes_inner,
                }
                .encode(&mut buf);
                for n in &p.nodes {
                    n.encode_to(&mut buf);
                }
                p.expiration.encode(&mut buf);
            }
        }
        buf.to_vec()
    }

    fn decode_data(type_byte: u8, mut data: &[u8]) -> Result<Self, DiscoveryError> {
        let h = RlpHeader::decode(&mut data).map_err(DiscoveryError::Rlp)?;
        if !h.list {
            return Err(DiscoveryError::Frame("expected list"));
        }
        match type_byte {
            TYPE_PING => {
                let version = u32::decode(&mut data).map_err(DiscoveryError::Rlp)?;
                let from = Endpoint::decode_from(&mut data).map_err(DiscoveryError::Rlp)?;
                let to = Endpoint::decode_from(&mut data).map_err(DiscoveryError::Rlp)?;
                let expiration = u64::decode(&mut data).map_err(DiscoveryError::Rlp)?;
                Ok(Self::Ping(Ping {
                    version,
                    from,
                    to,
                    expiration,
                }))
            }
            TYPE_PONG => {
                let to = Endpoint::decode_from(&mut data).map_err(DiscoveryError::Rlp)?;
                let ping_hash = H256::decode(&mut data).map_err(DiscoveryError::Rlp)?;
                let expiration = u64::decode(&mut data).map_err(DiscoveryError::Rlp)?;
                Ok(Self::Pong(Pong {
                    to,
                    ping_hash,
                    expiration,
                }))
            }
            TYPE_FIND_NODE => {
                let target = H256::decode(&mut data).map_err(DiscoveryError::Rlp)?;
                let expiration = u64::decode(&mut data).map_err(DiscoveryError::Rlp)?;
                Ok(Self::FindNode(FindNode { target, expiration }))
            }
            TYPE_NEIGHBOURS => {
                let nodes_h = RlpHeader::decode(&mut data).map_err(DiscoveryError::Rlp)?;
                if !nodes_h.list {
                    return Err(DiscoveryError::Frame("expected nodes list"));
                }
                let mut nodes = Vec::new();
                let start_len = data.len();
                while start_len - data.len() < nodes_h.payload_length {
                    nodes.push(Endpoint::decode_from(&mut data).map_err(DiscoveryError::Rlp)?);
                }
                let expiration = u64::decode(&mut data).map_err(DiscoveryError::Rlp)?;
                Ok(Self::Neighbours(Neighbours { nodes, expiration }))
            }
            other => Err(DiscoveryError::UnknownPacketType(other)),
        }
    }
}

/// Encode a signed Discovery v4 packet, ready for UDP transmission.
///
/// `secret` signs the packet. `packet` carries the body.
pub fn encode_packet(secret: &SecretKey, packet: &Packet) -> Result<Vec<u8>, DiscoveryError> {
    let data = packet.encode_data();
    let type_byte = packet.type_byte();

    // sig = sign(keccak256(type || data))
    let mut to_sign = Vec::with_capacity(1 + data.len());
    to_sign.push(type_byte);
    to_sign.extend_from_slice(&data);
    let sign_hash = keccak256(&to_sign);
    let signature =
        secp::sign(secret, &sign_hash).map_err(|e| DiscoveryError::Crypto(e.to_string()))?;
    let sig_bytes = signature.to_bytes(); // 65 bytes

    // hash = keccak256(sig || type || data)
    let mut to_hash = Vec::with_capacity(65 + 1 + data.len());
    to_hash.extend_from_slice(&sig_bytes);
    to_hash.push(type_byte);
    to_hash.extend_from_slice(&data);
    let hash = keccak256(&to_hash);

    let mut out = Vec::with_capacity(32 + 65 + 1 + data.len());
    out.extend_from_slice(hash.as_bytes());
    out.extend_from_slice(&sig_bytes);
    out.push(type_byte);
    out.extend_from_slice(&data);
    if out.len() > MAX_DISCOVERY_PACKET {
        return Err(DiscoveryError::PacketTooLarge(out.len()));
    }
    Ok(out)
}

/// Decoded inbound packet with recovered sender identity.
#[derive(Debug, Clone)]
pub struct DecodedPacket {
    /// The deserialized packet body.
    pub packet: Packet,
    /// Sender's secp256k1 public key, recovered from the signature.
    pub sender_pubkey: PublicKey,
    /// Sender's address (last 20 bytes of `keccak256(uncompressed_pubkey[1..])`).
    pub sender_address: Address,
    /// Hash of the wire packet — used as a "ping hash" in subsequent Pong.
    pub packet_hash: H256,
}

/// Decode + verify an inbound UDP packet.
pub fn decode_packet(bytes: &[u8]) -> Result<DecodedPacket, DiscoveryError> {
    if bytes.len() < 32 + 65 + 1 {
        return Err(DiscoveryError::Frame("packet too short"));
    }
    let hash_bytes = &bytes[0..32];
    let sig_bytes = &bytes[32..97];
    let type_byte = bytes[97];
    let data = &bytes[98..];

    // Verify hash = keccak256(sig || type || data).
    let mut to_hash = Vec::with_capacity(65 + 1 + data.len());
    to_hash.extend_from_slice(sig_bytes);
    to_hash.push(type_byte);
    to_hash.extend_from_slice(data);
    let expected_hash = keccak256(&to_hash);
    if expected_hash.as_bytes() != hash_bytes {
        return Err(DiscoveryError::Frame("bad packet hash"));
    }

    // Recover sender pubkey from signature over keccak256(type || data).
    let mut to_sign = Vec::with_capacity(1 + data.len());
    to_sign.push(type_byte);
    to_sign.extend_from_slice(data);
    let sign_hash = keccak256(&to_sign);
    let sig_arr: [u8; 65] = sig_bytes
        .try_into()
        .map_err(|_| DiscoveryError::Frame("sig wrong size"))?;
    let signature =
        Signature::from_bytes(&sig_arr).map_err(|e| DiscoveryError::Crypto(e.to_string()))?;
    let sender_pubkey =
        secp::recover(&signature, &sign_hash).map_err(|e| DiscoveryError::Crypto(e.to_string()))?;
    let sender_address = sender_pubkey.address();
    let packet = Packet::decode_data(type_byte, data)?;

    Ok(DecodedPacket {
        packet,
        sender_pubkey,
        sender_address,
        packet_hash: expected_hash,
    })
}

/// Convenience: return `now + secs` as a Unix-seconds expiration.
#[must_use]
pub fn expiration_in(secs: u64) -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + secs
}

/// Driver that binds a UDP socket and lets the caller exchange signed
/// Discovery v4 packets.
pub struct UdpDiscovery {
    socket: UdpSocket,
    secret: SecretKey,
    local_addr: SocketAddr,
}

impl UdpDiscovery {
    /// Bind on `addr` and own the secp256k1 secret used to sign outbound
    /// packets.
    pub async fn bind(addr: SocketAddr, secret: SecretKey) -> Result<Self, DiscoveryError> {
        let socket = UdpSocket::bind(addr)
            .await
            .map_err(|e| DiscoveryError::Io(e.to_string()))?;
        let local_addr = socket
            .local_addr()
            .map_err(|e| DiscoveryError::Io(e.to_string()))?;
        Ok(Self {
            socket,
            secret,
            local_addr,
        })
    }

    /// Local socket address the driver is actually bound to (useful when
    /// callers pass `:0` to let the OS pick a port).
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Send a packet to `peer`. Signs locally, returns the packet hash
    /// so callers can match the eventual Pong's `ping_hash`.
    pub async fn send(&self, peer: SocketAddr, packet: &Packet) -> Result<H256, DiscoveryError> {
        let bytes = encode_packet(&self.secret, packet)?;
        self.socket
            .send_to(&bytes, peer)
            .await
            .map_err(|e| DiscoveryError::Io(e.to_string()))?;
        // Compute the packet hash so the caller can correlate the Pong.
        let hash_arr: [u8; 32] = bytes[0..32]
            .try_into()
            .expect("just-built packet has 32-byte hash prefix");
        Ok(H256::new(hash_arr))
    }

    /// Read one inbound packet (blocks until something arrives or
    /// `timeout` elapses).
    pub async fn recv(
        &self,
        timeout: Duration,
    ) -> Result<(DecodedPacket, SocketAddr), DiscoveryError> {
        let mut buf = vec![0u8; MAX_DISCOVERY_PACKET];
        let (n, src) = tokio::time::timeout(timeout, self.socket.recv_from(&mut buf))
            .await
            .map_err(|_| DiscoveryError::Timeout)?
            .map_err(|e| DiscoveryError::Io(e.to_string()))?;
        let decoded = decode_packet(&buf[..n])?;
        Ok((decoded, src))
    }
}

/// Errors produced by the discovery layer.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    /// I/O error from the UDP socket.
    #[error("io: {0}")]
    Io(String),

    /// RLP encode/decode failure.
    #[error("rlp: {0}")]
    Rlp(alloy_rlp::Error),

    /// Generic framing failure (wrong length, bad hash, etc.).
    #[error("frame: {0}")]
    Frame(&'static str),

    /// Cryptographic failure (signature parse, signing, verification).
    #[error("crypto: {0}")]
    Crypto(String),

    /// Encountered an unknown packet-type byte.
    #[error("unknown packet type: 0x{0:02x}")]
    UnknownPacketType(u8),

    /// Encoded packet exceeded the spec's 1280-byte UDP ceiling.
    #[error("packet too large: {0} bytes (max {MAX_DISCOVERY_PACKET})")]
    PacketTooLarge(usize),

    /// `recv` timed out.
    #[error("recv timeout")]
    Timeout,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_secret(byte: u8) -> SecretKey {
        let mut bytes = [0u8; 32];
        bytes[31] = byte;
        SecretKey::from_bytes(&bytes).unwrap()
    }

    fn sample_endpoint(port: u16) -> Endpoint {
        Endpoint::loopback(port, port + 1)
    }

    #[test]
    fn endpoint_rlp_round_trip_ipv4() {
        let e = sample_endpoint(30303);
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        e.encode_to(&mut buf);
        let mut s: &[u8] = &buf;
        let decoded = Endpoint::decode_from(&mut s).unwrap();
        assert_eq!(decoded, e);
    }

    #[test]
    fn ping_packet_signs_verifies_and_round_trips() {
        let secret = fixed_secret(1);
        let ping = Packet::Ping(Ping {
            version: DISCOVERY_VERSION,
            from: sample_endpoint(30303),
            to: sample_endpoint(30304),
            expiration: 1_700_000_000,
        });
        let wire = encode_packet(&secret, &ping).unwrap();
        assert!(wire.len() <= MAX_DISCOVERY_PACKET);
        let decoded = decode_packet(&wire).unwrap();
        assert_eq!(decoded.packet, ping);
        assert_eq!(decoded.sender_address, secret.public_key().address());
    }

    #[test]
    fn pong_packet_signs_verifies_and_round_trips() {
        let secret = fixed_secret(2);
        let pong = Packet::Pong(Pong {
            to: sample_endpoint(30303),
            ping_hash: H256::new([0xab; 32]),
            expiration: 1_700_000_000,
        });
        let wire = encode_packet(&secret, &pong).unwrap();
        let decoded = decode_packet(&wire).unwrap();
        assert_eq!(decoded.packet, pong);
        assert_eq!(decoded.sender_address, secret.public_key().address());
    }

    #[test]
    fn tampered_packet_fails_hash_check() {
        let secret = fixed_secret(3);
        let ping = Packet::Ping(Ping {
            version: DISCOVERY_VERSION,
            from: sample_endpoint(30303),
            to: sample_endpoint(30304),
            expiration: 1,
        });
        let mut wire = encode_packet(&secret, &ping).unwrap();
        // Flip a byte in the data tail — the hash check must catch it.
        let last = wire.len() - 1;
        wire[last] ^= 0xff;
        let err = decode_packet(&wire);
        assert!(err.is_err());
    }

    #[test]
    fn truncated_packet_rejected() {
        let secret = fixed_secret(4);
        let pong = Packet::Pong(Pong {
            to: sample_endpoint(30303),
            ping_hash: H256::new([0xab; 32]),
            expiration: 1,
        });
        let wire = encode_packet(&secret, &pong).unwrap();
        // Keep only the 32-byte hash prefix.
        let err = decode_packet(&wire[..32]);
        assert!(err.is_err());
    }

    #[test]
    fn find_node_round_trip() {
        let secret = fixed_secret(7);
        let pkt = Packet::FindNode(FindNode {
            target: H256::new([0xab; 32]),
            expiration: 1_700_000_000,
        });
        let wire = encode_packet(&secret, &pkt).unwrap();
        let decoded = decode_packet(&wire).unwrap();
        assert_eq!(decoded.packet, pkt);
    }

    #[test]
    fn neighbours_round_trip_with_multiple_nodes() {
        let secret = fixed_secret(8);
        let pkt = Packet::Neighbours(Neighbours {
            nodes: vec![sample_endpoint(30303), sample_endpoint(30304)],
            expiration: 1_700_000_000,
        });
        let wire = encode_packet(&secret, &pkt).unwrap();
        let decoded = decode_packet(&wire).unwrap();
        assert_eq!(decoded.packet, pkt);
    }

    #[test]
    fn neighbours_round_trip_empty_list() {
        let secret = fixed_secret(9);
        let pkt = Packet::Neighbours(Neighbours {
            nodes: vec![],
            expiration: 1,
        });
        let wire = encode_packet(&secret, &pkt).unwrap();
        let decoded = decode_packet(&wire).unwrap();
        assert_eq!(decoded.packet, pkt);
    }

    #[test]
    fn unknown_type_byte_rejected() {
        let secret = fixed_secret(5);
        let pong = Packet::Pong(Pong {
            to: sample_endpoint(30303),
            ping_hash: H256::ZERO,
            expiration: 1,
        });
        let mut wire = encode_packet(&secret, &pong).unwrap();
        wire[97] = 0xff; // mutate the type byte AND fix the hash
        let mut to_hash = Vec::new();
        to_hash.extend_from_slice(&wire[32..97]);
        to_hash.extend_from_slice(&wire[97..]);
        let h = keccak256(&to_hash);
        wire[0..32].copy_from_slice(h.as_bytes());
        let err = decode_packet(&wire);
        match err {
            Err(DiscoveryError::UnknownPacketType(0xff)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    #[allow(clippy::similar_names)]
    async fn two_udp_drivers_ping_pong_over_loopback() {
        let sa = fixed_secret(11);
        let sb = fixed_secret(22);
        let a = UdpDiscovery::bind("127.0.0.1:0".parse().unwrap(), sa)
            .await
            .unwrap();
        let b = UdpDiscovery::bind("127.0.0.1:0".parse().unwrap(), sb)
            .await
            .unwrap();
        let a_addr = a.local_addr();
        let b_addr = b.local_addr();

        let ping = Packet::Ping(Ping {
            version: DISCOVERY_VERSION,
            from: Endpoint {
                ip: a_addr.ip(),
                udp_port: a_addr.port(),
                tcp_port: 0,
            },
            to: Endpoint {
                ip: b_addr.ip(),
                udp_port: b_addr.port(),
                tcp_port: 0,
            },
            expiration: expiration_in(60),
        });
        let ping_hash = a.send(b_addr, &ping).await.unwrap();

        // B receives the ping
        let (decoded_ping, from_a) = b.recv(Duration::from_secs(2)).await.unwrap();
        assert_eq!(from_a, a_addr);
        assert_eq!(decoded_ping.packet, ping);

        // B sends a pong echoing the ping hash
        let pong = Packet::Pong(Pong {
            to: Endpoint {
                ip: a_addr.ip(),
                udp_port: a_addr.port(),
                tcp_port: 0,
            },
            ping_hash,
            expiration: expiration_in(60),
        });
        b.send(a_addr, &pong).await.unwrap();

        // A receives the pong and correlates by ping_hash
        let (decoded_pong, from_b) = a.recv(Duration::from_secs(2)).await.unwrap();
        assert_eq!(from_b, b_addr);
        match decoded_pong.packet {
            Packet::Pong(p) => assert_eq!(p.ping_hash, ping_hash),
            other => panic!("expected Pong, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn recv_timeout_returns_timeout_error() {
        let s = fixed_secret(33);
        let d = UdpDiscovery::bind("127.0.0.1:0".parse().unwrap(), s)
            .await
            .unwrap();
        let err = d.recv(Duration::from_millis(50)).await;
        assert!(matches!(err, Err(DiscoveryError::Timeout)));
    }
}
