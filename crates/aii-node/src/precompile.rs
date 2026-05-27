//! On-chain submit path for staking + governance.
//!
//! A single magic destination address (`PRECOMPILE_ADDR`) accepts
//! calldata-encoded operations against the persistent `StakeTable`
//! and `Governance` stores. Wallets construct an ordinary
//! EIP-1559 / legacy transaction whose `to == PRECOMPILE_ADDR` and
//! `data` carries the operation; `execute_block_txs` recognises the
//! address and dispatches before falling through to revm.
//!
//! ## Wire layout
//!
//! The first 4 bytes of `data` are a Solidity-style function
//! selector `= keccak256(signature)[..4]`:
//!
//! | Selector | Signature | Arguments |
//! |----------|-----------|-----------|
//! | `0x64c9ec6f` | `bond()` | (value is the bonded amount; from `tx.value`) |
//! | `0x3f172cef` | `beginUnbond()` | (no args; tx.value must be 0) |
//! | `0x3ccfd60b` | `withdraw()` | (no args) |
//! | `0x37038a1d` | `propose(uint64,string)` | `voting_ends_at_be8 ‖ title_len_be4 ‖ title_utf8` |
//! | `0xc7f21560` | `vote(uint64,bool)` | `proposal_id_be8 ‖ support_byte (0/1)` |
//!
//! A unit test (`selectors_match_keccak_signatures`) asserts the
//! constants stay aligned with the canonical Solidity signature
//! hashes. Unknown selectors return `Err`; the tx receipt records
//! `status = false`.

use crate::staking::StakeTable;
use crate::Governance;
use aii_types::{Address, U256};

/// Precompile destination address. Equal to
/// `0x0000000000000000000000000000000000000099` — the AII mainnet
/// chain id padded into a 20-byte address.
pub const PRECOMPILE_ADDR: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x99,
]);

/// Solidity-style function selector: `keccak256("bond()")[..4]`
/// (`0x64c9ec6f`).
pub const OP_BOND: [u8; 4] = [0x64, 0xc9, 0xec, 0x6f];
/// `keccak256("beginUnbond()")[..4]` (`0x3f172cef`).
pub const OP_BEGIN_UNBOND: [u8; 4] = [0x3f, 0x17, 0x2c, 0xef];
/// `keccak256("withdraw()")[..4]` (`0x3ccfd60b`).
pub const OP_WITHDRAW: [u8; 4] = [0x3c, 0xcf, 0xd6, 0x0b];
/// `keccak256("propose(uint64,string)")[..4]` (`0x37038a1d`).
pub const OP_PROPOSE: [u8; 4] = [0x37, 0x03, 0x8a, 0x1d];
/// `keccak256("vote(uint64,bool)")[..4]` (`0xc7f21560`).
pub const OP_VOTE: [u8; 4] = [0xc7, 0xf2, 0x15, 0x60];

/// Outcome of executing one precompile call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrecompileOutcome {
    /// `bond` — staker locked `amount` Wei from `tx.value`.
    Bonded {
        /// Address that bonded.
        staker: Address,
        /// Amount added to the bond.
        amount: U256,
    },
    /// `begin_unbond` — staker initiated unbond at `block_height`.
    UnbondStarted {
        /// Staker.
        staker: Address,
    },
    /// `withdraw` — staker swept `amount` after unbond timer elapsed.
    Withdrawn {
        /// Staker.
        staker: Address,
        /// Wei swept back into the staker's free balance.
        amount: U256,
    },
    /// `propose` — proposal id assigned.
    Proposed {
        /// Assigned proposal id.
        id: u64,
    },
    /// `vote` — staker cast `support`.
    Voted {
        /// Proposal voted on.
        id: u64,
        /// Yes / no.
        support: bool,
    },
}

/// Errors produced by the precompile dispatcher.
#[derive(Debug, thiserror::Error)]
pub enum PrecompileError {
    /// Calldata < 4 bytes / unknown opcode.
    #[error("invalid opcode")]
    InvalidOpcode,
    /// Argument decoding failed (wrong length, bad UTF-8, etc.).
    #[error("invalid args: {0}")]
    InvalidArgs(String),
    /// Underlying staking / governance call returned an error.
    #[error("backend: {0}")]
    Backend(String),
}

