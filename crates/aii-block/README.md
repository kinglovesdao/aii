# aii-block

Block / Header / Tx / Receipt data types for the AII protocol.

- EIP-2718 envelope (Legacy / EIP-1559 / EIP-4844 placeholder)
- EIP-1559 + EIP-4895 + EIP-4844 + EIP-4788 header fields
- `AlgoId` extension: wire-compatible with Ethereum when `Secp256k1` (the default)
- Byte-perfect with ≥10 mainnet-style block fixtures
- `Hashable::hash()` = Keccak-256 of RLP encoding

See [`docs/superpowers/specs/2026-05-24-aii-block-design.md`](../../docs/superpowers/specs/2026-05-24-aii-block-design.md) for the design spec.
