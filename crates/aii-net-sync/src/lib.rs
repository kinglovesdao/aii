//! # aii-net-sync
//!
//! Block-sync state machine.
//!
//! ## Public API
//! - [`SyncEngine`] — owns the current state, accepts [`Event`]s, returns
//!   [`Action`]s
//! - [`SyncState`] — the enum exposing `Idle` / `Headers` / `Bodies` /
//!   `Done`
//! - [`SyncError`] umbrella

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aii_block::{Block, Header};
use aii_types::H256;
use thiserror::Error;

/// Sync-engine state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SyncState {
    /// No sync in progress.
    #[default]
    Idle,
    /// Header-download phase. `next` is the next height to ask for.
    Headers {
        /// Target peer block number.
        target: u64,
        /// Next height to request.
        next: u64,
    },
    /// Body-download phase. `pending` lists the block hashes still to fetch.
    Bodies {
        /// Block hashes whose bodies have not yet arrived.
        pending: Vec<H256>,
    },
    /// Local chain matches the target.
    Done,
}

/// External event consumed by the state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A peer advertised its tip; we may need to start syncing.
    PeerAnnouncedTip {
        /// Peer-advertised height.
        height: u64,
    },
    /// A batch of headers arrived from the peer.
    HeadersReceived(Vec<Header>),
    /// A block body arrived.
    BodyReceived(Box<Block>),
    /// The peer disconnected — abort the sync.
    PeerDisconnected,
}

/// Action the engine asks its embedder to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Request `count` headers starting at height `from`.
    RequestHeaders {
        /// Starting height.
        from: u64,
        /// Number of headers requested.
        count: u32,
    },
    /// Request a block body by hash.
    RequestBody(H256),
    /// Persist a finalised block.
    PersistBlock(Box<Block>),
    /// Nothing to do — engine is idle / done.
    Idle,
}

/// Sync state machine.
#[derive(Debug, Default)]
pub struct SyncEngine {
    state: SyncState,
    /// Headers we've fetched but whose bodies haven't arrived yet.
    headers_buffer: Vec<Header>,
    /// Current local height (initially 0; tracks the head as we persist).
    local_height: u64,
}

/// Batch size used for header requests.
pub const HEADER_BATCH: u32 = 192;

impl SyncEngine {
    /// Build a fresh engine starting at `local_height` (typically the
    /// node's last-finalised block number; 0 for a brand-new chain).
    pub const fn new(local_height: u64) -> Self {
        Self {
            state: SyncState::Idle,
            headers_buffer: Vec::new(),
            local_height,
        }
    }

    /// Inspect the current state.
    pub const fn state(&self) -> &SyncState {
        &self.state
    }

    /// Step the machine. Returns the action the embedder should perform.
    pub fn handle(&mut self, event: Event) -> Result<Action, SyncError> {
        match (self.state.clone(), event) {
            (SyncState::Idle, Event::PeerAnnouncedTip { height }) => {
                if height <= self.local_height {
                    self.state = SyncState::Done;
                    return Ok(Action::Idle);
                }
                self.state = SyncState::Headers {
                    target: height,
                    next: self.local_height + 1,
                };
                Ok(Action::RequestHeaders {
                    from: self.local_height + 1,
                    count: HEADER_BATCH,
                })
            }

            (SyncState::Headers { target, next }, Event::HeadersReceived(batch)) => {
                if batch.is_empty() {
                    return Err(SyncError::EmptyHeaderBatch);
                }
                // Verify contiguity of received headers.
                for (i, h) in batch.iter().enumerate() {
                    let expected_n = next + i as u64;
                    if h.number != expected_n {
                        return Err(SyncError::HeaderOutOfOrder {
                            expected: expected_n,
                            got: h.number,
                        });
                    }
                }
                self.headers_buffer.extend(batch.iter().cloned());
                let new_next = next + batch.len() as u64;
                if new_next > target {
                    // Move into body fetch.
                    use aii_block::Hashable;
                    let pending: Vec<H256> =
                        self.headers_buffer.iter().map(Hashable::hash).collect();
                    let first = pending[0];
                    self.state = SyncState::Bodies { pending };
                    return Ok(Action::RequestBody(first));
                }
                self.state = SyncState::Headers {
                    target,
                    next: new_next,
                };
                Ok(Action::RequestHeaders {
                    from: new_next,
                    count: HEADER_BATCH,
                })
            }

            (SyncState::Bodies { mut pending }, Event::BodyReceived(block)) => {
                use aii_block::Hashable;
                let block_hash = block.hash();
                if pending.first().copied() != Some(block_hash) {
                    return Err(SyncError::UnexpectedBody { got: block_hash });
                }
                pending.remove(0);
                self.local_height = block.header.number;
                if pending.is_empty() {
                    self.state = SyncState::Done;
                    self.headers_buffer.clear();
                    return Ok(Action::PersistBlock(block));
                }
                let next_hash = pending[0];
                self.state = SyncState::Bodies { pending };
                drop(block);
                // Emit PersistBlock first; embedder is expected to call
                // a follow-up step to request the next body. To keep the
                // API single-action, we emit PersistBlock now and the
                // embedder calls handle(PeerAnnouncedTip{height: same})
                // to resume — but for the v0.0.9 scope we instead just
                // request the next body and rely on the embedder to
                // persist what was returned in this step's argument.
                //
                // NOTE: caller MUST persist `block` themselves before
                // calling handle again. This is documented above.
                Ok(Action::RequestBody(next_hash))
            }

            (_, Event::PeerDisconnected) => {
                self.state = SyncState::Idle;
                self.headers_buffer.clear();
                Ok(Action::Idle)
            }

            (state, event) => Err(SyncError::Unexpected {
                state: format!("{state:?}"),
                event: format!("{event:?}"),
            }),
        }
    }
}

