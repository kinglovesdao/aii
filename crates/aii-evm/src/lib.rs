//! # aii-evm
//!
//! Execution layer for AII.
//!
//! ## Public API
//! - [`execute_transfer`] — fast-path value-transfer execution (no
//!   contract code). Validates nonce + balance and mutates `StateDb`.
//! - [`execute_with_revm`] — full EVM execution via `revm` 18. Handles
//!   contract calls, deployments, gas metering, and event logs.
//! - [`ExecError`] umbrella.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod revm_db;
pub mod revm_exec;

pub use revm_db::RevmDb;
pub use revm_exec::{execute_with_revm, ExecutionSummary};

use aii_block::{Bloom, Receipt, Tx, TxType};
use aii_state::{Account, StateDb, StateError};
use aii_storage::KvBackend;
use aii_types::{Address, U256};
use thiserror::Error;

/// Execute a value-transfer transaction (no contract code). Faster than
/// the full revm path and useful as a sanity oracle.
///
/// For contract call / CREATE / arbitrary bytecode execution use
/// [`execute_with_revm`] instead.
pub fn execute_transfer<B: KvBackend>(
    state: &StateDb<B>,
    sender: Address,
    tx: &Tx,
) -> Result<Receipt, ExecError> {
    let (nonce, gas_limit, to, value, _gas_price) = unpack(tx)?;

    let to = to.ok_or(ExecError::ContractCallsNotYetSupported)?;

    let mut sender_acc = state.account(&sender)?.unwrap_or(Account::EMPTY);
    if sender_acc.nonce != nonce {
        return Err(ExecError::NonceMismatch {
            expected: sender_acc.nonce,
            got: nonce,
        });
    }
    if sender_acc.balance < value {
        return Err(ExecError::InsufficientBalance);
    }

    let mut to_acc = state.account(&to)?.unwrap_or(Account::EMPTY);
    if !to_acc.is_eoa() {
        return Err(ExecError::ContractCallsNotYetSupported);
    }

    sender_acc.balance -= value;
    sender_acc.nonce = sender_acc.nonce.wrapping_add(1);
    to_acc.balance = to_acc.balance.wrapping_add(value);

    state.set_account(&sender, &sender_acc)?;
    state.set_account(&to, &to_acc)?;

    Ok(Receipt {
        tx_type: match tx {
            Tx::Legacy(_) => TxType::Legacy,
            Tx::Eip1559(_) => TxType::Eip1559,
            Tx::Eip4844(_) => TxType::Eip4844,
        },
        status: true,
        cumulative_gas_used: 21_000.min(gas_limit),
        logs_bloom: Bloom::ZERO,
        logs: vec![],
    })
}

#[allow(clippy::type_complexity)]
const fn unpack(tx: &Tx) -> Result<(u64, u64, Option<Address>, U256, U256), ExecError> {
    match tx {
        Tx::Legacy(t) => Ok((t.nonce, t.gas_limit, t.to, t.value, t.gas_price)),
        Tx::Eip1559(t) => Ok((t.nonce, t.gas_limit, t.to, t.value, t.max_fee_per_gas)),
        Tx::Eip4844(_) => Err(ExecError::ContractCallsNotYetSupported),
    }
}

