//! `oxide-mcp-server` binary — stdio MCP server.
//!
//! By default the binary starts with an empty tool registry. CLI tools can be
//! registered ahead of time by setting the `OXIDE_MCP_TOOLS` environment
//! variable to a JSON file describing the tools (see the README); the binary
//! is also designed to be embedded by other Rust Oxide processes that build
//! their own registry programmatically.

use oxide_mcp_server::{McpServer, ToolRegistry};
use tokio::io::BufReader;
use tracing::Level;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Log to stderr only — stdout is the JSON-RPC channel.
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();

    let registry = ToolRegistry::new();
    let server = McpServer::new(registry);

    let stdin = BufReader::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();
    server.run_io(stdin, stdout).await?;
    Ok(())
}
