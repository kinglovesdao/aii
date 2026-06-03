//! # aii-mcp
//!
//! Minimal MCP (Model Context Protocol) server over JSON-RPC stdio.
//!
//! ## Public API
//! - [`Server`] — owns the RPC backend URL + handles a single MCP request
//!   via [`Server::handle`]; embedders wire the I/O loop (see `main.rs`)
//! - [`Request`] / [`Response`] — JSON-RPC frame types
//! - [`McpError`] umbrella

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use aii_cli::{
    run_account_from_mnemonic, run_account_mnemonic, run_account_new, run_account_new_encrypted,
    run_account_verify, run_bft_capacity, run_chain_id, run_discovery_probe, run_get_block_header,
    run_recent_blocks, run_status, run_tier, DEFAULT_DISCOVERY_PROBE_SEEDS,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

/// MCP protocol version this server advertises in `initialize`.
pub const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

/// JSON-RPC 2.0 request.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Request {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Request id; absent for notifications.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// Method name.
    pub method: String,
    /// Method parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// Echoes the request id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// Success result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error object (per JSON-RPC 2.0).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcErrorObject>,
}

/// JSON-RPC error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcErrorObject {
    /// Error code (per JSON-RPC 2.0 conventions).
    pub code: i32,
    /// Short message.
    pub message: String,
}

/// MCP server.
#[derive(Debug, Clone)]
pub struct Server {
    rpc_url: String,
}

impl Server {
    /// Construct a server bound to a remote `aiid` JSON-RPC endpoint.
    pub fn new(rpc_url: impl Into<String>) -> Self {
        Self {
            rpc_url: rpc_url.into(),
        }
    }

    /// Dispatch a single MCP request and return its response.
    ///
    /// Returns `None` for notifications (no response on the wire).
    pub async fn handle(&self, req: Request) -> Option<Response> {
        let id = req.id.clone();
        let result = match req.method.as_str() {
            "initialize" => Ok(self.handle_initialize()),
            "notifications/initialized" => return None,
            "tools/list" => Ok(self.handle_tools_list()),
            "tools/call" => {
                self.handle_tools_call(req.params.unwrap_or(Value::Null))
                    .await
            }
            other => Err(RpcErrorObject {
                code: -32601,
                message: format!("Method not found: {other}"),
            }),
        };
        Some(match result {
            Ok(v) => Response {
                jsonrpc: "2.0".into(),
                id,
                result: Some(v),
                error: None,
            },
            Err(e) => Response {
                jsonrpc: "2.0".into(),
                id,
                result: None,
                error: Some(e),
            },
        })
    }