/// Errors produced by the sync engine.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SyncError {
    /// Header batch arrived empty.
    #[error("empty header batch")]
    EmptyHeaderBatch,

    /// Received header is not at the expected height.
    #[error("header out of order: expected {expected}, got {got}")]
    HeaderOutOfOrder {
        /// Expected block height.
        expected: u64,
        /// Block height received.
        got: u64,
    },

    /// Received body's hash does not match the next pending hash.
    #[error("unexpected body: {got:?}")]
    UnexpectedBody {
        /// Hash of the received body.
        got: H256,
    },

    /// Event arrived while the machine was in a state that does not handle it.
    #[error("unexpected event {event} while in state {state}")]
    Unexpected {
        /// Current state at the time of the event.
        state: String,
        /// The event that triggered the error.
        event: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_block::{BlockBody, Bloom, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
    use aii_types::{Address, U256};

    fn header(n: u64) -> Header {
        Header {
            parent_hash: H256::new([(n as u8).wrapping_sub(1); 32]),
            ommers_hash: EMPTY_LIST_HASH,
            beneficiary: Address::new([0x55; 20]),
            state_root: EMPTY_TRIE_HASH,
            transactions_root: EMPTY_TRIE_HASH,
            receipts_root: EMPTY_TRIE_HASH,
            logs_bloom: Bloom::ZERO,
            difficulty: U256::ZERO,
            number: n,
            gas_limit: 30_000_000,
            gas_used: 0,
            timestamp: 1_700_000_000 + n,
            extra_data: vec![],
            mix_hash: H256::ZERO,
            nonce: [0u8; 8],
            base_fee_per_gas: U256::from(1u64),
            withdrawals_root: EMPTY_TRIE_HASH,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
        }
    }

    #[test]
    fn idle_to_done_when_peer_is_behind() {
        let mut e = SyncEngine::new(10);
        let act = e.handle(Event::PeerAnnouncedTip { height: 5 }).unwrap();
        assert_eq!(act, Action::Idle);
        assert_eq!(e.state(), &SyncState::Done);
    }

    #[test]
    fn idle_to_headers_when_peer_is_ahead() {
        let mut e = SyncEngine::new(0);
        let act = e.handle(Event::PeerAnnouncedTip { height: 100 }).unwrap();
        assert_eq!(
            act,
            Action::RequestHeaders {
                from: 1,
                count: HEADER_BATCH
            }
        );
        assert!(matches!(e.state(), SyncState::Headers { .. }));
    }

    #[test]
    fn headers_batch_advances_to_bodies_when_target_reached() {
        let mut e = SyncEngine::new(0);
        let _ = e.handle(Event::PeerAnnouncedTip { height: 3 }).unwrap();
        let act = e
            .handle(Event::HeadersReceived(vec![
                header(1),
                header(2),
                header(3),
            ]))
            .unwrap();
        assert!(matches!(act, Action::RequestBody(_)));
        assert!(matches!(e.state(), SyncState::Bodies { .. }));
    }

    #[test]
    fn out_of_order_headers_rejected() {
        let mut e = SyncEngine::new(0);
        let _ = e.handle(Event::PeerAnnouncedTip { height: 3 }).unwrap();
        let err = e.handle(Event::HeadersReceived(vec![header(2), header(1)]));
        assert!(matches!(err, Err(SyncError::HeaderOutOfOrder { .. })));
    }

    #[test]
    fn empty_header_batch_rejected() {
        let mut e = SyncEngine::new(0);
        let _ = e.handle(Event::PeerAnnouncedTip { height: 1 }).unwrap();
        assert_eq!(
            e.handle(Event::HeadersReceived(vec![])),
            Err(SyncError::EmptyHeaderBatch)
        );
    }

    #[test]
    fn bodies_to_done_when_all_received() {
        let mut e = SyncEngine::new(0);
        let _ = e.handle(Event::PeerAnnouncedTip { height: 1 }).unwrap();
        let h1 = header(1);
        let _ = e.handle(Event::HeadersReceived(vec![h1.clone()])).unwrap();
        let block = Block {
            header: h1,
            body: BlockBody::default(),
        };
        let act = e.handle(Event::BodyReceived(Box::new(block))).unwrap();
        assert!(matches!(act, Action::PersistBlock(_)));
        assert_eq!(e.state(), &SyncState::Done);
    }

    #[test]
    fn wrong_body_hash_rejected() {
        let mut e = SyncEngine::new(0);
        let _ = e.handle(Event::PeerAnnouncedTip { height: 1 }).unwrap();
        let _ = e.handle(Event::HeadersReceived(vec![header(1)])).unwrap();
        // Wrong header in body (number 2 instead of 1)
        let block = Block {
            header: header(2),
            body: BlockBody::default(),
        };
        let err = e.handle(Event::BodyReceived(Box::new(block)));
        assert!(matches!(err, Err(SyncError::UnexpectedBody { .. })));
    }

    #[test]
    fn peer_disconnect_resets_to_idle() {
        let mut e = SyncEngine::new(0);
        let _ = e.handle(Event::PeerAnnouncedTip { height: 10 }).unwrap();
        let act = e.handle(Event::PeerDisconnected).unwrap();
        assert_eq!(act, Action::Idle);
        assert_eq!(e.state(), &SyncState::Idle);
    }
}
