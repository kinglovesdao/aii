# aii-types

Primitive types for the AII protocol — every downstream crate (`aii-state`,
`aii-evm`, `aii-consensus`, ...) depends on the types defined here.

## Exports

| Type | Purpose |
| --- | --- |
| `H256` | 32-byte hash (Keccak-256 output, MPT node, ...) |
| `Address` | 20-byte EVM-compatible account address |
| `U256` | 256-bit unsigned integer (re-exported from `alloy-primitives`) |
| `AlgoId` | 1-byte signature-algorithm tag (D7 spec — secp256k1 / BLS / Ed25519 / ML-DSA / SLH-DSA / Falcon / hybrid) |
| `BlsPubKey` | Compressed BLS12-381 G1 public key (48 bytes) |
| `BlsSignature` | Compressed BLS12-381 G2 signature (96 bytes) |
| `SignedTx` | Generic signed-transaction envelope (algo-id dispatched) |
| `TypesError` | Umbrella error |

## Stability

`0.0.x` — unstable; breaking changes can happen in any release until `0.1.0`.
After `0.1.0` semver applies.

## Testing

```bash
cargo test -p aii-types
cargo test -p aii-types --test proptest
cargo doc -p aii-types --no-deps --open
```