/// Errors produced by execution.
#[derive(Debug, Error)]
pub enum ExecError {
    /// State backend failed.
    #[error("state: {0}")]
    State(#[from] StateError),

    /// Transaction nonce doesn't match the sender's account nonce.
    #[error("nonce mismatch: account at {expected}, tx supplied {got}")]
    NonceMismatch {
        /// Account nonce read from state.
        expected: u64,
        /// Nonce field on the incoming transaction.
        got: u64,
    },

    /// Sender's balance < transfer value.
    #[error("insufficient balance")]
    InsufficientBalance,

    /// Contract creation or call path on the *fast* `execute_transfer`
    /// route. Use [`execute_with_revm`] for contract execution.
    #[error("contract execution requires execute_with_revm")]
    ContractCallsNotYetSupported,

    /// REVM execution failure.
    #[error("revm: {0}")]
    Revm(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_block::TxLegacy;
    use aii_state::Account;
    use aii_storage::MemoryBackend;
    use aii_types::{AlgoId, H256};
    use std::sync::Arc;

    fn fresh_state_with_alice() -> (Arc<StateDb<MemoryBackend>>, Address) {
        let state = Arc::new(StateDb::new(Arc::new(MemoryBackend::new())));
        let alice = Address::new([0x01; 20]);
        let acc = Account {
            nonce: 0,
            balance: U256::from(1_000_000_000_000_000_000u64),
            ..Account::EMPTY
        };
        state.set_account(&alice, &acc).unwrap();
        (state, alice)
    }

    fn make_tx(nonce: u64, to: Option<Address>, value: u64) -> Tx {
        Tx::Legacy(TxLegacy {
            nonce,
            gas_price: U256::from(1_000_000_000u64),
            gas_limit: 21_000,
            to,
            value: U256::from(value),
            data: vec![],
            v: 27,
            r: H256::new([0xaa; 32]),
            s: H256::new([0xbb; 32]),
            algo_id: AlgoId::Secp256k1,
        })
    }

    #[test]
    fn happy_path_transfer() {
        let (state, alice) = fresh_state_with_alice();
        let bob = Address::new([0x02; 20]);
        let tx = make_tx(0, Some(bob), 100);
        let receipt = execute_transfer(&state, alice, &tx).unwrap();
        assert!(receipt.status);
        let alice_after = state.account(&alice).unwrap().unwrap();
        let bob_after = state.account(&bob).unwrap().unwrap();
        assert_eq!(alice_after.nonce, 1);
        assert_eq!(
            alice_after.balance,
            U256::from(1_000_000_000_000_000_000u64 - 100)
        );
        assert_eq!(bob_after.balance, U256::from(100u64));
    }

    #[test]
    fn nonce_mismatch_rejects() {
        let (state, alice) = fresh_state_with_alice();
        let bob = Address::new([0x02; 20]);
        let tx = make_tx(5, Some(bob), 100);
        let err = execute_transfer(&state, alice, &tx);
        match err {
            Err(ExecError::NonceMismatch {
                expected: 0,
                got: 5,
            }) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn insufficient_balance_rejects() {
        let (state, alice) = fresh_state_with_alice();
        let bob = Address::new([0x02; 20]);
        let huge = u64::MAX;
        let tx = make_tx(0, Some(bob), huge);
        let err = execute_transfer(&state, alice, &tx);
        assert!(matches!(err, Err(ExecError::InsufficientBalance)));
    }

    #[test]
    fn contract_creation_rejected_on_fast_path() {
        let (state, alice) = fresh_state_with_alice();
        let tx = make_tx(0, None, 0);
        let err = execute_transfer(&state, alice, &tx);
        assert!(matches!(err, Err(ExecError::ContractCallsNotYetSupported)));
    }

    #[test]
    fn contract_recipient_rejected_on_fast_path() {
        let (state, alice) = fresh_state_with_alice();
        let contract = Address::new([0x03; 20]);
        state
            .set_account(
                &contract,
                &Account {
                    code_hash: H256::new([0xcc; 32]),
                    ..Account::EMPTY
                },
            )
            .unwrap();
        let tx = make_tx(0, Some(contract), 100);
        let err = execute_transfer(&state, alice, &tx);
        assert!(matches!(err, Err(ExecError::ContractCallsNotYetSupported)));
    }

    #[test]
    fn nonce_increments_atomically() {
        let (state, alice) = fresh_state_with_alice();
        let bob = Address::new([0x02; 20]);
        execute_transfer(&state, alice, &make_tx(0, Some(bob), 10)).unwrap();
        execute_transfer(&state, alice, &make_tx(1, Some(bob), 10)).unwrap();
        let alice_after = state.account(&alice).unwrap().unwrap();
        assert_eq!(alice_after.nonce, 2);
    }
}
