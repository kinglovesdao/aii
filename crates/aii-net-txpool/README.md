# aii-net-txpool

Local mempool for the AII protocol.

- `TxPool` — capacity-bounded, indexed by `(sender, nonce)`
- `add` rejects duplicates, replaces on higher gas-price for same nonce
- `drain_ready(sender, current_nonce)` returns a contiguous nonce run
- `evict_to(capacity)` drops lowest-gas-price entries first
