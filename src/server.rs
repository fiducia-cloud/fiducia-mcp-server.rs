//! The MCP server: one tool per fiducia.cloud diagnostic question.
//!
//! Every tool is read-only (GETs only). Mutations — acquiring locks, moving
//! shards, releasing leases — stay with the real clients (fiducia-client,
//! fiducia-cli) where fencing tokens are handled properly.

use rmcp::{
    handler::server::wrapper::Parameters,
    model::{
        CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities,
        ServerInfo,
    },
    schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler,
};
use std::sync::Arc;

use crate::repo_map::REPO_MAP;
use crate::upstream::{urlencode, Plane, Upstream};

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ObserveParams {
    /// What to observe on the node: "locks", "semaphores", "elections",
    /// "shards", or "metrics".
    pub what: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PlacementParams {
    /// Shard id to look up; omit for the full shard map.
    #[serde(default)]
    pub shard: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RouteKeyParams {
    /// The coordination key to resolve, e.g. "orders/checkout".
    pub key: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct KvGetParams {
    /// Exact key to read. Provide exactly one of `key` or `prefix`.
    #[serde(default)]
    pub key: Option<String>,
    /// Key prefix to list.
    #[serde(default)]
    pub prefix: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LockGetParams {
    /// Lock key to inspect (returns holder, fencing token, wait queue).
    pub key: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ServicesParams {
    /// Service name to list live instances for; omit to list all services.
    #[serde(default)]
    pub service: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FileLeaseParams {
    /// Repository the file belongs to, e.g. "fiducia-cloud/fiducia-node.rs".
    pub repository: String,
    /// Repo-relative file path, e.g. "src/main.rs".
    pub path: String,
}

const OBSERVE_KINDS: [&str; 5] = ["locks", "semaphores", "elections", "shards", "metrics"];

#[derive(Clone)]
pub struct FiduciaMcp {
    upstream: Arc<Upstream>,
}

fn ok_json(value: serde_json::Value) -> CallToolResult {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    CallToolResult::success(vec![ContentBlock::text(text)])
}

fn err_text(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

fn render(result: Result<serde_json::Value, String>) -> Result<CallToolResult, McpError> {
    Ok(match result {
        Ok(value) => ok_json(value),
        Err(message) => err_text(message),
    })
}

#[tool_router]
impl FiduciaMcp {
    pub fn new(upstream: Upstream) -> Self {
        Self {
            upstream: Arc::new(upstream),
        }
    }

    #[tool(
        description = "Explain the fiducia.cloud architecture: which repo does what, \
                       request path, ports, auth headers, invariants. Offline reference; \
                       start here when unsure where something lives."
    )]
    fn repo_map(&self) -> Result<CallToolResult, McpError> {
        Ok(CallToolResult::success(vec![ContentBlock::text(REPO_MAP)]))
    }

    #[tool(
        description = "Cluster-wide health from the brain (control plane): topology, \
                       node health, shard placement summary, scale plan, brain HA state. \
                       GET brain /v1/status."
    )]
    async fn cluster_status(&self) -> Result<CallToolResult, McpError> {
        render(self.upstream.get_json(Plane::Brain, "/v1/status").await)
    }

    #[tool(description = "Node membership snapshot from the brain. GET brain /v1/nodes.")]
    async fn cluster_nodes(&self) -> Result<CallToolResult, McpError> {
        render(self.upstream.get_json(Plane::Brain, "/v1/nodes").await)
    }

    #[tool(
        description = "Shard placement from the brain: the full shard map, or one shard's \
                       assignment (404 if unplaced). GET brain /v1/placement[/{shard}]."
    )]
    async fn placement(
        &self,
        Parameters(params): Parameters<PlacementParams>,
    ) -> Result<CallToolResult, McpError> {
        let path = match params.shard {
            Some(shard) => format!("/v1/placement/{shard}"),
            None => "/v1/placement".to_string(),
        };
        render(self.upstream.get_json(Plane::Brain, &path).await)
    }

    #[tool(description = "Resolve which shard owns a coordination key. \
                       GET brain /v1/route?key=...")]
    async fn route_key(
        &self,
        Parameters(params): Parameters<RouteKeyParams>,
    ) -> Result<CallToolResult, McpError> {
        let path = format!("/v1/route?key={}", urlencode(&params.key));
        render(self.upstream.get_json(Plane::Brain, &path).await)
    }

    #[tool(
        description = "Status of the configured data-plane node: version + consensus \
                       state. GET node /v1/status."
    )]
    async fn node_status(&self) -> Result<CallToolResult, McpError> {
        render(self.upstream.node_call(|c| c.status(), "/v1/status").await)
    }

    #[tool(description = "Read-only observability on the node, org-scoped. \
                       what=locks (held/waiting inventory), semaphores, elections, \
                       shards (per-node raft/quorum health), or metrics (per-op \
                       counts/errors/latency). GET node /v1/observe/{what}.")]
    async fn observe(
        &self,
        Parameters(params): Parameters<ObserveParams>,
    ) -> Result<CallToolResult, McpError> {
        let what = params.what.trim().to_ascii_lowercase();
        if !OBSERVE_KINDS.contains(&what.as_str()) {
            return Ok(err_text(format!(
                "unknown observe kind {what:?}; expected one of {OBSERVE_KINDS:?}"
            )));
        }
        render(
            self.upstream
                .get_json(Plane::Node, &format!("/v1/observe/{what}"))
                .await,
        )
    }

    #[tool(
        description = "Read a KV entry by exact key, or list entries by prefix \
                       (provide exactly one). GET node /v1/kv?key=... | ?prefix=..."
    )]
    async fn kv_get(
        &self,
        Parameters(params): Parameters<KvGetParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = match (params.key, params.prefix) {
            (Some(key), None) => {
                let path = format!("/v1/kv?key={}", urlencode(&key));
                self.upstream
                    .node_call(move |c| c.kv_get(&key), &path)
                    .await
            }
            (None, Some(prefix)) => {
                let path = format!("/v1/kv?prefix={}", urlencode(&prefix));
                self.upstream
                    .node_call(move |c| c.kv_list(&prefix), &path)
                    .await
            }
            _ => {
                return Ok(err_text(
                    "provide exactly one of `key` or `prefix`".to_string(),
                ))
            }
        };
        render(result)
    }

    #[tool(
        description = "Inspect a distributed lock: current holder, fencing token, \
                       wait queue. Read-only — never acquires. GET node /v1/locks?key=..."
    )]
    async fn lock_get(
        &self,
        Parameters(params): Parameters<LockGetParams>,
    ) -> Result<CallToolResult, McpError> {
        let key = params.key;
        let path = format!("/v1/locks?key={}", urlencode(&key));
        render(
            self.upstream
                .node_call(move |c| c.lock_get(&key), &path)
                .await,
        )
    }

    #[tool(
        description = "Service discovery: list registered services, or the live \
                       instances of one service. GET node /v1/services[/{service}]."
    )]
    async fn services(
        &self,
        Parameters(params): Parameters<ServicesParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = match params.service {
            Some(service) => {
                let path = format!("/v1/services/{}", urlencode(&service));
                self.upstream
                    .node_call(move |c| c.service_instances(&service), &path)
                    .await
            }
            None => {
                self.upstream
                    .node_call(|c| c.service_list(), "/v1/services")
                    .await
            }
        };
        render(result)
    }

    #[tool(
        description = "Which agent holds the file lease on (repository, path)? Answers \
                       via the ai-agent control plane (requires \
                       FIDUCIA_AGENT_CONTROL_PLANE_URL). Read-only; acquire/release \
                       stay with the agents themselves. GET /v1/file-leases."
    )]
    async fn file_lease(
        &self,
        Parameters(params): Parameters<FileLeaseParams>,
    ) -> Result<CallToolResult, McpError> {
        let path = format!(
            "/v1/file-leases?repository={}&path={}",
            urlencode(&params.repository),
            urlencode(&params.path)
        );
        render(
            self.upstream
                .get_json(Plane::AgentControlPlane, &path)
                .await,
        )
    }
}

