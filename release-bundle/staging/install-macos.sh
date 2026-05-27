#!/usr/bin/env bash
# AII one-click installer for macOS (Intel + Apple Silicon).
#
# Mac builds aren't shipped pre-compiled in this bundle (no signing
# infrastructure yet). This script clones the source repo, builds
# `aiid` / `aii` / `aii-mcp` via `cargo build --release`, installs to
# /usr/local/bin, and runs as a `launchd` LaunchDaemon. Total time:
# ~3 min on M1, ~6 min on Intel.
#
# Usage:
#   ./install-macos.sh              # build + install + start (validator)
#   ./install-macos.sh --observer   # RPC-only
#   ./install-macos.sh --uninstall  # remove
set -euo pipefail

PREFIX="${PREFIX:-/usr/local}"
DATA_DIR="${DATA_DIR:-/usr/local/var/aiid}"
LOG_FILE="${LOG_FILE:-/usr/local/var/log/aiid.log}"
PLIST="/Library/LaunchDaemons/org.aii.aiid.plist"
BOOTNODE="${BOOTNODE:-http://8.211.135.234:8545}"
SRC_REPO="https://github.com/kinglovesdao/aii.git"
MODE="${1:-validator}"

if [[ "$MODE" == "--uninstall" ]]; then
  echo "[aii-install] stopping aiid + removing binaries…"
  sudo launchctl unload "$PLIST" 2>/dev/null || true
  sudo rm -f "$PLIST"
  sudo rm -f "$PREFIX/bin/aiid" "$PREFIX/bin/aii" "$PREFIX/bin/aii-mcp"
  echo "[aii-install] uninstalled (data dir at $DATA_DIR kept)"
  exit 0
fi

command -v cargo >/dev/null || {
  echo "[aii-install] installing Rust toolchain via rustup…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
  source "$HOME/.cargo/env"
}

WORK="$(mktemp -d -t aii-build.XXXXXX)"
echo "[aii-install] cloning $SRC_REPO into $WORK"
git clone --depth 1 "$SRC_REPO" "$WORK/aii"
cd "$WORK/aii"

echo "[aii-install] cargo build --release (this takes a few minutes)…"
cargo build --release -p aii-node -p aii-cli -p aii-mcp

echo "[aii-install] installing binaries to $PREFIX/bin/ (will prompt for sudo)"
sudo install -m 0755 target/release/aiid    "$PREFIX/bin/aiid"
sudo install -m 0755 target/release/aii     "$PREFIX/bin/aii"
sudo install -m 0755 target/release/aii-mcp "$PREFIX/bin/aii-mcp"

sudo mkdir -p "$DATA_DIR" "$DATA_DIR/data" "$(dirname "$LOG_FILE")"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
sudo install -m 0644 "$HERE/config/testnet-genesis.json" "$DATA_DIR/genesis.json"

if [[ "$MODE" != "--observer" && ! -f "$DATA_DIR/keystore.json" ]]; then
  echo "[aii-install] generating validator keystore at $DATA_DIR/keystore.json"
  sudo bash -c "'$PREFIX/bin/aii' validator keygen > '$DATA_DIR/keystore.json'"
fi

ARGS_OBS="--data-dir $DATA_DIR/data --rpc 0.0.0.0:8545 --testnet --bootnode $BOOTNODE"
ARGS_VAL="$ARGS_OBS --bft --genesis $DATA_DIR/genesis.json --keystore $DATA_DIR/keystore.json --bft-listen 0.0.0.0:30311 --peers 8.211.135.234:30311,106.14.223.128:30311"
ARGS="$ARGS_VAL"
[[ "$MODE" == "--observer" ]] && ARGS="$ARGS_OBS"

# Build the plist with one ProgramArgument per CLI token.
read -r -a TOK <<< "$ARGS"
PROG_ARGS_XML="<string>$PREFIX/bin/aiid</string>"
for t in "${TOK[@]}"; do
  PROG_ARGS_XML="$PROG_ARGS_XML
        <string>$t</string>"
done

sudo tee "$PLIST" >/dev/null <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>            <string>org.aii.aiid</string>
    <key>ProgramArguments</key> <array>$PROG_ARGS_XML</array>
    <key>RunAtLoad</key>        <true/>
    <key>KeepAlive</key>        <true/>
    <key>StandardOutPath</key>  <string>$LOG_FILE</string>
    <key>StandardErrorPath</key><string>$LOG_FILE</string>
</dict>
</plist>
PLIST_EOF

sudo launchctl unload "$PLIST" 2>/dev/null || true
sudo launchctl load -w "$PLIST"
sleep 2
if sudo launchctl list | grep -q org.aii.aiid; then
  echo "[aii-install] ✅ aiid is running via launchd. tail -f $LOG_FILE"
  echo "[aii-install] RPC: http://127.0.0.1:8545"
else
  echo "[aii-install] ❌ launchd reported failure; check $LOG_FILE"
  exit 1
fi
