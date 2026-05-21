//! 256-bit unsigned integer. Re-exported from `alloy-primitives` for
//! one-source-of-truth across the AII workspace.

pub use alloy_primitives::U256;

#[cfg(test)]
mod tests {
    use super::U256;

    #[test]
    fn u256_addition_overflows_safely() {
        let max = U256::MAX;
        let (sum, overflow) = max.overflowing_add(U256::from(1u8));
        assert_eq!(sum, U256::ZERO);
        assert!(overflow);
    }

    #[test]
    fn u256_from_u64_round_trips_through_to_string() {
        let n = U256::from(1_234_567_890u64);
        assert_eq!(n.to_string(), "1234567890");
    }

    #[test]
    fn u256_zero_is_zero() {
        assert_eq!(U256::ZERO, U256::from(0u8));
    }
}
