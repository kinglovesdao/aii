# aii-state

World-state primitives for the AII protocol.

- `Account` — 4 fields (nonce, balance, code_hash, storage_root) + Ethereum-compatible RLP
- `StateDb<B>` — KV-backed `Address → Account` store on top of `aii-storage`
- `mpt_root` — placeholder (empty input only); full MPT lands in v0.0.6

See [`docs/superpowers/specs/2026-05-24-aii-state-design.md`](../../docs/superpowers/specs/2026-05-24-aii-state-design.md) for the design spec.
