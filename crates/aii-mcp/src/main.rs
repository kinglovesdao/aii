//! `aii-mcp` — Model Context Protocol server (stdio transport).

use aii_mcp::{Request, Server};
use clap::Parser;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Parser)]
#[command(
    name = "aii-mcp",
    version,
    about = "MCP server for the AII protocol — exposes AII tools to Claude / Cursor / Cline via stdio."
)]
struct Cli {
    /// JSON-RPC endpoint of the AII node that the MCP server proxies to.
    #[arg(long, default_value = "http://127.0.0.1:8545")]
    rpc: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let cli = Cli::parse();
    let server = Server::new(cli.rpc);

    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let mut stdout = tokio::io::stdout();

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let err_resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": { "code": -32700, "message": format!("parse: {e}") },
                });
                stdout.write_all(err_resp.to_string().as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
                continue;
            }
        };
        if let Some(resp) = server.handle(req).await {
            let line = serde_json::to_string(&resp)?;
            stdout.write_all(line.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}
