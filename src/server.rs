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

use crate::cloudflare::{Cloudflare, UpsertParams};
use crate::domains::{self, DnsCheckInput, SystemResolver};
use crate::k8s;
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

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CloudflareZoneParams {
    /// Zone name (e.g. "fiducia.cloud") or a 32-char zone id.
    pub zone: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CloudflareUpsertParams {
    /// Zone name or id the record lives in.
    pub zone: String,
    /// Record type: one of A, AAAA, CNAME, TXT, MX.
    #[serde(rename = "type")]
    pub record_type: String,
    /// Record name (FQDN, e.g. "app.fiducia.cloud").
    pub name: String,
    /// Record value (IP, hostname, or text content).
    pub content: String,
    /// TTL in seconds; 1 = automatic (default).
    #[serde(default)]
    pub ttl: Option<i64>,
    /// Proxy through Cloudflare — only honored for A/AAAA/CNAME (default false).
    #[serde(default)]
    pub proxied: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CloudflareDeleteParams {
    /// Zone name or id.
    pub zone: String,
    /// Explicit DNS record id to delete.
    pub record_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DomainParams {
    /// Domain to look up, e.g. "fiducia.cloud".
    pub domain: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DnsCheckParams {
    /// Domain to check, e.g. "app.fiducia.cloud". Omit when using `preset`.
    #[serde(default)]
    pub name: Option<String>,
    /// Record type (A, AAAA, CNAME, NS, TXT, MX). Defaults to A when `name` is set.
    #[serde(default, rename = "type")]
    pub record_type: Option<String>,
    /// Expected values; the check PASSes if any is observed.
    #[serde(default)]
    pub values: Option<Vec<String>>,
    /// Built-in check set; use "fiducia" for the fiducia.cloud DNS cutover.
    #[serde(default)]
    pub preset: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct K8sWorkloadsParams {
    /// kubectl context (validated against `kubectl config get-contexts`).
    pub context: String,
    /// Namespace (default "fiducia").
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct K8sRolloutParams {
    /// kubectl context (validated before use).
    pub context: String,
    /// "deployment" or "statefulset".
    pub kind: String,
    /// Workload name.
    pub name: String,
    /// Namespace (default "fiducia").
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct K8sEventsParams {
    /// kubectl context (validated before use).
    pub context: String,
    /// Namespace (default "fiducia").
    #[serde(default)]
    pub namespace: Option<String>,
    /// How many recent events to return (default 30).
    #[serde(default)]
    pub last: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct K8sServiceParams {
    /// kubectl context (validated before use).
    pub context: String,
    /// Namespace (default "fiducia").
    #[serde(default)]
    pub namespace: Option<String>,
    /// Service name to inspect endpoints for.
    pub service: String,
}

const OBSERVE_KINDS: [&str; 5] = ["locks", "semaphores", "elections", "shards", "metrics"];

#[derive(Clone)]
pub struct FiduciaMcp {
    upstream: Arc<Upstream>,
    cloudflare: Arc<Cloudflare>,
    /// Dedicated client for RDAP: redirects disabled so we follow exactly one
    /// hop from the bootstrap server ourselves.
    rdap_client: reqwest::Client,
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
        let rdap_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client");
        Self {
            upstream: Arc::new(upstream),
            cloudflare: Arc::new(Cloudflare::from_env()),
            rdap_client,
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

    // ---- Cloudflare DNS (needs CLOUDFLARE_API_TOKEN) ----

    #[tool(
        description = "List Cloudflare zones on the account: name, id, status, \
                       nameservers. GET Cloudflare /zones."
    )]
    async fn cloudflare_zones(&self) -> Result<CallToolResult, McpError> {
        render(self.cloudflare.zones().await)
    }

    #[tool(
        description = "List DNS records in a Cloudflare zone (accepts a zone name or \
                       id), following pagination → [{id,type,name,content,proxied,ttl}]."
    )]
    async fn cloudflare_dns_records(
        &self,
        Parameters(params): Parameters<CloudflareZoneParams>,
    ) -> Result<CallToolResult, McpError> {
        render(self.cloudflare.dns_records(&params.zone).await)
    }

    #[tool(
        description = "MUTATION (gated by FIDUCIA_MCP_ALLOW_MUTATIONS=1): create or \
                       update a DNS record, matched on (type, name). Allowed types: \
                       A/AAAA/CNAME/TXT/MX. POST if absent, else PUT."
    )]
    async fn cloudflare_dns_upsert(
        &self,
        Parameters(params): Parameters<CloudflareUpsertParams>,
    ) -> Result<CallToolResult, McpError> {
        render(
            self.cloudflare
                .dns_upsert(UpsertParams {
                    zone: params.zone,
                    record_type: params.record_type,
                    name: params.name,
                    content: params.content,
                    ttl: params.ttl,
                    proxied: params.proxied,
                })
                .await,
        )
    }

    #[tool(
        description = "MUTATION (gated by FIDUCIA_MCP_ALLOW_MUTATIONS=1): delete a DNS \
                       record by explicit id in a Cloudflare zone."
    )]
    async fn cloudflare_dns_delete(
        &self,
        Parameters(params): Parameters<CloudflareDeleteParams>,
    ) -> Result<CallToolResult, McpError> {
        render(
            self.cloudflare
                .dns_delete(&params.zone, &params.record_id)
                .await,
        )
    }

    // ---- Domains: RDAP + external DNS verification ----

    #[tool(
        description = "Registrar, nameservers, status, and expiry for a domain via RDAP \
                       (Squarespace-registered domains expose no DNS API). Follows one \
                       redirect to the authoritative registry."
    )]
    async fn domain_registrar_status(
        &self,
        Parameters(params): Parameters<DomainParams>,
    ) -> Result<CallToolResult, McpError> {
        render(
            domains::registrar_status(&self.rdap_client, domains::RDAP_BASE, &params.domain).await,
        )
    }

    #[tool(
        description = "Verify live DNS from the outside. Give `name` (+ optional `type` \
                       and `values`), or preset:\"fiducia\" to check the GitHub Pages / \
                       Hetzner edge / Cloudflare-nameserver cutover. Reports per-record \
                       PASS / PENDING / MISMATCH."
    )]
    async fn dns_check(
        &self,
        Parameters(params): Parameters<DnsCheckParams>,
    ) -> Result<CallToolResult, McpError> {
        let resolver = SystemResolver::new();
        render(
            domains::dns_check(
                &resolver,
                DnsCheckInput {
                    name: params.name,
                    record_type: params.record_type,
                    values: params.values,
                    preset: params.preset,
                },
            )
            .await,
        )
    }

    // ---- Kubernetes: read-only kubectl (respects KUBECONFIG) ----

    #[tool(description = "List kubectl contexts and mark which are allowed (per \
                       FIDUCIA_K8S_CONTEXTS). kubectl config get-contexts.")]
    async fn k8s_contexts(&self) -> Result<CallToolResult, McpError> {
        render(k8s::contexts().await)
    }

    #[tool(
        description = "Deployments/statefulsets (ready/desired + images) and pods \
                       (phase/restarts/node) in a namespace (default \"fiducia\"). \
                       Read-only kubectl get -o json."
    )]
    async fn k8s_workloads(
        &self,
        Parameters(params): Parameters<K8sWorkloadsParams>,
    ) -> Result<CallToolResult, McpError> {
        render(k8s::workloads(&params.context, params.namespace.as_deref().unwrap_or("")).await)
    }

    #[tool(description = "Current rollout status of a deployment or statefulset \
                       (non-blocking). kubectl rollout status --watch=false.")]
    async fn k8s_rollout_status(
        &self,
        Parameters(params): Parameters<K8sRolloutParams>,
    ) -> Result<CallToolResult, McpError> {
        render(
            k8s::rollout_status(
                &params.context,
                &params.kind,
                &params.name,
                params.namespace.as_deref().unwrap_or(""),
            )
            .await,
        )
    }

    #[tool(
        description = "Most recent events in a namespace (default 30) by lastTimestamp. \
                       kubectl get events -o json."
    )]
    async fn k8s_events(
        &self,
        Parameters(params): Parameters<K8sEventsParams>,
    ) -> Result<CallToolResult, McpError> {
        render(
            k8s::events(
                &params.context,
                params.namespace.as_deref().unwrap_or(""),
                params.last.unwrap_or(0),
            )
            .await,
        )
    }

    #[tool(description = "Ready and not-ready backend addresses for a service. \
                       kubectl get endpoints -o json.")]
    async fn k8s_service_endpoints(
        &self,
        Parameters(params): Parameters<K8sServiceParams>,
    ) -> Result<CallToolResult, McpError> {
        render(
            k8s::service_endpoints(
                &params.context,
                params.namespace.as_deref().unwrap_or(""),
                &params.service,
            )
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
                 ai-agent control plane. `cloudflare_*` manage DNS (CLOUDFLARE_API_TOKEN); \
                 `domain_registrar_status`/`dns_check` verify domains from outside; \
                 `k8s_*` run read-only kubectl. The ONLY tools that mutate anything are \
                 `cloudflare_dns_upsert`/`cloudflare_dns_delete`, and only when \
                 FIDUCIA_MCP_ALLOW_MUTATIONS=1; everything else is read-only."
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
            "cloudflare_zones",
            "cloudflare_dns_records",
            "cloudflare_dns_upsert",
            "cloudflare_dns_delete",
            "domain_registrar_status",
            "dns_check",
            "k8s_contexts",
            "k8s_workloads",
            "k8s_rollout_status",
            "k8s_events",
            "k8s_service_endpoints",
        ] {
            assert!(router.has_route(tool), "missing tool {tool}");
        }
        assert_eq!(router.list_all().len(), 22);
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

    /// stdout/stdin is the MCP wire: one garbage line from a confused client
    /// must not kill the service loop. Serve the real handler over an
    /// in-memory duplex, feed it a non-JSON line followed by a valid
    /// `initialize` request, and require a correct response to the latter.
    #[tokio::test]
    async fn garbage_stdio_line_is_ignored_and_next_request_served() {
        use rmcp::ServiceExt;
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (client_read, mut client_write) = tokio::io::split(client_io);

        let service = tokio::spawn(async move { server().serve(server_io).await });

        client_write
            .write_all(b"this is not json {{{\n")
            .await
            .unwrap();
        let init = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": serde_json::to_value(ProtocolVersion::LATEST).unwrap(),
                "capabilities": {},
                "clientInfo": { "name": "loop-test", "version": "0.0.0" },
            },
        });
        client_write
            .write_all(format!("{init}\n").as_bytes())
            .await
            .unwrap();

        let mut lines = BufReader::new(client_read).lines();
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), lines.next_line())
            .await
            .expect("server did not answer within deadline")
            .expect("read error on client side")
            .expect("server closed the stream after a garbage line");
        let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            reply["id"], 1,
            "response must correlate to the valid request"
        );
        assert!(
            reply.get("error").is_none(),
            "valid initialize after a garbage line must not error: {reply}"
        );
        assert_eq!(
            reply["result"]["serverInfo"]["name"],
            env!("CARGO_PKG_NAME"),
            "initialize must return this server's identity"
        );

        service.abort();
    }

    #[tokio::test]
    async fn missing_credentials_surface_as_tool_error_not_crash() {
        // Default config has no secrets: brain call must return an is_error
        // result telling the caller which env var to set.
        let result = server().cluster_status().await.unwrap();
        assert_eq!(result.is_error, Some(true));
    }
}