    #[allow(clippy::unused_self)]
    fn handle_initialize(&self) -> Value {
        json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "aii-mcp",
                "version": env!("CARGO_PKG_VERSION"),
            },
        })
    }

    #[allow(clippy::too_many_lines, clippy::unused_self)]
    fn handle_tools_list(&self) -> Value {
        json!({
            "tools": [
                {
                    "name": "chain_status",
                    "description": "Return the AII node's chain id, network name, and head block number.",
                    "inputSchema": { "type": "object", "properties": {} },
                },
                {
                    "name": "chain_id",
                    "description": "Return only the EIP-155 chain id as a decimal number.",
                    "inputSchema": { "type": "object", "properties": {} },
                },
                {
                    "name": "account_new",
                    "description": "Generate a fresh secp256k1 address (private key is dropped).",
                    "inputSchema": { "type": "object", "properties": {} },
                },
                {
                    "name": "account_new_encrypted",
                    "description": "Generate a fresh secp256k1 key and return a Web3 v3 encrypted keystore (JSON) under the supplied password.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "password": { "type": "string", "description": "Password to encrypt the keystore under." }
                        },
                        "required": ["password"]
                    },
                },
                {
                    "name": "account_verify",
                    "description": "Verify that a password decrypts a Web3 v3 keystore JSON, returning the embedded address on success.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "keystore_json": { "type": "string", "description": "The full keystore JSON string." },
                            "password": { "type": "string", "description": "Password to test." }
                        },
                        "required": ["keystore_json", "password"]
                    },
                },
                {
                    "name": "mnemonic_new",
                    "description": "Generate a fresh BIP-39 mnemonic + derive its first ETH-compatible address (BIP-44 m/44'/60'/0'/0/0).",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "words": { "type": "integer", "description": "Word count: 12 / 15 / 18 / 21 / 24. Defaults to 12.", "minimum": 12, "maximum": 24 }
                        }
                    },
                },
                {
                    "name": "account_from_mnemonic",
                    "description": "Re-derive an ETH-compatible address from a BIP-39 phrase at BIP-44 path m/44'/60'/0'/0/{index}.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "phrase":     { "type": "string", "description": "Space-separated BIP-39 phrase." },
                            "passphrase": { "type": "string", "description": "Optional BIP-39 passphrase (\"25th word\"); defaults to empty." },
                            "index":      { "type": "integer", "description": "Address index. Defaults to 0.", "minimum": 0 }
                        },
                        "required": ["phrase"]
                    },
                },
                {
                    "name": "tier_recommend",
                    "description": "Probe local hardware and return the recommended AII node Tier (T1–T7).",
                    "inputSchema": { "type": "object", "properties": {} },
                },
                {
                    "name": "bft_capacity",
                    "description": "Compute the deterministic BFT committee capacity budget for the 30-second PoS finality target.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "validators": { "type": "integer", "description": "Active DPoS/BFT validators in the voting committee. Defaults to 21.", "minimum": 1, "maximum": 128 },
                            "proposal_bytes": { "type": "integer", "description": "Proposal bytes to budget. Defaults to the wire codec maximum.", "minimum": 0 },
                            "target_secs": { "type": "integer", "description": "Finality target seconds. Defaults to 30.", "minimum": 1 }
                        }
                    },
                },
                {
                    "name": "discovery_probe",
                    "description": "Probe AII Discovery v4 seeds and report discovered peers plus the public UDP endpoint observed by the seed.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "seeds": { "type": "string", "description": "Comma-separated Discovery v4 seed specs. Defaults to public testnet seeds." },
                            "listen": { "type": "string", "description": "Temporary UDP bind address. Defaults to 0.0.0.0:0." },
                            "bft_listen": { "type": "string", "description": "BFT listener whose TCP port is advertised in the probe. Defaults to 0.0.0.0:30311." },
                            "timeout_ms": { "type": "integer", "description": "Milliseconds to wait for replies. Defaults to 1500.", "minimum": 1 }
                        }
                    },
                },
                {
                    "name": "block_lookup",
                    "description": "Fetch a single block header by number or 32-byte hash.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string", "description": "Decimal block number, 0x-prefixed block number, or 0x-prefixed 32-byte block hash." }
                        },
                        "required": ["query"]
                    },
                },
                {
                    "name": "recent_blocks",
                    "description": "Return the N most-recently finalised block headers, newest first.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "limit": { "type": "integer", "description": "Max headers (server-capped at 100). Defaults to 10.", "minimum": 1, "maximum": 100 }
                        }
                    },
                },
            ],
        })
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_tools_call(&self, params: Value) -> Result<Value, RpcErrorObject> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);
        match name {
            "chain_status" => {
                let s = run_status(&self.rpc_url).await.map_err(|e| rpc_err(&e))?;
                Ok(tool_text(serde_json::to_string_pretty(&s).unwrap()))
            }
            "chain_id" => {
                let id = run_chain_id(&self.rpc_url).await.map_err(|e| rpc_err(&e))?;
                Ok(tool_text(format!("{id}")))
            }
            "account_new" => {
                let addr = run_account_new().map_err(|e| rpc_err(&e))?;
                Ok(tool_text(format!("0x{}", hex::encode(addr.as_bytes()))))
            }
            "account_new_encrypted" => {
                let password = string_arg(&args, "password")?;
                let json = run_account_new_encrypted(&password).map_err(|e| rpc_err(&e))?;
                Ok(tool_text(json))
            }
            "account_verify" => {
                let keystore_json = string_arg(&args, "keystore_json")?;
                let password = string_arg(&args, "password")?;
                let addr =
                    run_account_verify(&keystore_json, &password).map_err(|e| rpc_err(&e))?;
                Ok(tool_text(format!("0x{}", hex::encode(addr.as_bytes()))))
            }
            "mnemonic_new" => {
                let words = args
                    .get("words")
                    .and_then(Value::as_u64)
                    .map_or(12usize, |n| n as usize);
                let r = run_account_mnemonic(words).map_err(|e| rpc_err(&e))?;
                Ok(tool_text(serde_json::to_string_pretty(&r).unwrap()))
            }
            "account_from_mnemonic" => {
                let phrase = string_arg(&args, "phrase")?;
                let passphrase = args
                    .get("passphrase")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let index = args.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                let addr = run_account_from_mnemonic(&phrase, &passphrase, index)
                    .map_err(|e| rpc_err(&e))?;
                Ok(tool_text(format!("0x{}", hex::encode(addr.as_bytes()))))
            }
            "tier_recommend" => {
                let t = run_tier();
                Ok(tool_text(format!("score {} → {:?}", t.score, t.tier)))
            }
            "bft_capacity" => {
                let validators = args
                    .get("validators")
                    .and_then(Value::as_u64)
                    .map_or(21usize, |n| n as usize);
                let proposal_bytes = args
                    .get("proposal_bytes")
                    .and_then(Value::as_u64)
                    .map(|n| n as usize);
                let target_secs = args.get("target_secs").and_then(Value::as_u64);
                let r = run_bft_capacity(validators, proposal_bytes, target_secs, None)
                    .map_err(|e| rpc_err(&e))?;
                Ok(tool_text(serde_json::to_string_pretty(&r).unwrap()))
            }
            "discovery_probe" => {
                let seeds = args.get("seeds").and_then(Value::as_str).map_or_else(
                    || {
                        DEFAULT_DISCOVERY_PROBE_SEEDS
                            .iter()
                            .map(|seed| (*seed).to_string())
                            .collect()
                    },
                    split_csv,
                );
                let listen = args
                    .get("listen")
                    .and_then(Value::as_str)
                    .unwrap_or("0.0.0.0:0")
                    .parse()
                    .map_err(|e| RpcErrorObject {
                        code: -32602,
                        message: format!("bad listen address: {e}"),
                    })?;
                let bft_listen = args
                    .get("bft_listen")
                    .and_then(Value::as_str)
                    .unwrap_or("0.0.0.0:30311")
                    .parse()
                    .map_err(|e| RpcErrorObject {
                        code: -32602,
                        message: format!("bad bft_listen address: {e}"),
                    })?;
                let timeout_ms = args
                    .get("timeout_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(1500);
                let r = run_discovery_probe(&seeds, listen, bft_listen, timeout_ms, &[])
                    .await
                    .map_err(|e| rpc_err(&e))?;
                Ok(tool_text(serde_json::to_string_pretty(&r).unwrap()))
            }
            "block_lookup" => {
                let query = string_arg(&args, "query")?;
                let r = run_get_block_header(&self.rpc_url, &query)
                    .await
                    .map_err(|e| rpc_err(&e))?;
                match r {
                    Some(v) => Ok(tool_text(serde_json::to_string_pretty(&v).unwrap())),
                    None => Ok(tool_text(format!("block not found: {query}"))),
                }
            }
            "recent_blocks" => {
                let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(10);
                let headers = run_recent_blocks(&self.rpc_url, limit)
                    .await
                    .map_err(|e| rpc_err(&e))?;
                Ok(tool_text(serde_json::to_string_pretty(&headers).unwrap()))
            }
            other => Err(RpcErrorObject {
                code: -32602,
                message: format!("Unknown tool: {other}"),
            }),
        }
    }
}

