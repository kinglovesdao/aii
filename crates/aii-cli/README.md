# aii-cli — `aii`

User-facing CLI for the AII protocol.

```bash
# Show node status (chain id, head, network)
$ aii status --rpc http://127.0.0.1:8545
chain_id: 99
network:  aii-mainnet
head:     #0

# Show chain id only (machine-readable)
$ aii chain-id --rpc http://127.0.0.1:8545 --json
{"chain_id":99}

# Generate a fresh wallet key (in-memory; v0.0.10 has no keystore yet)
$ aii account new
address: 0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf

# Probe local hardware and print the recommended Tier
$ aii tier
score: 87 → T1Validator
```

Subcommands:
- `status` — chain id / network / head block
- `chain-id` — chain id only
- `account new` — generate a fresh secp256k1 key (prints the address only)
- `tier` — run the onboarding probe + print the recommended Tier

Future commands (post-v0.0.10): `account import`, `account list`, `send`,
`balance`, `block <n>`, `tx <hash>`, `node start/stop`.
