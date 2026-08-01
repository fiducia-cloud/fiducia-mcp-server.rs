# Fiducia platform architecture: control planes and the request path

Synced against the fiducia-cloud org (`gh repo list`) on 2026-08-01. This is the
prose companion to the `repo_map` tool: it explains how the brain, node, edge,
load-balancer, auth, admin, and AI-agent control planes fit together and who
owns what. The condensed, machine-friendly version is served live by `repo_map`.

fiducia.cloud is a coordination service for distributed systems and AI agents:
distributed locks, semaphores, RW-locks, KV with watches, leader election,
service discovery, counters, barriers, rate limits, idempotency, and
agent-oriented primitives (file leases, work items) — built on sharded
multi-Raft. Authority (leases, fencing) lives in the node; NATS JetStream is
messaging; Postgres/Supabase hold only non-authoritative state.

## Request path (front door to authority)

```
client
  -> fiducia-edge            Cloudflare Worker: global entry, region selection
  -> fiducia-load-balance.rs :8088/:8443 — key-aware edge LB; bearer auth via
                             fiducia-auth; routes to the owning shard's leader;
                             injects trusted-hop headers
                             (x-fiducia-internal-auth + x-fiducia-org-id)
  -> fiducia-node.rs         :8090 client plane / :9090 raft peer plane — the
                             data plane and source of authority
```

`fiducia-brain.rs` (:8095) sits beside this path as the control plane, and
`fiducia-node-sidecar.rs` bridges each node to the brain and the observability
stack. `fiducia-routing.rs` is the shared `fnv1a + shard_for` key->shard
function — the single source of truth used by both the LB and the node so they
never disagree about which shard owns a key.

## The control planes

| Plane | Repo | Port(s) | Owns |
|---|---|---|---|
| Data | `fiducia-node.rs` | 8090 / 9090 | Sharded multi-Raft engine: locks, KV+watches, leader election, service discovery, semaphores, counters, barriers, tasks, claims, idempotency, rate limits, cron. The authority. |
| Control | `fiducia-brain.rs` | 8095 | Shard placement, scaling, node-failure handling. `GET /v1/status`, `/v1/nodes`, `/v1/placement`, `/v1/route?key=`. |
| Edge | `fiducia-edge` | — | Cloudflare Worker: global entry point, region selection, edge concerns. |
| Load balance | `fiducia-load-balance.rs` | 8088 / 8443 | Key-aware routing to the owning shard's leader; bearer auth at the edge; strips client-supplied internal headers and injects trusted-hop scoping. Debug: `/_lb/routes`, `/_lb/resolve`. |
| Identity | `fiducia-auth.rs` | — | Supabase dashboard sessions + hashed B2B API keys; cached introspection (`POST /v1/introspect`, `x-server-auth`) and key->JWT exchange. Stores hashed keys in node KV under `__auth/` (dogfooding). |
| Admin | `fiducia-admin.rs` | 8096 | Operator-only server-rendered dashboard (MASH stack): accounts + API keys (via fiducia-auth) and infra ops (via fiducia-brain). `WS /admin/ws` streams fiducia-sync change frames. |
| AI-agent | `fiducia-ai-agent-control-plane` | — | Stateless API over the node for agent coordination: file leases (`/v1/file-leases` get/acquire/release, `x-internal-auth`), agents, work items. |

### Per-plane auth (server-to-server)

- **node + brain:** `x-fiducia-internal-auth` = `FIDUCIA_INTERNAL_SECRET`; the
  node additionally **requires** `x-fiducia-org-id` for tenant scoping. Fail
  closed when the secret is unset.
- **ai-agent control plane:** `x-internal-auth` = `FIDUCIA_CONTROL_PLANE_SECRET`
  (falls back to `FIDUCIA_INTERNAL_SECRET`).
- **fiducia-auth introspection:** `x-server-auth` = `FIDUCIA_INTROSPECT_SECRET`.
- **External clients:** `Authorization: Bearer <api key or JWT>` at the LB,
  which strips any client-supplied internal headers before forwarding.

These are easy to mix up; the header-per-plane split is enforced in this
server's `src/upstream.rs` and its unit tests.

## The AI-agent layer around the control plane

- `fiducia-ai-agent-bridge.rs` — topic-routed agent chatrooms (HTTP :8142 /
  TCP :8143) and the file-lease authority the control plane fronts.
