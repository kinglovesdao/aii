# aii-mcp

Model Context Protocol (MCP) server for the AII protocol.

Exposes a small read-only tool surface to MCP-capable clients (Claude Desktop /
Claude Code / Cursor / Cline). Tools are calls into a running `aiid` node via
JSON-RPC, or local-only helpers (Tier probe, address generation).

## Usage

```bash
# Bind to an aiid endpoint and serve MCP over stdio.
$ aii-mcp --rpc http://127.0.0.1:8545
```

Drop into Claude Desktop's `claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "aii": {
      "command": "aii-mcp",
      "args": ["--rpc", "http://127.0.0.1:8545"]
    }
  }
}
```

## Tools (v0.0.10)

| Name | Type | Description |
|------|------|-------------|
| `chain_status` | read | Chain id + network + head block |
| `chain_id` | read | Chain id (decimal) |
| `account_new` | read | Generate a fresh address (key is dropped) |
| `tier_recommend` | read | Probe local hardware → recommended Tier |

Write tools (`send_transaction`, `account_import`) land in v0.0.11 alongside
the encrypted keystore. They will follow a "confirm-then-sign" pattern with
client-side approval.

## Protocol

Implements MCP 2024-11-05 over stdio:

- `initialize` → server capabilities
- `tools/list` → tool catalog
- `tools/call` → invoke a tool
- `notifications/initialized` → no-op
