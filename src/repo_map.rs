//! Embedded, offline reference: what lives where in the fiducia-cloud org.
//! Served by the `repo_map` tool so agents can orient without cloning
//! everything. Update this when repos are added, renamed, or archived.

pub const REPO_MAP: &str = r#"# fiducia.cloud — org / architecture map

fiducia.cloud is a coordination service for distributed systems and AI agents:
distributed locks, semaphores, RW-locks, KV with watches, leader election,
service discovery, counters, barriers, rate limits, idempotency, and
agent-oriented primitives (file leases, work items) — built on sharded
multi-Raft.

## Core invariants

- NATS JetStream = messaging; fiducia-node = authority (leases/fencing);
  CockroachDB/Postgres = state. There is no "fiducia-mq".
- Every mutation that matters is fenced: lock grants carry fencing tokens and
  downstream systems must check them.
- Vectors suggest, never control, authoritative state (fiducia-memory).

## Request path

client -> fiducia-edge (Cloudflare Worker, region pick)
       -> fiducia-load-balance (:8088/:8443, bearer auth via fiducia-auth,
          key-aware routing to the owning shard's leader; injects trusted-hop
          headers x-fiducia-internal-auth + x-fiducia-org-id)
       -> fiducia-node (:8090 client plane, :9090 raft peer plane)
fiducia-brain (:8095) is the control plane: shard placement, scaling,
node-failure handling. fiducia-node-sidecar bridges each node to the brain
(heartbeats) and the observability stack.

## Repos (dir name = repo name in github.com/fiducia-cloud)

Data/control plane (Rust):
- fiducia-node.rs — data plane: sharded multi-Raft engine (locks, KV+watches,
  leader election, service discovery, semaphores, counters, barriers, tasks,
  effects, handoffs, decisions, budgets, claims, idempotency, rate-limit, cron).
- fiducia-brain.rs — control plane: placement, scaling, failure handling.
  GET /v1/status, /v1/nodes, /v1/placement, /v1/route?key=...
- fiducia-load-balance.rs — edge key-aware LB; debug endpoints /_lb/routes,
  /_lb/resolve.
- fiducia-node-sidecar.rs — per-node heartbeat/metadata + logs/metrics.
- fiducia-routing.rs — shared key->shard routing (fnv1a + shard_for), the
  single source of truth used by LB and node.
- fiducia-edge — Cloudflare Worker: global entry, region selection.

Identity, customer, admin:
- fiducia-auth.rs — Supabase dashboard sessions + B2B API keys (hashed),
  cached introspection (POST /v1/introspect, x-server-auth), key->JWT exchange.
- fiducia-customer.rs — canonical customer web app + BFF (Rust MASH: Maud,
  Axum, SeaORM/Supabase, HTMX). Crate/image/k8s still named fiducia-backend.
- fiducia-admin.rs — operator-only admin dashboard (accounts, API keys, infra).
- fiducia-marketing.web — static Astro marketing site (GitHub Pages +
  synced fallback into fiducia-customer.rs/static/).
- fiducia-customer-ui.web — ARCHIVED legacy SPA; do not touch.

AI-agent layer:
- fiducia-ai-agent-control-plane — stateless API over the node for agent
  coordination: file leases (/v1/file-leases get/acquire/release; auth header
  x-internal-auth), agents, work items. The bridge is the file-lease authority.
- fiducia-ai-agent-bridge.rs — topic-routed agent chatrooms (HTTP :8142 /
  TCP :8143) + file-lease authority integration.
- fiducia-ai-agent-manager.rs — agent lifecycle manager.
- fiducia-memory.rs / fiducia-memory — shared brain: tenant-scoped claims
  ledger + hybrid recall for agents.
- fiducia-mcp-server.rs — this MCP server.

Messaging, clients, interfaces:
- fiducia-messaging.rs / fiducia-messaging — versioned NATS envelopes with
  transactional Postgres outbox/inbox.
- fiducia-clients — official HTTP client libraries in 12 languages; the Rust
  crate is fiducia-client at clients/rust (blocking, ureq).
- fiducia-interfaces — JSON Schema (typed-IO) + canonical SQL, codegen to
  Rust/TS/Python/Go; generated/rust is a common path dependency.
- fiducia-sync — local-first sync SDK (@fiducia/sync), Rust core -> WASM.
- fiducia-cli.rs — `fiducia` CLI (closest-region probe, data-plane calls).
- fiducia-telemetry.rs — shared OpenTelemetry init for services (stdout/OTLP;
  NOT used by this MCP server because stdout is the MCP wire).

Infra, testing, meta:
- fiducia-infra — multi-cluster Kubernetes (GCP + AWS + third platform),
  survives losing any 1 of 3 clusters via cluster-level Raft quorum.
- fiducia-e2e — end-to-end tests (private).
- fiducia-test-config — shared browser-test harness (@fiducia/test-config).
- fiducia-operations-control-plane — ops control plane (private).
- fiducia-lambda-service.rs — lambda-style service runner.
- fiducia-monorepo — superproject pinning all repos as git submodules.

## Auth cheat sheet (server-to-server)

- node + brain: header x-fiducia-internal-auth = FIDUCIA_INTERNAL_SECRET;
  node additionally REQUIRES x-fiducia-org-id (tenant scoping). Fail closed
  when the secret is unset.
- ai-agent control plane: header x-internal-auth =
  FIDUCIA_CONTROL_PLANE_SECRET (falls back to FIDUCIA_INTERNAL_SECRET).
- fiducia-auth introspection: header x-server-auth = FIDUCIA_INTROSPECT_SECRET.
- External clients: Authorization: Bearer <api key or JWT> at the LB; the LB
  strips any client-supplied internal headers.

## Observability

dd-prometheus:9090, dd-loki:3100, Grafana at /telemetry, OTLP :4317.
Node exposes /v1/observe/{locks,semaphores,elections,shards,metrics};
brain and memory expose /v1/status. No Alertmanager yet.

## Hosting

fiducia.cloud = GitHub Pages (marketing). app./admin. = Hetzner edge
95.217.171.250 via dd-remote-gateway.
"#;

#[cfg(test)]
mod tests {
    use super::REPO_MAP;

    /// The map is what agents use to orient; if it drifts behind a repo
    /// rename it actively misroutes them. Pin: the renamed repos appear
    /// under their CURRENT names, and any mention of a pre-rename name is
    /// explicitly flagged as renamed/archived/deprecated right where it
    /// appears.
    #[test]
    fn repo_map_uses_renamed_names_and_marks_stale_ones() {
        for current in ["fiducia-customer.rs", "fiducia-marketing.web"] {
            assert!(
                REPO_MAP.contains(current),
                "repo map must name the renamed repo {current:?}"
            );
        }

        let stale = [
            "fiducia-customer-ui.web",
            "fiducia-backend.rs",
            "fiducia-ui.web",
        ];
        let markers = ["renamed", "archived", "deprecated", "legacy", "still named"];
        let lines: Vec<&str> = REPO_MAP.lines().collect();
        for name in stale {
            for (i, line) in lines.iter().enumerate() {
                if !line.contains(name) {
                    continue;
                }
                let window = lines[i.saturating_sub(1)..(i + 2).min(lines.len())]
                    .join("\n")
                    .to_ascii_lowercase();
                assert!(
                    markers.iter().any(|m| window.contains(m)),
                    "stale repo name {name:?} appears without a rename/archive \
                     marker nearby:\n{line}"
                );
            }
        }
    }
}