/// Dispatch a precompile call. `sender` is the recovered tx signer;
/// `value` is `tx.value`; `data` is `tx.data`; `block_height` is the
/// height of the block currently being applied.
///
/// # Errors
/// Returns a typed error if the calldata is malformed or the
/// underlying store rejects the operation.
pub fn dispatch(
    table: &StakeTable,
    gov: &Governance,
    sender: Address,
    value: U256,
    data: &[u8],
    block_height: u64,
    unbonding_period_blocks: u64,
) -> Result<PrecompileOutcome, PrecompileError> {
    if data.len() < 4 {
        return Err(PrecompileError::InvalidOpcode);
    }
    let opcode: [u8; 4] = data[..4].try_into().unwrap();
    let args = &data[4..];
    match opcode {
        OP_BOND => {
            if value.is_zero() {
                return Err(PrecompileError::InvalidArgs("bond value is zero".into()));
            }
            table
                .bond(&sender, value)
                .map_err(|e| PrecompileError::Backend(e.to_string()))?;
            Ok(PrecompileOutcome::Bonded {
                staker: sender,
                amount: value,
            })
        }
        OP_BEGIN_UNBOND => {
            table
                .begin_unbond(&sender, block_height, unbonding_period_blocks)
                .map_err(|e| PrecompileError::Backend(e.to_string()))?;
            Ok(PrecompileOutcome::UnbondStarted { staker: sender })
        }
        OP_WITHDRAW => {
            let swept = table
                .withdraw(&sender, block_height)
                .map_err(|e| PrecompileError::Backend(e.to_string()))?
                .unwrap_or(U256::ZERO);
            Ok(PrecompileOutcome::Withdrawn {
                staker: sender,
                amount: swept,
            })
        }
        OP_PROPOSE => {
            if args.len() < 8 + 4 {
                return Err(PrecompileError::InvalidArgs(
                    "propose args truncated".into(),
                ));
            }
            let mut end_arr = [0u8; 8];
            end_arr.copy_from_slice(&args[..8]);
            let voting_ends_at = u64::from_be_bytes(end_arr);
            let mut len_arr = [0u8; 4];
            len_arr.copy_from_slice(&args[8..12]);
            let title_len = u32::from_be_bytes(len_arr) as usize;
            if args.len() < 12 + title_len {
                return Err(PrecompileError::InvalidArgs(
                    "propose title length overflows args".into(),
                ));
            }
            let title = std::str::from_utf8(&args[12..12 + title_len])
                .map_err(|e| PrecompileError::InvalidArgs(format!("propose title: {e}")))?
                .to_string();
            let id = gov
                .propose(sender, title, voting_ends_at)
                .map_err(|e| PrecompileError::Backend(e.to_string()))?;
            Ok(PrecompileOutcome::Proposed { id })
        }
        OP_VOTE => {
            if args.len() != 8 + 1 {
                return Err(PrecompileError::InvalidArgs(
                    "vote args must be 9 bytes".into(),
                ));
            }
            let mut id_arr = [0u8; 8];
            id_arr.copy_from_slice(&args[..8]);
            let proposal_id = u64::from_be_bytes(id_arr);
            let support = args[8] != 0;
            gov.cast_vote(table, proposal_id, sender, support, block_height)
                .map_err(|e| PrecompileError::Backend(e.to_string()))?;
            Ok(PrecompileOutcome::Voted {
                id: proposal_id,
                support,
            })
        }
        _ => Err(PrecompileError::InvalidOpcode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Governance;
    use crate::StakeTable;
    use aii_storage::RocksDbBackend;
    use std::sync::Arc;

    fn fresh() -> (StakeTable, Governance) {
        let backend = Arc::new(RocksDbBackend::open_in_temp().unwrap());
        let table = StakeTable::new(Arc::clone(&backend));
        let gov = Governance::new(Arc::clone(&backend));
        (table, gov)
    }

    #[test]
    fn bond_via_precompile_records_stake() {
        let (table, gov) = fresh();
        let alice = Address::new([0xa1; 20]);
        let data = OP_BOND.to_vec();
        let out = dispatch(&table, &gov, alice, U256::from(1_000u64), &data, 1, 100).unwrap();
        assert!(matches!(out, PrecompileOutcome::Bonded { .. }));
        let rec = table.get(&alice).unwrap().unwrap();
        assert_eq!(rec.amount_wei, U256::from(1_000u64));
    }

    #[test]
    fn unbond_then_withdraw_round_trips_through_precompile() {
        let (table, gov) = fresh();
        let alice = Address::new([0xa1; 20]);
        // Bond.
        dispatch(&table, &gov, alice, U256::from(500u64), &OP_BOND, 1, 10).unwrap();
        // Begin unbond.
        dispatch(&table, &gov, alice, U256::ZERO, &OP_BEGIN_UNBOND, 5, 10).unwrap();
        // Withdraw too early: returns Withdrawn with amount 0.
        let early = dispatch(&table, &gov, alice, U256::ZERO, &OP_WITHDRAW, 6, 10).unwrap();
        match early {
            PrecompileOutcome::Withdrawn { amount, .. } => assert_eq!(amount, U256::ZERO),
            _ => panic!("expected Withdrawn"),
        }
        // Withdraw after unbond elapsed (5 + 10 = 15).
        let late = dispatch(&table, &gov, alice, U256::ZERO, &OP_WITHDRAW, 16, 10).unwrap();
        match late {
            PrecompileOutcome::Withdrawn { amount, .. } => assert_eq!(amount, U256::from(500u64)),
            _ => panic!("expected Withdrawn"),
        }
    }

    #[test]
    fn propose_via_precompile_records_governance_entry() {
        let (table, gov) = fresh();
        let alice = Address::new([0xa1; 20]);
        // voting_ends_at_be8 ‖ title_len_be4 ‖ title_utf8
        let title = b"raise gas limit";
        let mut data = OP_PROPOSE.to_vec();
        data.extend_from_slice(&100u64.to_be_bytes());
        data.extend_from_slice(&u32::try_from(title.len()).unwrap().to_be_bytes());
        data.extend_from_slice(title);
        let out = dispatch(&table, &gov, alice, U256::ZERO, &data, 1, 100).unwrap();
        match out {
            PrecompileOutcome::Proposed { id } => {
                assert_eq!(id, 1);
                let p = gov.get(id).unwrap().unwrap();
                assert_eq!(p.title, "raise gas limit");
                assert_eq!(p.voting_ends_at, 100);
            }
            _ => panic!("expected Proposed"),
        }
    }

    #[test]
    fn vote_via_precompile_records_vote_weight() {
        let (table, gov) = fresh();
        let alice = Address::new([0xa1; 20]);
        // Bond + propose first.
        table.bond(&alice, U256::from(1_000u64)).unwrap();
        let id = gov.propose(alice, "x".into(), 100).unwrap();
        // Vote yes.
        let mut data = OP_VOTE.to_vec();
        data.extend_from_slice(&id.to_be_bytes());
        data.push(1);
        let out = dispatch(&table, &gov, alice, U256::ZERO, &data, 5, 100).unwrap();
        assert_eq!(out, PrecompileOutcome::Voted { id, support: true });
    }

    #[test]
    fn selectors_match_keccak_signatures() {
        use aii_crypto::keccak::keccak256;
        fn sel(sig: &str) -> [u8; 4] {
            let h = keccak256(sig.as_bytes());
            let mut out = [0u8; 4];
            out.copy_from_slice(&h.as_bytes()[..4]);
            out
        }
        assert_eq!(OP_BOND, sel("bond()"));
        assert_eq!(OP_BEGIN_UNBOND, sel("beginUnbond()"));
        assert_eq!(OP_WITHDRAW, sel("withdraw()"));
        assert_eq!(OP_PROPOSE, sel("propose(uint64,string)"));
        assert_eq!(OP_VOTE, sel("vote(uint64,bool)"));
    }

    #[test]
    fn unknown_opcode_rejected() {
        let (table, gov) = fresh();
        let alice = Address::new([0xa1; 20]);
        let data = [0u8, 0, 0, 0xff];
        assert!(matches!(
            dispatch(&table, &gov, alice, U256::ZERO, &data, 1, 100),
            Err(PrecompileError::InvalidOpcode)
        ));
    }
}
