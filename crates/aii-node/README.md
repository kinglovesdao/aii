# aii-node — `aiid`

The AII node binary. Boots a node from a chain spec + data dir, opens
RocksDB storage, wires up the RPC server, and serves until SIGINT.

```bash
# Run with defaults (chain 99 mainnet, 127.0.0.1:8545 RPC, tmp data-dir).
cargo run -p aii-node --bin aiid -- --data-dir /tmp/aiid --rpc 127.0.0.1:8545

# Smoke-test:
curl -sS -X POST http://127.0.0.1:8545 -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"aii_status","params":[]}' | jq
```

Day-0 follow-ups: consensus engine wiring, peer / mempool wiring,
graceful shutdown of the storage backend.