fn rpc_err(e: &aii_cli::CliError) -> RpcErrorObject {
    RpcErrorObject {
        code: -32000,
        message: e.to_string(),
    }
}

fn string_arg(args: &Value, key: &str) -> Result<String, RpcErrorObject> {
    args.get(key)
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| RpcErrorObject {
            code: -32602,
            message: format!("missing or non-string argument: {key}"),
        })
}

fn split_csv(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(String::from)
        .collect()
}

fn tool_text(s: impl Into<String>) -> Value {
    json!({
        "content": [ { "type": "text", "text": s.into() } ],
        "isError": false,
    })
}

/// Top-level MCP error.
#[derive(Debug, Error)]
pub enum McpError {
    /// JSON parse / serialize failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    /// I/O error from the transport.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initialize_advertises_protocol_version_and_tools_capability() {
        let s = Server::new("http://127.0.0.1:0");
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "initialize".into(),
            params: None,
        };
        let resp = s.handle(req).await.unwrap();
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert!(result["capabilities"]["tools"].is_object());
        assert_eq!(result["serverInfo"]["name"], "aii-mcp");
    }

    #[tokio::test]
    async fn tools_list_includes_twelve_tools() {
        let s = Server::new("http://127.0.0.1:0");
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "tools/list".into(),
            params: None,
        };
        let resp = s.handle(req).await.unwrap();
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 12);
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        for expected in [
            "chain_status",
            "chain_id",
            "account_new",
            "account_new_encrypted",
            "account_verify",
            "mnemonic_new",
            "account_from_mnemonic",
            "tier_recommend",
            "bft_capacity",
            "discovery_probe",
            "block_lookup",
            "recent_blocks",
        ] {
            assert!(names.contains(&expected), "missing tool: {expected}");
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn call(name: &str, args: Value) -> Request {
        Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: Some(json!({ "name": name, "arguments": args })),
        }
    }

    #[tokio::test]
    async fn account_new_encrypted_returns_valid_keystore_json() {
        let s = Server::new("http://127.0.0.1:0");
        let req = call("account_new_encrypted", json!({ "password": "pw" }));
        let resp = s.handle(req).await.unwrap();
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["version"], 3);
        assert_eq!(parsed["crypto"]["cipher"], "aes-128-ctr");
    }

    #[tokio::test]
    async fn account_verify_round_trip() {
        let s = Server::new("http://127.0.0.1:0");
        // Generate a keystore first.
        let gen = s
            .handle(call(
                "account_new_encrypted",
                json!({ "password": "secret" }),
            ))
            .await
            .unwrap();
        let ks_json = gen.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        // Then verify it.
        let verify = s
            .handle(call(
                "account_verify",
                json!({ "keystore_json": ks_json, "password": "secret" }),
            ))
            .await
            .unwrap();
        let addr = verify.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(addr.starts_with("0x"));
        assert_eq!(addr.len(), 42);
    }

    #[tokio::test]
    async fn account_verify_wrong_password_errors() {
        let s = Server::new("http://127.0.0.1:0");
        let gen = s
            .handle(call(
                "account_new_encrypted",
                json!({ "password": "right" }),
            ))
            .await
            .unwrap();
        let ks_json = gen.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let verify = s
            .handle(call(
                "account_verify",
                json!({ "keystore_json": ks_json, "password": "wrong" }),
            ))
            .await
            .unwrap();
        assert!(verify.error.is_some());
    }

    #[tokio::test]
    async fn mnemonic_new_returns_phrase_and_address() {
        let s = Server::new("http://127.0.0.1:0");
        let resp = s
            .handle(call("mnemonic_new", json!({ "words": 12 })))
            .await
            .unwrap();
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["word_count"], 12);
        assert!(
            parsed["phrase"]
                .as_str()
                .unwrap()
                .split_whitespace()
                .count()
                == 12
        );
        assert!(parsed["address"].as_str().unwrap().starts_with("0x"));
    }

    #[tokio::test]
    async fn mnemonic_new_defaults_to_12_words() {
        let s = Server::new("http://127.0.0.1:0");
        let resp = s.handle(call("mnemonic_new", json!({}))).await.unwrap();
        let result = resp.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["word_count"], 12);
    }

    #[tokio::test]
    async fn account_from_mnemonic_matches_canonical_fixture() {
        let s = Server::new("http://127.0.0.1:0");
        let resp = s.handle(call(
            "account_from_mnemonic",
            json!({
                "phrase": "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
                "index": 0
            }),
        )).await.unwrap();
        let addr = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            addr.to_lowercase(),
            "0x9858effd232b4033e47d90003d41ec34ecaeda94"
        );
    }

    #[tokio::test]
    async fn account_from_mnemonic_missing_phrase_errors() {
        let s = Server::new("http://127.0.0.1:0");
        let resp = s
            .handle(call("account_from_mnemonic", json!({})))
            .await
            .unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[tokio::test]
    async fn notifications_initialized_returns_no_response() {
        let s = Server::new("http://127.0.0.1:0");
        let req = Request {
            jsonrpc: "2.0".into(),
            id: None,
            method: "notifications/initialized".into(),
            params: None,
        };
        assert!(s.handle(req).await.is_none());
    }

    #[tokio::test]
    async fn unknown_method_returns_jsonrpc_minus_32601() {
        let s = Server::new("http://127.0.0.1:0");
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(3)),
            method: "no/such/method".into(),
            params: None,
        };
        let resp = s.handle(req).await.unwrap();
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn account_new_tool_returns_hex_address() {
        let s = Server::new("http://127.0.0.1:0");
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(4)),
            method: "tools/call".into(),
            params: Some(json!({ "name": "account_new" })),
        };
        let resp = s.handle(req).await.unwrap();
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.starts_with("0x"));
        assert_eq!(text.len(), 42);
    }

    #[tokio::test]
    async fn tier_recommend_tool_returns_label() {
        let s = Server::new("http://127.0.0.1:0");
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(5)),
            method: "tools/call".into(),
            params: Some(json!({ "name": "tier_recommend" })),
        };
        let resp = s.handle(req).await.unwrap();
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.starts_with("score "));
    }

    #[tokio::test]
    async fn bft_capacity_tool_returns_budget_json() {
        let s = Server::new("http://127.0.0.1:0");
        let resp = s
            .handle(call("bft_capacity", json!({ "validators": 21 })))
            .await
            .unwrap();
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["validators"], 21);
        assert_eq!(parsed["target_secs"], 30);
        assert_eq!(parsed["equal_stake_quorum_votes"], 15);
        assert_eq!(parsed["satisfies_design_cap"], true);
    }

    #[tokio::test]
    async fn bft_capacity_tool_rejects_oversized_committee() {
        let s = Server::new("http://127.0.0.1:0");
        let resp = s
            .handle(call("bft_capacity", json!({ "validators": 129 })))
            .await
            .unwrap();
        assert!(resp.error.is_some());
        assert!(resp.error.unwrap().message.contains("exceeds maximum"));
    }

    #[tokio::test]
    async fn discovery_probe_tool_returns_empty_report_for_bad_seed() {
        let s = Server::new("http://127.0.0.1:0");
        let resp = s
            .handle(call(
                "discovery_probe",
                json!({
                    "seeds": "not a seed",
                    "listen": "127.0.0.1:0",
                    "bft_listen": "127.0.0.1:30311",
                    "timeout_ms": 1
                }),
            ))
            .await
            .unwrap();
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["resolved_seeds"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["discovered_bft_peers"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn unknown_tool_returns_invalid_params() {
        let s = Server::new("http://127.0.0.1:0");
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(6)),
            method: "tools/call".into(),
            params: Some(json!({ "name": "no_such_tool" })),
        };
        let resp = s.handle(req).await.unwrap();
        assert_eq!(resp.error.unwrap().code, -32602);
    }
}
