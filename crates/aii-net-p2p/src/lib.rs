//! # aii-net-p2p
//!
//! Peer transport for the AII protocol.
//!
//! Two layers, used independently:
//!
//! - **TCP transport** — length-prefixed RLP framing for application
//!   messages. [`Message`], [`Server`], [`dial`], [`Peer`].
//! - **UDP Discovery v4** ([`discovery`]) — devp2p-compatible
//!   Ping/Pong (FindNode/Neighbours land in v0.0.18+). Packets are
//!   secp256k1-signed and verified end-to-end; sender identity is
//!   recovered from the signature.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod discovery;

use alloy_rlp::{Decodable, Encodable, Header as RlpHeader};
use std::net::SocketAddr;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Network protocol version exchanged in `Hello`.
pub const AII_P2P_VERSION: u32 = 1;

/// Maximum encoded message size (1 MiB).
pub const MAX_FRAME_BYTES: u32 = 1024 * 1024;

/// Wire-level peer message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// First message of a session: protocol version + local peer name.
    Hello {
        /// Protocol major version (currently always [`AII_P2P_VERSION`]).
        version: u32,
        /// Human-readable peer name (≤ 64 bytes).
        name: String,
    },
    /// Liveness probe.
    Ping(u64),
    /// Reply to a `Ping`, echoing its nonce.
    Pong(u64),
    /// Graceful disconnect with a numeric reason.
    Disconnect(u32),
    /// BFT consensus payload — the bytes are an already-encoded
    /// `aii_consensus_bft::wire::BftMessage`. The transport treats
    /// them as opaque; only the consumer decodes them.
    Bft(Vec<u8>),
}

const TYPE_HELLO: u8 = 0x01;
const TYPE_PING: u8 = 0x02;
const TYPE_PONG: u8 = 0x03;
const TYPE_DISCONNECT: u8 = 0x04;
const TYPE_BFT: u8 = 0x05;

impl Message {
    /// Serialize to bytes (length-tagged inside, but does NOT prepend
    /// the 4-byte big-endian length used at the framing layer — that
    /// happens in [`Peer::send`]).
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = alloy_rlp::bytes::BytesMut::new();
        match self {
            Self::Hello { version, name } => {
                buf.extend_from_slice(&[TYPE_HELLO]);
                let inner = version.length() + name.as_bytes().length();
                RlpHeader {
                    list: true,
                    payload_length: inner,
                }
                .encode(&mut buf);
                version.encode(&mut buf);
                name.as_bytes().encode(&mut buf);
            }
            Self::Ping(n) => {
                buf.extend_from_slice(&[TYPE_PING]);
                let inner = n.length();
                RlpHeader {
                    list: true,
                    payload_length: inner,
                }
                .encode(&mut buf);
                n.encode(&mut buf);
            }
            Self::Pong(n) => {
                buf.extend_from_slice(&[TYPE_PONG]);
                let inner = n.length();
                RlpHeader {
                    list: true,
                    payload_length: inner,
                }
                .encode(&mut buf);
                n.encode(&mut buf);
            }
            Self::Disconnect(r) => {
                buf.extend_from_slice(&[TYPE_DISCONNECT]);
                let inner = r.length();
                RlpHeader {
                    list: true,
                    payload_length: inner,
                }
                .encode(&mut buf);
                r.encode(&mut buf);
            }
            Self::Bft(payload) => {
                buf.extend_from_slice(&[TYPE_BFT]);
                let bytes_ref: &[u8] = payload.as_slice();
                let inner = bytes_ref.length();
                RlpHeader {
                    list: true,
                    payload_length: inner,
                }
                .encode(&mut buf);
                bytes_ref.encode(&mut buf);
            }
        }
        buf.to_vec()
    }

    /// Decode a single message from its body bytes (without the
    /// 4-byte length prefix).
    pub fn decode(bytes: &[u8]) -> Result<Self, P2pError> {
        if bytes.is_empty() {
            return Err(P2pError::Frame("empty frame".into()));
        }
        let ty = bytes[0];
        let mut buf: &[u8] = &bytes[1..];
        let h = RlpHeader::decode(&mut buf).map_err(|e| P2pError::Frame(e.to_string()))?;
        if !h.list {
            return Err(P2pError::Frame("expected list".into()));
        }
        match ty {
            TYPE_HELLO => {
                let version = u32::decode(&mut buf).map_err(|e| P2pError::Frame(e.to_string()))?;
                let name_bytes = <alloy_rlp::bytes::Bytes as Decodable>::decode(&mut buf)
                    .map_err(|e| P2pError::Frame(e.to_string()))?;
                let name = String::from_utf8(name_bytes.to_vec())
                    .map_err(|_| P2pError::Frame("Hello.name not utf-8".into()))?;
                Ok(Self::Hello { version, name })
            }
            TYPE_PING => {
                let n = u64::decode(&mut buf).map_err(|e| P2pError::Frame(e.to_string()))?;
                Ok(Self::Ping(n))
            }
            TYPE_PONG => {
                let n = u64::decode(&mut buf).map_err(|e| P2pError::Frame(e.to_string()))?;
                Ok(Self::Pong(n))
            }
            TYPE_DISCONNECT => {
                let r = u32::decode(&mut buf).map_err(|e| P2pError::Frame(e.to_string()))?;
                Ok(Self::Disconnect(r))
            }
            TYPE_BFT => {
                let payload = <alloy_rlp::bytes::Bytes as Decodable>::decode(&mut buf)
                    .map_err(|e| P2pError::Frame(e.to_string()))?;
                if payload.len() > MAX_FRAME_BYTES as usize {
                    return Err(P2pError::Frame(format!(
                        "Bft payload too large: {}",
                        payload.len()
                    )));
                }
                Ok(Self::Bft(payload.to_vec()))
            }
            other => Err(P2pError::Frame(format!(
                "unknown message type 0x{other:02x}"
            ))),
        }
    }
}

