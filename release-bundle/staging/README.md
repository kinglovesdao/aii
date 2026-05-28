# AII Node — Desktop / Server One-Click Installer (v0.0.65)

This bundle gives any operator a 60-second path to a running AII chain
node — validator or RPC-only observer — on Linux, macOS, or Windows.
Defaults connect to the live testnet (`aii.allfund.xyz`, chain id 9999).

## What's in the bundle

```
bin/                   ← pre-compiled Linux x86_64 binaries (~26 MB total)
  aiid                 ← chain daemon (RPC server + BFT consensus + block producer)
  aii                  ← user CLI (wallet, chain queries, validator keygen, sub-chain runner)
  aii-mcp              ← Model Context Protocol stdio server (Claude/Cursor/Cline tools)

config/
  testnet-genesis.json ← live testnet genesis (validators + initial state alloc)

install-linux.sh       ← Linux installer (systemd unit + boots on next reboot)
install-macos.sh       ← macOS installer (launchd LaunchDaemon, builds from source)
install-windows.ps1    ← Windows installer (Windows Service, builds from source)
README.md              ← this file
```

## One-command install

### Linux (Ubuntu 22.04+, Debian 12+, RHEL 9+)

```bash
sudo ./install-linux.sh                # validator + join testnet
sudo ./install-linux.sh --observer     # RPC-only node (no validator key)
sudo ./install-linux.sh --uninstall    # stop + remove
```

After install:
- `systemctl status aiid`
- `tail -f /var/log/aiid.log`
- `aii status --rpc http://127.0.0.1:8545`

### macOS (Intel + Apple Silicon)

```bash
./install-macos.sh                # builds from source via cargo, ~3–6 min
./install-macos.sh --observer
./install-macos.sh --uninstall
```

Installs as a launchd LaunchDaemon (`/Library/LaunchDaemons/org.aii.aiid.plist`).
Auto-starts on boot.

### Windows 10/11

```powershell
# Run as Administrator:
.\install-windows.ps1                # builds from source, registers as service
.\install-windows.ps1 -Observer
.\install-windows.ps1 -Uninstall
```

Registers `aiid` as a Windows Service (auto-start). Logs at
`C:\ProgramData\aii\aiid.log`.

## Validator vs Observer mode

| Mode | What it does | Genesis required? | Validator key? |
|---|---|---|---|
| **validator** (default) | Participates in BFT consensus, produces blocks when elected as proposer, votes on every round | yes (bundled) | yes (auto-generated on first install) |
| **observer** (`--observer` flag) | Serves RPC + indexes blocks via cold-join sync from the bootnode; no consensus participation | no | no |

**Joining the live testnet as a validator** requires your validator
pubkeys to be added to the testnet genesis. After `install-linux.sh`
runs, the BLS + VRF pubkeys are echoed to stdout (and saved in
`/var/lib/aiid/keystore.json`). Send them to the testnet coordinator
to be folded into the next epoch's elected set.

## Environment variables

The installer reads these overrides:

| Variable | Default | Purpose |
|---|---|---|
| `PREFIX` | `/usr/local` | Where to put binaries (`$PREFIX/bin/aiid`, …) |
| `DATA_DIR` | `/var/lib/aiid` | Where RocksDB, genesis, keystore live |
| `LOG_FILE` | `/var/log/aiid.log` | aiid stdout/stderr destination |
| `RPC_BIND` | `0.0.0.0:8545` | JSON-RPC listener |
| `BFT_LISTEN` | `0.0.0.0:30311` | BFT gossip TCP listener |
| `DISCOVERY_ADVERTISE` | empty | Public UDP Discovery v4 address advertised to peers, e.g. `203.0.113.10:30310` |
| `BFT_ADVERTISE` | empty | Public BFT TCP address advertised to peers, e.g. `203.0.113.10:30311` |
| `BOOTNODE` | `http://8.211.135.234:8545` | Used by `--bootnode` for cold-join sync |
| `DISCOVERY_SEEDS` | `8.211.135.234:30310,106.14.223.128:30310` | Exported as `AII_DISCOVERY_SEEDS` for automatic peer discovery |

Example: `sudo PREFIX=/opt DATA_DIR=/srv/aii ./install-linux.sh`

When `DISCOVERY_ADVERTISE` / `BFT_ADVERTISE` are empty, the node asks
Discovery v4 seeds what UDP endpoint they observe and uses that public
IP for advertisement. Set the variables explicitly when port mappings
do not match the default Discovery/BFT ports or when the inferred
address is not reachable from other validators.

## Verify the node works

After install, exercise the v0.0.65 RPC surface:

```bash
# Chain status
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"aii_status","params":[],"id":1}' \
  http://127.0.0.1:8545

# Most recent 5 blocks
aii recent --rpc http://127.0.0.1:8545 --limit 5

# DPoS active validator set
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"aii_getActiveValidators","params":[],"id":1}' \
  http://127.0.0.1:8545

# Governance proposals
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"aii_listProposals","params":[],"id":1}' \
  http://127.0.0.1:8545
```

A browser-friendly view of the same data is at
`https://allfund.xyz/#/explorer/chain`.

## Building from source

If you don't trust the pre-compiled Linux binary (or you're on aarch64 /
RISC-V / FreeBSD), build it yourself in 3 minutes:

```bash
git clone https://github.com/kinglovesdao/aii.git
cd aii
cargo build --release -p aii-node -p aii-cli -p aii-mcp
# binaries: target/release/{aiid,aii,aii-mcp}
```

Toolchain: rust 1.85+ (workspace pins via `rust-version`); 1.94.1 is the
known-good version used by the project's CI.

## Network ports

Open inbound on the firewall when running a validator:

| Port | Protocol | Purpose |
|---|---|---|
| `8545` | TCP | JSON-RPC (eth_* + aii_*) |
| `30311` | TCP | BFT gossip |
| `30310` | UDP | Discovery v4 peer bootstrap |

## Reporting issues

- Bugs: https://github.com/kinglovesdao/aii/issues
- Operational questions: tail `/var/log/aiid.log` first; the `INFO` lines
  are emitted on every block + every RPC call.

## License

MIT. See `LICENSE` in the source repo.