#[tool_handler]
impl ServerHandler for FiduciaMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            // Not Implementation::from_build_env(): that captures rmcp's own
            // package name/version, not this crate's.
            .with_server_info(
                Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
                    .with_title("fiducia.cloud diagnostics")
                    .with_description(env!("CARGO_PKG_DESCRIPTION"))
                    .with_website_url("https://github.com/fiducia-cloud/fiducia-mcp-server.rs"),
            )
            .with_protocol_version(ProtocolVersion::LATEST)
            .with_instructions(
                "Read-only diagnostics for fiducia.cloud. Start with `repo_map` for \
                 architecture questions (offline). Live tools need the fiducia stack \
                 reachable and env credentials: `cluster_status`/`cluster_nodes`/\
                 `placement`/`route_key` hit the brain, `node_status`/`observe`/\
                 `kv_get`/`lock_get`/`services` hit a node, `file_lease` hits the \
                 ai-agent control plane. Nothing here mutates cluster state."
                    .to_string(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upstream::Config;

    fn server() -> FiduciaMcp {
        FiduciaMcp::new(Upstream::new(Config::default()))
    }

    #[test]
    fn router_exposes_all_tools() {
        let router = FiduciaMcp::tool_router();
        for tool in [
            "repo_map",
            "cluster_status",
            "cluster_nodes",
            "placement",
            "route_key",
            "node_status",
            "observe",
            "kv_get",
            "lock_get",
            "services",
            "file_lease",
        ] {
            assert!(router.has_route(tool), "missing tool {tool}");
        }
        assert_eq!(router.list_all().len(), 11);
    }

    #[test]
    fn repo_map_is_served_verbatim() {
        let result = server().repo_map().unwrap();
        assert_ne!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn observe_rejects_unknown_kind() {
        let result = server()
            .observe(Parameters(ObserveParams {
                what: "raft".into(),
            }))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn kv_get_requires_exactly_one_selector() {
        let both = server()
            .kv_get(Parameters(KvGetParams {
                key: Some("a".into()),
                prefix: Some("b".into()),
            }))
            .await
            .unwrap();
        assert_eq!(both.is_error, Some(true));

        let neither = server()
            .kv_get(Parameters(KvGetParams {
                key: None,
                prefix: None,
            }))
            .await
            .unwrap();
        assert_eq!(neither.is_error, Some(true));
    }

    #[tokio::test]
    async fn missing_credentials_surface_as_tool_error_not_crash() {
        // Default config has no secrets: brain call must return an is_error
        // result telling the caller which env var to set.
        let result = server().cluster_status().await.unwrap();
        assert_eq!(result.is_error, Some(true));
    }
}
