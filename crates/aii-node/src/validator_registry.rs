//! Validator runtime-key registry.
//!
//! Staking answers "how much did this address bond?". BFT also needs
//! "which BLS/VRF keys should this bonded address use?". This registry
//! stores that second half under the staker address so DPoS elections
//! can materialise a runtime-ready validator set at epoch boundaries.

use aii_storage::{ColumnFamily, KvBackend, RocksDbBackend};
use aii_types::{Address, BlsPubKey, VrfPubKey};
use std::sync::Arc;

/// Persisted-record column-family key prefix.
const KEY_PREFIX: &[u8] = b"validator_key:";

/// BFT runtime keys registered by one staker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatorKeys {
    /// Compressed BLS12-381 G1 public key.
    pub bls_pubkey: BlsPubKey,
    /// Compressed VRF public key.
    pub vrf_pubkey: VrfPubKey,
}

/// Registry accessor over `ColumnFamily::Meta`.
pub struct ValidatorRegistry {
    backend: Arc<RocksDbBackend>,
}

impl ValidatorRegistry {
    /// Construct from a shared backend.
    #[must_use]
    pub const fn new(backend: Arc<RocksDbBackend>) -> Self {
        Self { backend }
    }

    fn key(addr: &Address) -> Vec<u8> {
        let mut k = Vec::with_capacity(KEY_PREFIX.len() + 20);
        k.extend_from_slice(KEY_PREFIX);
        k.extend_from_slice(addr.as_bytes());
        k
    }

    fn encode_value(keys: ValidatorKeys) -> [u8; 80] {
        let mut out = [0u8; 80];
        out[..48].copy_from_slice(keys.bls_pubkey.as_bytes());
        out[48..].copy_from_slice(keys.vrf_pubkey.as_bytes());
        out
    }

    fn decode_value(bytes: &[u8]) -> Option<ValidatorKeys> {
        if bytes.len() != 80 {
            return None;
        }
        let mut bls = [0u8; 48];
        bls.copy_from_slice(&bytes[..48]);
        let mut vrf = [0u8; 32];
        vrf.copy_from_slice(&bytes[48..]);
        Some(ValidatorKeys {
            bls_pubkey: BlsPubKey::new(bls),
            vrf_pubkey: VrfPubKey::new(vrf),
        })
    }

    /// Register or replace the validator runtime keys for `addr`.
    ///
    /// # Errors
    /// Propagates backend write errors.
    pub fn register(
        &self,
        addr: &Address,
        keys: ValidatorKeys,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.backend.put(
            ColumnFamily::Meta,
            &Self::key(addr),
            &Self::encode_value(keys),
        )?;
        Ok(())
    }

    /// Read the registered keys for `addr`.
    ///
    /// # Errors
    /// Propagates backend read errors.
    pub fn get(
        &self,
        addr: &Address,
    ) -> Result<Option<ValidatorKeys>, Box<dyn std::error::Error + Send + Sync>> {
        let Some(bytes) = self.backend.get(ColumnFamily::Meta, &Self::key(addr))? else {
            return Ok(None);
        };
        Ok(Self::decode_value(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_then_get_round_trips() {
        let backend = Arc::new(RocksDbBackend::open_in_temp().unwrap());
        let registry = ValidatorRegistry::new(backend);
        let addr = Address::new([0xa1; 20]);
        let keys = ValidatorKeys {
            bls_pubkey: BlsPubKey::new([0xb1; 48]),
            vrf_pubkey: VrfPubKey::new([0xc1; 32]),
        };

        registry.register(&addr, keys).unwrap();

        assert_eq!(registry.get(&addr).unwrap(), Some(keys));
    }

    #[test]
    fn missing_key_returns_none() {
        let backend = Arc::new(RocksDbBackend::open_in_temp().unwrap());
        let registry = ValidatorRegistry::new(backend);
        assert_eq!(registry.get(&Address::new([0xa1; 20])).unwrap(), None);
    }
}