/// One peer-to-peer TCP session.
pub struct Peer {
    stream: TcpStream,
}

impl Peer {
    /// Send a single message.
    pub async fn send(&mut self, m: &Message) -> Result<(), P2pError> {
        let body = m.encode();
        let len =
            u32::try_from(body.len()).map_err(|_| P2pError::Frame("frame > u32::MAX".into()))?;
        if len > MAX_FRAME_BYTES {
            return Err(P2pError::Frame(format!("frame too large: {len}")));
        }
        self.stream.write_all(&len.to_be_bytes()).await?;
        self.stream.write_all(&body).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// Receive a single message.
    pub async fn recv(&mut self) -> Result<Message, P2pError> {
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf);
        if len > MAX_FRAME_BYTES {
            return Err(P2pError::Frame(format!("frame too large: {len}")));
        }
        let mut body = vec![0u8; len as usize];
        self.stream.read_exact(&mut body).await?;
        Message::decode(&body)
    }
}

/// Dial a remote peer.
pub async fn dial(addr: SocketAddr) -> Result<Peer, P2pError> {
    let stream = TcpStream::connect(addr).await?;
    Ok(Peer { stream })
}

/// A bound listener that accepts inbound peers.
pub struct Server {
    listener: TcpListener,
}

impl Server {
    /// Bind to `addr`.
    pub async fn bind(addr: SocketAddr) -> Result<Self, P2pError> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self { listener })
    }

    /// Address the listener is actually bound to.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accept one inbound peer.
    pub async fn accept(&self) -> Result<Peer, P2pError> {
        let (stream, _) = self.listener.accept().await?;
        Ok(Peer { stream })
    }
}

