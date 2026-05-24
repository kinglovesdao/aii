//! Immutable chain identifier and protocol constants.

use serde::{Deserialize, Serialize};

/// AII mainnet chain id (per project memo).
pub const AII_CHAIN_ID: u64 = 99;
/// AII testnet chain id (off-by-one for parity with reth/erigon conventions).
pub const AII_TESTNET_CHAIN_ID: u64 = 9999;

/// Chain-wide parameters that never change after genesis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainSpec {
    /// EIP-155 chain id (99 on AII mainnet).
    pub chain_id: u64,
    /// Human-readable network name (mainnet / testnet / devnet).
    pub network: String,
    /// Target seconds per block.
    pub block_time_seconds: u64,
    /// Default gas limit at genesis (subsequent headers may adjust within EIP-1559 rules).
    pub initial_gas_limit: u64,
    /// EIP-1559 base fee floor (Wei).
    pub min_base_fee_per_gas: u64,
}

impl ChainSpec {
    /// Validate basic invariants. Returns the first violation encountered.
    pub const fn validate(&self) -> Result<(), &'static str> {
        if self.chain_id == 0 {
            return Err("chain_id must be > 0");
        }
        if self.block_time_seconds == 0 {
            return Err("block_time_seconds must be > 0");
        }
        if self.initial_gas_limit < 5_000_000 {
            return Err("initial_gas_limit must be >= 5_000_000");
        }
        Ok(())
    }
}

/// Reference AII mainnet spec.
pub const AII_MAINNET: ChainSpec = ChainSpec {
    chain_id: AII_CHAIN_ID,
    network: String::new(), // populated by `mainnet()`
    block_time_seconds: 3,
    initial_gas_limit: 30_000_000,
    min_base_fee_per_gas: 1_000_000_000,
};

/// Reference AII testnet spec.
pub const AII_TESTNET: ChainSpec = ChainSpec {
    chain_id: AII_TESTNET_CHAIN_ID,
    network: String::new(),
    block_time_seconds: 3,
    initial_gas_limit: 30_000_000,
    min_base_fee_per_gas: 100_000_000,
};

impl ChainSpec {
    /// Canonical AII mainnet spec.
    pub fn mainnet() -> Self {
        Self {
            network: "aii-mainnet".to_string(),
            ..AII_MAINNET
        }
    }

    /// Canonical AII testnet spec.
    pub fn testnet() -> Self {
        Self {
            network: "aii-testnet".to_string(),
            ..AII_TESTNET
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mainnet_chain_id_is_99() {
        assert_eq!(ChainSpec::mainnet().chain_id, 99);
    }

    #[test]
    fn testnet_distinguished_from_mainnet() {
        assert_ne!(ChainSpec::mainnet().chain_id, ChainSpec::testnet().chain_id);
    }

    #[test]
    fn mainnet_validates() {
        assert!(ChainSpec::mainnet().validate().is_ok());
    }

    #[test]
    fn zero_chain_id_rejected() {
        let mut bad = ChainSpec::mainnet();
        bad.chain_id = 0;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn low_gas_limit_rejected() {
        let mut bad = ChainSpec::mainnet();
        bad.initial_gas_limit = 100_000;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn json_round_trip() {
        let spec = ChainSpec::mainnet();
        let json = serde_json::to_string(&spec).unwrap();
        let back: ChainSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }
}
