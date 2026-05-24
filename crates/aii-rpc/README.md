# aii-rpc

JSON-RPC + WebSocket server for the AII node. Built on `jsonrpsee`.

Scope (v0.0.7):

- `eth_chainId` — returns the chain spec's id as `0x` quantity
- `eth_blockNumber` — returns a `RpcState`-supplied head block number
- `aii_status` — returns chain id + head block number + chain name

Day-0 follow-ups: full `eth_*` (getBalance, getTransactionByHash, sendRawTransaction, …) + `aii_*` (vnode, microchain) land alongside `aii-state` MPT.
