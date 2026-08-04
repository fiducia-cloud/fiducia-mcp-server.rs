//! Binary bootstrap for the Fiducia MCP server.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    fiducia_mcp_server::runtime::run_stdio().await
}
