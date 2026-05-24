# aii-config

Chain spec, genesis, and runtime parameters for the AII protocol.

- `ChainSpec` — chain id (default 99 per project memo), block time, target gas, finality params
- `Genesis` — initial allocation, timestamp, extra data, chain spec ref
- `Genesis::to_header()` — produces a deterministic genesis block header
