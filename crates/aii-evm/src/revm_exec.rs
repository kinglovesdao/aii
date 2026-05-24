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

    // Apply revm's state diff back to our StateDb:
    //   1. Account header (nonce/balance/code_hash)
    //   2. Newly-deployed bytecode under its code_hash (Code CF)
    //   3. Per-account storage slots that changed (AccountStorage CF)
    for (revm_addr, revm_acc) in state_changes {
        let aii_addr = AiiAddress::new(revm_addr.into_array());
        let info = &revm_acc.info;
        let mut out = state.account(&aii_addr)?.unwrap_or(Account::EMPTY);
        out.nonce = info.nonce;
        out.balance = info.balance;
        let code_hash_bytes = *info.code_hash.as_slice().first_chunk::<32>().unwrap();
        out.code_hash = AiiH256::new(code_hash_bytes);
        state.set_account(&aii_addr, &out)?;

        // Persist freshly-deployed bytecode. revm hands us the
        // analysed bytecode via `info.code` whenever it is brand-new
        // for the transaction; we store the *original* bytes content-
        // addressed by the code hash. `code_put` is idempotent — calling
        // it again with the same hash is a free no-op.
        if let Some(code) = &info.code {
            if !code.is_empty() {
                state.code_put(&AiiH256::new(code_hash_bytes), code.original_byte_slice())?;
            }
        }

        // Persist storage diff. revm reports every slot it touched;
        // we only need to write the ones that *changed*.
        for (slot_index, slot) in &revm_acc.storage {
            if !slot.is_changed() {
                continue;
            }
            let slot_key = AiiH256::new(slot_index.to_be_bytes::<32>());
            let new_val = AiiH256::new(slot.present_value.to_be_bytes::<32>());
            state.storage_put(&aii_addr, &slot_key, &new_val)?;
        }
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

    /// Hand-crafted EVM bytecode that, when CALLed, executes
    /// `SSTORE(slot=0, value=0x42)` then STOPs.
    ///
    /// Runtime (6 bytes): `60 42 60 00 55 00`
    ///   PUSH1 0x42 ; PUSH1 0x00 ; SSTORE ; STOP
    ///
    /// Deploy header (12 bytes) copies the 6-byte runtime to memory
    /// then RETURNs it:
    ///   PUSH1 0x06 ; PUSH1 0x0C ; PUSH1 0x00 ; CODECOPY
    ///   PUSH1 0x06 ; PUSH1 0x00 ; RETURN
    ///
    /// Full creation bytecode = deploy header || runtime.
    fn writer_creation_bytecode() -> Vec<u8> {
        vec![
            0x60, 0x06, 0x60, 0x0C, 0x60, 0x00, 0x39, 0x60, 0x06, 0x60, 0x00, 0xF3, // deploy
            0x60, 0x42, 0x60, 0x00, 0x55, 0x00, // runtime
        ]
    }

    fn writer_runtime_bytecode() -> Vec<u8> {
        vec![0x60, 0x42, 0x60, 0x00, 0x55, 0x00]
    }

    /// Hand-crafted EVM bytecode that, when CALLed, returns `SLOAD(0)`
    /// padded to 32 bytes as the call's return data.
    ///
    /// Runtime (11 bytes): `60 00 54 60 00 52 60 20 60 00 F3`
    ///   PUSH1 0x00 ; SLOAD ; PUSH1 0x00 ; MSTORE
    ///   PUSH1 0x20 ; PUSH1 0x00 ; RETURN
    fn reader_creation_bytecode() -> Vec<u8> {
        vec![
            0x60, 0x0B, 0x60, 0x0C, 0x60, 0x00, 0x39, 0x60, 0x0B, 0x60, 0x00, 0xF3, // deploy
            0x60, 0x00, 0x54, 0x60, 0x00, 0x52, 0x60, 0x20, 0x60, 0x00, 0xF3, // runtime
        ]
    }

    #[test]
    fn deploy_persists_runtime_bytecode_under_code_hash() {
        let state = fresh_state();
        let alice = AiiAddress::new([0xa1; 20]);
        fund(&state, alice, 1_000_000_000_000_000_000);

        let s = execute_with_revm(
            &state,
            alice,
            None,
            U256::ZERO,
            writer_creation_bytecode(),
            200_000,
            U256::ZERO,
        )
        .unwrap();
        assert!(s.success, "deploy failed");
        let contract = s.deployed_contract.expect("CREATE returns address");

        // Account at the deployed address must point at the runtime
        // bytecode's keccak hash, and the bytecode itself must be
        // retrievable from state.
        let acc = state.account(&contract).unwrap().expect("contract exists");
        let expected_hash = aii_crypto::keccak256(&writer_runtime_bytecode());
        assert_eq!(
            acc.code_hash, expected_hash,
            "account.code_hash must equal keccak(runtime)"
        );
        let fetched = state.code_get(&acc.code_hash).unwrap();
        assert_eq!(
            fetched.as_deref(),
            Some(writer_runtime_bytecode().as_slice()),
            "Code CF must contain the runtime bytecode"
        );
    }

    #[test]
    fn calling_writer_persists_storage_slot() {
        let state = fresh_state();
        let alice = AiiAddress::new([0xa2; 20]);
        fund(&state, alice, 1_000_000_000_000_000_000);

        // Tx 1: deploy.
        let deploy = execute_with_revm(
            &state,
            alice,
            None,
            U256::ZERO,
            writer_creation_bytecode(),
            200_000,
            U256::ZERO,
        )
        .unwrap();
        assert!(deploy.success);
        let contract = deploy.deployed_contract.unwrap();

        // Tx 2: CALL the writer. This is a SEPARATE execute_with_revm
        // invocation, so revm must reload the runtime bytecode from
        // state via code_by_hash — that path is what we are exercising.
        let call = execute_with_revm(
            &state,
            alice,
            Some(contract),
            U256::ZERO,
            vec![],
            100_000,
            U256::ZERO,
        )
        .unwrap();
        assert!(call.success, "CALL failed — code_by_hash likely empty");

        // Verify slot 0 now holds 0x42.
        let slot_0 = aii_types::H256::ZERO;
        let mut expected = [0u8; 32];
        expected[31] = 0x42;
        assert_eq!(
            state.storage_get(&contract, &slot_0).unwrap(),
            aii_types::H256::new(expected),
            "storage[contract][0] must equal 0x42 after writer call",
        );
    }

    #[test]
    fn reader_recovers_persisted_storage() {
        let state = fresh_state();
        let alice = AiiAddress::new([0xa3; 20]);
        fund(&state, alice, 1_000_000_000_000_000_000);

        // Tx 1: deploy writer, call writer → slot 0 = 0x42.
        let w_deploy = execute_with_revm(
            &state,
            alice,
            None,
            U256::ZERO,
            writer_creation_bytecode(),
            200_000,
            U256::ZERO,
        )
        .unwrap();
        let writer = w_deploy.deployed_contract.unwrap();
        execute_with_revm(
            &state,
            alice,
            Some(writer),
            U256::ZERO,
            vec![],
            100_000,
            U256::ZERO,
        )
        .unwrap();

        // Tx 2: deploy a reader at a DIFFERENT address.
        let r_deploy = execute_with_revm(
            &state,
            alice,
            None,
            U256::ZERO,
            reader_creation_bytecode(),
            200_000,
            U256::ZERO,
        )
        .unwrap();
        let reader = r_deploy.deployed_contract.unwrap();

        // Pre-seed the reader's slot 0 directly via StateDb so reader
        // returns it. This proves storage_get → revm.storage hits the
        // store, independent of writer/reader being the same contract.
        let slot_0 = aii_types::H256::ZERO;
        let mut expected_bytes = [0u8; 32];
        expected_bytes[31] = 0x77;
        state
            .storage_put(&reader, &slot_0, &aii_types::H256::new(expected_bytes))
            .unwrap();

        // Tx 3: CALL the reader; expect 32-byte big-endian 0x77.
        let read = execute_with_revm(
            &state,
            alice,
            Some(reader),
            U256::ZERO,
            vec![],
            100_000,
            U256::ZERO,
        )
        .unwrap();
        assert!(read.success);
        assert_eq!(read.output.len(), 32);
        assert_eq!(read.output[31], 0x77);
        assert!(read.output[..31].iter().all(|b| *b == 0));
    }
}
