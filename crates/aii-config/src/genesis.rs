//! Genesis block — initial state allocation and the resulting header.

use crate::chain_spec::ChainSpec;
use aii_block::{Bloom, Header, EMPTY_LIST_HASH, EMPTY_TRIE_HASH};
use aii_state::{Account, EMPTY_CODE_HASH};
use aii_types::{Address, BlsPubKey, VrfPubKey, H256, U256};
use serde::{Deserialize, Serialize};

/// One BFT validator entry in the [`Genesis`].
///
/// The wire-level pubkey types ([`BlsPubKey`] / [`VrfPubKey`]) carry
/// the raw compressed bytes — `aii-consensus-bft` deserialises them
/// into runtime keys when building [`crate::Genesis::to_bft_config`]'s
/// equivalent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenesisValidator {
    /// Compressed BLS12-381 G1 public key (48 bytes, hex-encoded in JSON).
    pub bls_pubkey: BlsPubKey,
    /// Compressed VRF public key (32 bytes, hex-encoded in JSON).
    pub vrf_pubkey: VrfPubKey,
    /// Initial stake (uint, no decimals).
    pub stake: u64,
}

/// A single pre-allocation entry — pairs an address with its starting balance
/// and (optional) code hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenesisAlloc {
    /// Recipient.
    pub address: Address,
    /// Initial balance in Wei.
    pub balance: U256,
    /// Optional pre-deployed bytecode hash; `None` ⇒ EOA.
    #[serde(default)]
    pub code_hash: Option<H256>,
}

impl GenesisAlloc {
    /// Convert the entry into an [`Account`] record.
    pub fn to_account(&self) -> Account {
        Account {
            nonce: 0,
            balance: self.balance,
            storage_root: EMPTY_TRIE_HASH,
            code_hash: self.code_hash.unwrap_or(EMPTY_CODE_HASH),
        }
    }
}

/// Everything needed to bootstrap a brand-new AII chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Genesis {
    /// Chain spec (used to imprint `chain_id` and parameters).
    pub chain_spec: ChainSpec,
    /// Unix-seconds timestamp at the genesis block.
    pub timestamp: u64,
    /// Free-form extra data (≤ 32 bytes after RLP encoding).
    #[serde(default)]
    pub extra_data: Vec<u8>,
    /// Initial state allocation (each entry carries its own recipient address).
    #[serde(default)]
    pub alloc: Vec<GenesisAlloc>,
    /// Initial BFT validator set. Empty allowed for legacy / dev-mode chains;
    /// production deployments populate this so `aii-consensus-bft::BftConfig`
    /// can be derived directly from the genesis file.
    #[serde(default)]
    pub validators: Vec<GenesisValidator>,
    /// 32-byte seed used for leader selection at height 1 round 0. Subsequent
    /// rounds derive their seed from the previous leader's VRF output. For
    /// deterministic dev chains this can be all-zero; production chains should
    /// fold in chain-genesis randomness (e.g. a hash of validator metadata).
    #[serde(default)]
    pub initial_seed: [u8; 32],
}

impl Genesis {
    /// Materialise the genesis [`Header`].
    ///
    /// `state_root` is provided by the caller — it must already match the
    /// MPT root of the genesis `alloc` (computed via `aii_state::mpt_root`
    /// once that lands in v0.0.7). For v0.0.6 callers pass
    /// `EMPTY_TRIE_HASH` if `alloc` is empty.
    pub fn to_header(&self, state_root: H256) -> Header {
        Header {
            parent_hash: H256::ZERO,
            ommers_hash: EMPTY_LIST_HASH,
            beneficiary: Address::ZERO,
            state_root,
            transactions_root: EMPTY_TRIE_HASH,
            receipts_root: EMPTY_TRIE_HASH,
            logs_bloom: Bloom::ZERO,
            difficulty: U256::ZERO,
            number: 0,
            gas_limit: self.chain_spec.initial_gas_limit,
            gas_used: 0,
            timestamp: self.timestamp,
            extra_data: self.extra_data.clone(),
            mix_hash: H256::ZERO,
            nonce: [0u8; 8],
            base_fee_per_gas: U256::from(self.chain_spec.min_base_fee_per_gas),
            withdrawals_root: EMPTY_TRIE_HASH,
            blob_gas_used: None,
            excess_blob_gas: None,
            parent_beacon_block_root: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aii_block::Hashable;

    fn empty_mainnet() -> Genesis {
        Genesis {
            chain_spec: ChainSpec::mainnet(),
            timestamp: 1_700_000_000,
            extra_data: b"aii-genesis".to_vec(),
            alloc: Vec::new(),
            validators: Vec::new(),
            initial_seed: [0u8; 32],
        }
    }

    #[test]
    fn genesis_header_zero_block_number() {
        let g = empty_mainnet();
        assert_eq!(g.to_header(EMPTY_TRIE_HASH).number, 0);
    }

    #[test]
    fn genesis_header_zero_parent_hash() {
        let g = empty_mainnet();
        assert_eq!(g.to_header(EMPTY_TRIE_HASH).parent_hash, H256::ZERO);
    }

    #[test]
    fn genesis_header_uses_chain_spec_gas_limit() {
        let g = empty_mainnet();
        assert_eq!(g.to_header(EMPTY_TRIE_HASH).gas_limit, 30_000_000);
    }

    #[test]
    fn genesis_header_hash_is_stable() {
        let g = empty_mainnet();
        let h1 = g.to_header(EMPTY_TRIE_HASH).hash();
        let h2 = g.to_header(EMPTY_TRIE_HASH).hash();
        assert_eq!(h1, h2);
    }

    #[test]
    fn alloc_to_account_uses_empty_code_hash_for_eoa() {
        let alloc = GenesisAlloc {
            address: Address::new([0x42; 20]),
            balance: U256::from(1_000u64),
            code_hash: None,
        };
        let acc = alloc.to_account();
        assert_eq!(acc.code_hash, EMPTY_CODE_HASH);
        assert_eq!(acc.balance, U256::from(1_000u64));
        assert_eq!(acc.nonce, 0);
    }

    #[test]
    fn genesis_json_round_trip() {
        let g = empty_mainnet();
        let json = serde_json::to_string(&g).unwrap();
        let back: Genesis = serde_json::from_str(&json).unwrap();
        assert_eq!(g, back);
    }
}
