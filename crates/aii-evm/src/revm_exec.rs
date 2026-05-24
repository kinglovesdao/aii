//! Run a transaction through `revm` and apply the resulting state diff
//! back into `aii-state::StateDb`.

use crate::{revm_db::RevmDb, ExecError};
use aii_state::{Account, StateDb};
use aii_storage::KvBackend;
use aii_types::{Address as AiiAddress, H256 as AiiH256};
use revm::primitives::{Address, ExecutionResult, Output, TxKind, U256};
use revm::Evm;
use std::sync::Arc;

/// Outcome of one revm-driven transaction.
#[derive(Debug, Clone)]
pub struct ExecutionSummary {
    /// `true` if revm reported `ExecutionResult::Success`.
    pub success: bool,
    /// Gas consumed (≤ tx gas_limit).
    pub gas_used: u64,
    /// Output bytes — return data for a CALL, runtime bytecode for a
    /// successful CREATE, otherwise empty.
    pub output: Vec<u8>,
    /// Contract address for a successful CREATE; `None` otherwise.
    pub deployed_contract: Option<AiiAddress>,
}

/// Execute a single transaction via `revm` and commit state changes
/// back to `state`.
///
/// `to` = `None` runs a contract creation (CREATE); `Some(addr)` runs a
/// CALL.  `gas_price` is used by revm for the sender-side gas-fee
/// debit; pass `0` to skip gas accounting (useful for tests).
pub fn execute_with_revm<B: KvBackend>(
    state: &Arc<StateDb<B>>,
    sender: AiiAddress,
    to: Option<AiiAddress>,
    value: U256,
    data: Vec<u8>,
    gas_limit: u64,
    gas_price: U256,
) -> Result<ExecutionSummary, ExecError> {
    let db = RevmDb::new(state.clone());

    let mut evm = Evm::builder()
        .with_db(db)
        .modify_tx_env(|tx| {
            tx.caller = Address::new(*sender.as_bytes());
            tx.transact_to = match to {
                Some(a) => TxKind::Call(Address::new(*a.as_bytes())),
                None => TxKind::Create,
            };
            tx.value = value;
            tx.data = data.into();
            tx.gas_limit = gas_limit;
            tx.gas_price = gas_price;
            tx.chain_id = None; // disable EIP-155 enforcement at this layer
        })
        .build();

    let result_and_state = evm
        .transact()
        .map_err(|e| ExecError::Revm(format!("{e:?}")))?;
    let result = result_and_state.result;
    let state_changes = result_and_state.state;

    // Apply revm's state diff back to our StateDb. We translate every
    // touched account into our `Account` shape and write it.
    for (revm_addr, revm_acc) in state_changes {
        let aii_addr = AiiAddress::new(revm_addr.into_array());
        let info = &revm_acc.info;
        let mut out = state.account(&aii_addr)?.unwrap_or(Account::EMPTY);
        out.nonce = info.nonce;
        out.balance = info.balance;
        out.code_hash = AiiH256::new(*info.code_hash.as_slice().first_chunk::<32>().unwrap());
        state.set_account(&aii_addr, &out)?;
    }

    let success = matches!(result, ExecutionResult::Success { .. });
    let gas_used = result.gas_used();
    let (output, deployed) = match result {
        ExecutionResult::Success { output, .. } => match output {
            Output::Call(bytes) => (bytes.to_vec(), None),
            Output::Create(bytes, addr) => (
                bytes.to_vec(),
                addr.map(|a| AiiAddress::new(a.into_array())),
            ),
        },
        _ => (Vec::new(), None),
    };

    Ok(ExecutionSummary {
        success,
        gas_used,
        output,
        deployed_contract: deployed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_state::Account;
    use aii_storage::MemoryBackend;
    use std::sync::Arc;

    fn fresh_state() -> Arc<StateDb<MemoryBackend>> {
        Arc::new(StateDb::new(Arc::new(MemoryBackend::new())))
    }

    fn fund(state: &Arc<StateDb<MemoryBackend>>, who: AiiAddress, balance_wei: u128) {
        let acc = Account {
            nonce: 0,
            balance: U256::from(balance_wei),
            ..Account::EMPTY
        };
        state.set_account(&who, &acc).unwrap();
    }

    #[test]
    fn revm_value_transfer_advances_balances() {
        let state = fresh_state();
        let alice = AiiAddress::new([0x01; 20]);
        let bob = AiiAddress::new([0x02; 20]);
        fund(&state, alice, 1_000_000_000_000_000_000); // 1 AII

        let summary = execute_with_revm(
            &state,
            alice,
            Some(bob),
            U256::from(123u64),
            vec![],
            21_000,
            U256::ZERO,
        )
        .unwrap();

        assert!(summary.success, "revm reported failure");
        assert_eq!(summary.deployed_contract, None);

        let alice_after = state.account(&alice).unwrap().unwrap();
        let bob_after = state.account(&bob).unwrap().unwrap();
        assert_eq!(
            alice_after.balance,
            U256::from(1_000_000_000_000_000_000u128 - 123)
        );
        assert_eq!(bob_after.balance, U256::from(123u64));
        // revm should have advanced the sender nonce.
        assert_eq!(alice_after.nonce, 1);
    }

    #[test]
    fn revm_insufficient_balance_returns_failure_or_error() {
        let state = fresh_state();
        let alice = AiiAddress::new([0x11; 20]);
        let bob = AiiAddress::new([0x22; 20]);
        fund(&state, alice, 100);
        // Try to send way more than Alice has.
        let r = execute_with_revm(
            &state,
            alice,
            Some(bob),
            U256::from(u128::MAX),
            vec![],
            21_000,
            U256::ZERO,
        );
        // revm raises a tx-validation error (Err) for over-balance value transfers.
        assert!(
            r.is_err(),
            "expected revm to reject over-balance transfer, got Ok"
        );
    }

    #[test]
    fn revm_empty_create_deploys_an_address() {
        // CREATE with empty init code deploys an account at a deterministic
        // address derived from (sender, nonce). The runtime code is empty.
        let state = fresh_state();
        let alice = AiiAddress::new([0xaa; 20]);
        fund(&state, alice, 1_000_000_000_000_000_000);

        let summary = execute_with_revm(
            &state,
            alice,
            None,
            U256::ZERO,
            vec![], // empty init code
            100_000,
            U256::ZERO,
        )
        .unwrap();

        assert!(summary.success);
        // For empty init code, revm returns a deployed address.
        assert!(summary.deployed_contract.is_some());
    }

    #[test]
    fn revm_call_to_eoa_with_zero_value_is_a_no_op_success() {
        let state = fresh_state();
        let alice = AiiAddress::new([0x33; 20]);
        let bob = AiiAddress::new([0x44; 20]);
        fund(&state, alice, 1_000_000_000);

        let s = execute_with_revm(
            &state,
            alice,
            Some(bob),
            U256::ZERO,
            vec![],
            21_000,
            U256::ZERO,
        )
        .unwrap();
        assert!(s.success);
        assert_eq!(s.output.len(), 0);
    }
}
