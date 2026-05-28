#!/usr/bin/env bash
# AII one-click installer for Linux (x86_64 glibc 2.34+ / Ubuntu 22.04+).
# Installs aiid + aii + aii-mcp into /usr/local/bin, sets up a systemd
# unit + a fresh validator keystore, and joins the live testnet.
#
# Usage:
#   sudo ./install-linux.sh           # install + start + join testnet
#   sudo ./install-linux.sh --observer  # RPC-only node, no validator key
#   sudo ./install-linux.sh --uninstall # stop + remove
set -euo pipefail

PREFIX="${PREFIX:-/usr/local}"
DATA_DIR="${DATA_DIR:-/var/lib/aiid}"
LOG_FILE="${LOG_FILE:-/var/log/aiid.log}"
UNIT="/etc/systemd/system/aiid.service"
RPC_BIND="${RPC_BIND:-0.0.0.0:8545}"
BFT_LISTEN="${BFT_LISTEN:-0.0.0.0:30311}"
DISCOVERY_ADVERTISE="${DISCOVERY_ADVERTISE:-}"
BFT_ADVERTISE="${BFT_ADVERTISE:-}"
BOOTNODE="${BOOTNODE:-http://8.211.135.234:8545}"
DISCOVERY_SEEDS="${DISCOVERY_SEEDS:-8.211.135.234:30310,106.14.223.128:30310}"
MODE="${1:-validator}"

if [[ "$MODE" == "--uninstall" ]]; then
  echo "[aii-install] stopping aiid + removing binaries…"
  systemctl stop aiid 2>/dev/null || true
  systemctl disable aiid 2>/dev/null || true
  rm -f "$UNIT"
  rm -f "$PREFIX/bin/aiid" "$PREFIX/bin/aii" "$PREFIX/bin/aii-mcp"
  systemctl daemon-reload
  echo "[aii-install] uninstalled. (data dir at $DATA_DIR kept; rm -rf manually if desired)"
  exit 0
fi

if [[ "$EUID" -ne 0 ]]; then
  echo "[aii-install] re-run with sudo (writes to $PREFIX/bin + /etc/systemd/system)" >&2
  exit 1
fi

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ -x "$HERE/bin/aiid" ]; then
  echo "[aii-install] using pre-built binaries from $HERE/bin/"
  install -m 0755 "$HERE/bin/aiid"    "$PREFIX/bin/aiid"
  install -m 0755 "$HERE/bin/aii"     "$PREFIX/bin/aii"
  install -m 0755 "$HERE/bin/aii-mcp" "$PREFIX/bin/aii-mcp"
else
  echo "[aii-install] no pre-built binaries — building from source (cargo, ~2–4 min)…"
  command -v cargo >/dev/null || {
    echo "[aii-install] installing Rust toolchain via rustup (no sudo needed)…"
    su - "$(logname 2>/dev/null || echo "$SUDO_USER")" -c \
      "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable"
    export PATH="/home/${SUDO_USER:-$USER}/.cargo/bin:$PATH"
  }
  # Build from a workspace root: either the bundle's parent (when
  # extracted into the repo) or a fresh clone in /tmp.
  if [ -f "$HERE/../Cargo.toml" ]; then
    WORK="$(cd "$HERE/.." && pwd)"
  else
    WORK="$(mktemp -d -t aii-build.XXXXXX)/aii"
    git clone --depth 1 https://github.com/kinglovesdao/aii.git "$WORK"
  fi
  ( cd "$WORK" && cargo build --release -p aii-node -p aii-cli -p aii-mcp )
  install -m 0755 "$WORK/target/release/aiid"    "$PREFIX/bin/aiid"
  install -m 0755 "$WORK/target/release/aii"     "$PREFIX/bin/aii"
  install -m 0755 "$WORK/target/release/aii-mcp" "$PREFIX/bin/aii-mcp"
fi

mkdir -p "$DATA_DIR" "$DATA_DIR/data"
install -m 0644 "$HERE/config/testnet-genesis.json" "$DATA_DIR/genesis.json"

# Generate a validator keystore if one isn't already on disk.
if [[ "$MODE" != "--observer" && ! -f "$DATA_DIR/keystore.json" ]]; then
  echo "[aii-install] generating fresh validator keystore at $DATA_DIR/keystore.json"
  "$PREFIX/bin/aii" validator keygen | tee "$DATA_DIR/keystore.json" > /dev/null
  echo "[aii-install]   ⚠ pubkeys above — send them to the testnet coordinator to be added to genesis"
fi

# Build the ExecStart line.
EXEC_BASE="$PREFIX/bin/aiid --data-dir $DATA_DIR/data --rpc $RPC_BIND --testnet --bootnode $BOOTNODE"
ADVERTISE_ARGS=""
if [[ -n "$DISCOVERY_ADVERTISE" ]]; then
  ADVERTISE_ARGS="$ADVERTISE_ARGS --discovery-advertise $DISCOVERY_ADVERTISE"
fi
if [[ -n "$BFT_ADVERTISE" ]]; then
  ADVERTISE_ARGS="$ADVERTISE_ARGS --bft-advertise $BFT_ADVERTISE"
fi
if [[ "$MODE" == "--observer" ]]; then
  EXEC="$EXEC_BASE"
else
  EXEC="$EXEC_BASE --bft --genesis $DATA_DIR/genesis.json --keystore $DATA_DIR/keystore.json --bft-listen $BFT_LISTEN$ADVERTISE_ARGS --peers 8.211.135.234:30311,106.14.223.128:30311"
fi

cat > "$UNIT" <<UNIT_EOF
[Unit]
Description=AII chain node (aiid)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
Environment=AII_DISCOVERY_SEEDS=$DISCOVERY_SEEDS
ExecStart=$EXEC
StandardOutput=append:$LOG_FILE
StandardError=append:$LOG_FILE
Restart=on-failure
RestartSec=3
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
UNIT_EOF

systemctl daemon-reload
systemctl enable aiid >/dev/null
systemctl restart aiid

sleep 2
if systemctl is-active --quiet aiid; then
  echo "[aii-install] ✅ aiid is running. tail -f $LOG_FILE  (or  systemctl status aiid)"
  echo "[aii-install] RPC:    http://$(hostname -I | awk '{print $1}'):8545"
  echo "[aii-install] CLI:    aii status --rpc http://127.0.0.1:8545"
else
  echo "[aii-install] ❌ aiid did not start — check $LOG_FILE"
  exit 1
fi