/// Errors produced by the p2p layer.
#[derive(Debug, Error)]
pub enum P2pError {
    /// I/O error from the socket layer.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Framing / decoding failure.
    #[error("frame: {0}")]
    Frame(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trip_encode_decode() {
        let m = Message::Hello {
            version: AII_P2P_VERSION,
            name: "aii-node-test".to_string(),
        };
        let bytes = m.encode();
        let decoded = Message::decode(&bytes).unwrap();
        assert_eq!(decoded, m);
    }

    #[test]
    fn ping_pong_round_trip() {
        let p = Message::Ping(0xdead_beef_cafe_babe);
        assert_eq!(Message::decode(&p.encode()).unwrap(), p);
        let q = Message::Pong(42);
        assert_eq!(Message::decode(&q.encode()).unwrap(), q);
    }

    #[test]
    fn disconnect_round_trip() {
        let d = Message::Disconnect(7);
        assert_eq!(Message::decode(&d.encode()).unwrap(), d);
    }

    #[test]
    fn unknown_type_byte_rejected() {
        let mut bytes = Message::Ping(1).encode();
        bytes[0] = 0xff;
        assert!(Message::decode(&bytes).is_err());
    }

    #[tokio::test]
    async fn two_peers_exchange_hello() {
        let server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = server.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let mut peer = server.accept().await.unwrap();
            let msg = peer.recv().await.unwrap();
            assert!(matches!(msg, Message::Hello { .. }));
            peer.send(&Message::Hello {
                version: AII_P2P_VERSION,
                name: "server".to_string(),
            })
            .await
            .unwrap();
        });

        let mut client = dial(addr).await.unwrap();
        client
            .send(&Message::Hello {
                version: AII_P2P_VERSION,
                name: "client".to_string(),
            })
            .await
            .unwrap();
        let reply = client.recv().await.unwrap();
        assert_eq!(
            reply,
            Message::Hello {
                version: AII_P2P_VERSION,
                name: "server".to_string()
            }
        );

        server_task.await.unwrap();
    }

    #[test]
    fn bft_envelope_round_trip_preserves_payload() {
        // BftMessage bytes are opaque to the transport: we just need a
        // black-box round trip.
        let payload: Vec<u8> = (0..173u16).map(|i| (i & 0xff) as u8).collect();
        let m = Message::Bft(payload.clone());
        let bytes = m.encode();
        match Message::decode(&bytes).unwrap() {
            Message::Bft(p) => assert_eq!(p, payload),
            other => panic!("expected Bft, got {other:?}"),
        }
    }

    #[test]
    fn bft_envelope_rejects_oversized_frame() {
        // Any Bft payload bigger than MAX_FRAME_BYTES must be rejected
        // at encode-or-decode time so a hostile peer can't flood us.
        let p2 = vec![0u8; (MAX_FRAME_BYTES as usize) + 1];
        let m2 = Message::Bft(p2);
        // We allow encode to produce the bytes, but send/recv enforce
        // MAX_FRAME_BYTES. The Message::decode path also enforces it.
        let bytes = m2.encode();
        assert!(Message::decode(&bytes).is_err());
    }

    #[tokio::test]
    async fn bft_envelope_round_trip_over_tcp() {
        // End-to-end: encode + frame + send + recv + decode on real
        // tokio sockets.
        let server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = server.local_addr().unwrap();
        let payload = vec![0xab, 0xcd, 0xef, 0x01, 0x02];
        let p_clone = payload.clone();
        let server_task = tokio::spawn(async move {
            let mut peer = server.accept().await.unwrap();
            match peer.recv().await.unwrap() {
                Message::Bft(p) => assert_eq!(p, p_clone),
                other => panic!("expected Bft, got {other:?}"),
            }
        });
        let mut client = dial(addr).await.unwrap();
        client.send(&Message::Bft(payload)).await.unwrap();
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn ping_pong_round_trip_over_tcp() {
        let server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = server.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let mut peer = server.accept().await.unwrap();
            match peer.recv().await.unwrap() {
                Message::Ping(n) => peer.send(&Message::Pong(n)).await.unwrap(),
                other => panic!("expected Ping, got {other:?}"),
            }
        });

        let mut client = dial(addr).await.unwrap();
        client.send(&Message::Ping(0x4242)).await.unwrap();
        assert_eq!(client.recv().await.unwrap(), Message::Pong(0x4242));
        server_task.await.unwrap();
    }
}