- `fiducia-ai-agent-manager.rs` — agent lifecycle manager (durable file outbox
  -> JetStream; Core-NATS for disposable live progress).
- `fiducia-memory.rs` / `fiducia-memory` — shared brain for agents: a durable,
  tenant-scoped claims ledger plus hybrid recall. Vectors suggest; they never
  control authoritative state.
- `fiducia-mcp-server.rs` — this read-only MCP diagnostics server.

## Supporting services and shared libraries

| Repo | Role |
|---|---|
| `fiducia-routing.rs` | Shared key->shard routing (`fnv1a + shard_for`); single source of truth for LB and node. |
| `fiducia-node-sidecar.rs` | Per-node sidecar: control-plane heartbeat/metadata + logs/metrics. |
| `fiducia-telemetry.rs` | Shared OpenTelemetry + tracing init (tag **v0.2.1**, OpenTelemetry 0.32 pipeline; `fiducia.service.starts` counter). Pinned fleet-wide by every Rust service. |
| `fiducia-messaging.rs` / `fiducia-messaging` | Versioned NATS envelopes with a transactional Postgres outbox/inbox. |
| `fiducia-customer.rs` | Canonical customer web app + BFF (:8080, MASH: Maud/Axum/SeaORM/Supabase/HTMX); mounts the fiducia-payments routes. |
| `fiducia-payments.rs` | Provider-agnostic Stripe + PayPal webhook signature verification and event parsing (pure library, offline-testable). |
| `fiducia-auth.rs` | (see control planes above) identity for both customer and admin surfaces. |
| `fiducia-clients` | Official HTTP client libraries in 12 languages; the Rust `fiducia-client` (`clients/rust`) is the sanctioned node-plane caller. |
| `fiducia-interfaces` | JSON Schema (typed-IO) + canonical SQL, codegen to Rust/TS/Python/Go; owns the sync contracts. |
| `fiducia-sync` | Local-first sync SDK: zero-IO Rust core (native + WASM), Postgres change-journal adapter, `@fiducia/sync` browser SDK, Dart/Flutter package. |
| `fiducia-cli.rs` | `fiducia` CLI (closest-region latency probe, data-plane calls). |
| `fiducia-infra` | Multi-cluster Kubernetes (GCP + AWS + third platform); survives losing any 1 of 3 clusters via cluster-level Raft quorum. |
| `fiducia-marketing.web` | Static Astro marketing site (renamed from `fiducia-ui.web`, 2026-07). |
| `fiducia-cloud.github.io` | Public marketing website: the org's GitHub Pages Astro site. |
| `fiducia-test-config` | Shared browser-test harness (`@fiducia/test-config`). |
| `fiducia-e2e` / `fiducia-operations-control-plane` / `fiducia-lambda-service.rs` / `fiducia-monorepo` | E2E suite, ops control plane, lambda-style service runner, and the superproject pinning every repo as a submodule. |

`fiducia-customer-ui.web` is **archived** (legacy Vite SPA); the canonical
customer frontend is the server-rendered surface in `fiducia-customer.rs`.

## Observability lifecycle (fiducia-telemetry v0.2.1)

Every Rust service links `fiducia-telemetry` at tag **v0.2.1** (OpenTelemetry
0.32 pipeline). One `init()` gives the whole process:

- JSON structured logs to stdout, node-collected and routed to **Loki**;
- OTLP/gRPC traces and metrics to a local collector -> **Prometheus** when
  `OTEL_EXPORTER_OTLP_ENDPOINT` is set (stdout-only in local dev);
- a low-cardinality `fiducia.service.starts` counter that also proves the
  service -> collector -> Prometheus path is live.

Endpoints: dd-prometheus:9090, dd-loki:3100, Grafana at `/telemetry`, OTLP
:4317. The node exposes `/v1/observe/{locks,semaphores,elections,shards,
metrics}`; brain and memory expose `/v1/status`. No Alertmanager yet.

**This MCP server is the one deliberate exception:** it does *not* use
`fiducia-telemetry`, because that library's fallback logger writes to stdout and
stdout here is the MCP wire. It ships its own stdio-safe telemetry module
(`src/telemetry.rs`) that keeps all logging on stderr.
