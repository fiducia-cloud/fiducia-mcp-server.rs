//! fiducia-mcp: MCP server over stdio.
//!
//! stdout carries the MCP JSON-RPC stream, so ALL logging goes to stderr
//! (which is also why this binary does not use fiducia-telemetry — its
//! fallback logger writes to stdout and would corrupt the wire).

use rmcp::{transport::stdio, ServiceExt};
use tracing_subscriber::EnvFilter;

use fiducia_mcp_server::server::FiduciaMcp;
use fiducia_mcp_server::upstream::{Config, Upstream};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let config = Config::from_env();
    tracing::info!(
        node_url = %config.node_url,
        brain_url = %config.brain_url,
        agent_cp_url = config.agent_cp_url.as_deref().unwrap_or("(unset)"),
        internal_secret = if config.internal_secret.is_some() { "set" } else { "unset" },
        org_id = config.org_id.as_deref().unwrap_or("(unset)"),
        api_key = if config.api_key.is_some() { "set" } else { "unset" },
        "starting fiducia-mcp (stdio)"
    );

    let service = FiduciaMcp::new(Upstream::new(config))
        .serve(stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}
