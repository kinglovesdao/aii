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

use aii_cli::{run_account_new, run_chain_id, run_status, run_tier};
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

    #[allow(clippy::unused_self)]
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
                    "name": "tier_recommend",
                    "description": "Probe local hardware and return the recommended AII node Tier (T1–T7).",
                    "inputSchema": { "type": "object", "properties": {} },
                },
            ],
        })
    }

    async fn handle_tools_call(&self, params: Value) -> Result<Value, RpcErrorObject> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
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
            "tier_recommend" => {
                let t = run_tier();
                Ok(tool_text(format!("score {} → {:?}", t.score, t.tier)))
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
    async fn tools_list_includes_four_tools() {
        let s = Server::new("http://127.0.0.1:0");
        let req = Request {
            jsonrpc: "2.0".into(),
            id: Some(json!(2)),
            method: "tools/list".into(),
            params: None,
        };
        let resp = s.handle(req).await.unwrap();
        let tools = resp.result.unwrap()["tools"].as_array().unwrap().len();
        assert_eq!(tools, 4);
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
