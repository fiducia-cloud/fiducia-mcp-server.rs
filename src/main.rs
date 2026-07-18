//! fiducia-mcp: MCP server over stdio.
//!
//! stdout carries the MCP JSON-RPC stream, so ALL logging goes to stderr
//! using the local stdio-safe OpenTelemetry module.

use rmcp::{transport::stdio, ServiceExt};
use tracing::Instrument;

use fiducia_mcp_server::server::FiduciaMcp;
use fiducia_mcp_server::upstream::{Config, Upstream};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _telemetry = fiducia_mcp_server::telemetry::init("fiducia-mcp", "fiducia-cloud");

    let config = Config::from_env();
    tracing::info!(
        node_url_configured = std::env::var_os("FIDUCIA_NODE_URL").is_some(),
        brain_url_configured = std::env::var_os("FIDUCIA_BRAIN_URL").is_some(),
        agent_cp_url_configured = config.agent_cp_url.is_some(),
        internal_secret_configured = config.internal_secret.is_some(),
        org_id_configured = config.org_id.is_some(),
        api_key_configured = config.api_key.is_some(),
        "starting fiducia-mcp (stdio)"
    );

    let server_span = tracing::info_span!("mcp.server", rpc.system = "mcp", transport = "stdio");
    let service = FiduciaMcp::new(Upstream::new(config))
        .serve(stdio())
        .instrument(server_span.clone())
        .await?;
    service.waiting().instrument(server_span).await?;
    Ok(())
}
