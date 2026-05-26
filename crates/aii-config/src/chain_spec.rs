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
    /// Initial per-block subsidy in Wei (minted to the block beneficiary).
    /// Halves every [`Self::block_reward_halving_interval`] blocks.
    #[serde(default = "default_block_reward")]
    pub block_reward_initial_wei: u128,
    /// Halving period in blocks. Set to `u64::MAX` to disable halving.
    #[serde(default = "default_halving_interval")]
    pub block_reward_halving_interval: u64,
}

const fn default_block_reward() -> u128 {
    // 2 AII (2 * 1e18 wei) per block — initial.
    2_000_000_000_000_000_000
}

const fn default_halving_interval() -> u64 {
    // ~4 years at 3 s/block: 365 * 24 * 60 * 60 * 4 / 3 = 42_048_000.
    // We round to a clean number and keep the value tuneable per spec.
    42_048_000
}

impl ChainSpec {
    /// Effective per-block subsidy at block `n`. Halves every
    /// [`Self::block_reward_halving_interval`] blocks; saturates to 0
    /// once the halving exponent exceeds the wei mantissa.
    #[must_use]
    pub const fn block_reward_at(&self, n: u64) -> u128 {
        if self.block_reward_halving_interval == 0 || self.block_reward_halving_interval == u64::MAX
        {
            return self.block_reward_initial_wei;
        }
        let halvings = n / self.block_reward_halving_interval;
        if halvings >= 128 {
            return 0;
        }
        self.block_reward_initial_wei >> halvings
    }
}

impl ChainSpec {
    /// Validate basic invariants. Returns the first violation encountered.
    #[allow(clippy::missing_const_for_fn)]
    pub fn validate(&self) -> Result<(), &'static str> {
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
    block_reward_initial_wei: 2_000_000_000_000_000_000, // 2 AII / block
    block_reward_halving_interval: 42_048_000,           // ~4y at 3s/block
};

/// Reference AII testnet spec.
pub const AII_TESTNET: ChainSpec = ChainSpec {
    chain_id: AII_TESTNET_CHAIN_ID,
    network: String::new(),
    block_time_seconds: 3,
    initial_gas_limit: 30_000_000,
    min_base_fee_per_gas: 100_000_000,
    block_reward_initial_wei: 2_000_000_000_000_000_000,
    block_reward_halving_interval: 42_048_000,
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

    #[test]
    fn block_reward_halves_at_interval_boundary() {
        let spec = ChainSpec::mainnet();
        let initial = spec.block_reward_initial_wei;
        assert_eq!(spec.block_reward_at(0), initial);
        assert_eq!(
            spec.block_reward_at(spec.block_reward_halving_interval - 1),
            initial
        );
        assert_eq!(
            spec.block_reward_at(spec.block_reward_halving_interval),
            initial / 2
        );
        assert_eq!(
            spec.block_reward_at(spec.block_reward_halving_interval * 2),
            initial / 4
        );
    }

    #[test]
    fn block_reward_saturates_to_zero_after_many_halvings() {
        let spec = ChainSpec::mainnet();
        // 128 halvings shifts a u128 to 0.
        let far = spec.block_reward_halving_interval.saturating_mul(200);
        assert_eq!(spec.block_reward_at(far), 0);
    }
}
