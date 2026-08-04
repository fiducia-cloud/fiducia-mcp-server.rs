//! Process bootstrap and stdio transport lifecycle.
//!
//! Domain tools, upstream clients, startup flag parsing, and telemetry live in
//! dedicated modules. This module owns their composition and the MCP stdio
//! lifecycle so `main.rs` remains a thin executable entrypoint.

use std::error::Error;

use rmcp::{transport::stdio, ServiceExt};
use tracing::Instrument;

use crate::{
    server::FiduciaMcp,
    upstream::{Config, Upstream},
};

/// Start the Fiducia MCP server over stdio.
///
/// stdout is exclusively owned by MCP JSON-RPC framing. Runtime diagnostics
/// flow through the structured stderr subscriber installed by telemetry.
pub async fn run_stdio() -> Result<(), Box<dyn Error>> {
    let log_filter = crate::flags::process_log_filter()?;
    let _telemetry = crate::telemetry::init("fiducia-mcp", "fiducia-cloud", log_filter);

    let config = Config::from_env();
    tracing::info!(
        node_url_configured = std::env::var_os("FIDUCIA_NODE_URL").is_some(),
        brain_url_configured = std::env::var_os("FIDUCIA_BRAIN_URL").is_some(),
        agent_cp_url_configured = config.agent_cp_url.is_some(),
        internal_secret_configured = config.internal_secret.is_some(),
        org_id_configured = config.org_id.is_some(),
        api_key_configured = config.api_key.is_some(),
        transport = "stdio",
        "starting fiducia-mcp"
    );

    let server_span = tracing::info_span!("mcp.server", rpc.system = "mcp", transport = "stdio");
    let service = FiduciaMcp::new(Upstream::new(config))
        .serve(stdio())
        .instrument(server_span.clone())
        .await?;
    service.waiting().instrument(server_span).await?;
    Ok(())
}
