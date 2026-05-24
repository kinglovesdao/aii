# aii-microchain

Sub-chain registry and lifecycle bookkeeping.

- `MicroChainId` (u32 newtype)
- `MicroChainSpec` — id + name + parent flush interval
- `Registry` — `id → spec` with `register` / `lookup` / `iter`
- `FlushAnchor` — pair of `(parent_block_hash, sub_block_hash)` tracking
  the last block flushed to the main chain
