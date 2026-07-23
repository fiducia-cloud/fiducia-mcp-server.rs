# fiducia-mcp-server.rs

MCP (Model Context Protocol) server for fiducia.cloud. Gives coding agents
(Claude Code, Codex, anything MCP-capable) read-only diagnostic tools over the
fiducia stack, plus an embedded map of the whole org so agents can orient
without cloning every repo.

Binary: `fiducia-mcp` — speaks MCP over **stdio** (stdout is the wire; all
logs go to stderr).

Platform reference docs live in [`docs/`](docs/): where secrets/KV are
persisted and how consumers ingest them ([secrets-and-kv.md](docs/secrets-and-kv.md)),
NATS/JetStream design + hardening invariants ([nats-and-messaging.md](docs/nats-and-messaging.md)),
and the admin/customer MASH web stacks + browser testing
([web-stacks-and-testing.md](docs/web-stacks-and-testing.md)). The same facts
in condensed form are served live by the `repo_map` tool.

## Tools

Tools are **read-only by default**. The *only* exceptions are two Cloudflare
DNS write tools (`cloudflare_dns_upsert`, `cloudflare_dns_delete`), which stay
locked unless `FIDUCIA_MCP_ALLOW_MUTATIONS=1` is set — see
[Configuration](#configuration-env). Cluster mutations — acquiring locks,
moving shards, releasing leases — deliberately stay with the real clients
(`fiducia-client`, `fiducia` CLI) where fencing tokens are handled properly.

Node data-plane tools (`node_status`, `kv_get`, `lock_get`, `services`) go
through the official Rust client — `fiducia-client`, a path dependency on the
sibling checkout `../fiducia-clients/clients/rust` — in both internal and
bearer modes. `observe` uses the same client. Plain HTTP remains only for the
brain, agent control plane, Cloudflare, RDAP, and other non-node APIs.

| Tool | Upstream | What it answers |
|---|---|---|
| `repo_map` | none (embedded) | What repo does what? Ports, auth headers, invariants. |
| `cluster_status` | brain `GET /v1/status` | Is the cluster healthy? Topology, placement summary, scale plan, brain HA. |
| `cluster_nodes` | brain `GET /v1/nodes` | Node membership snapshot. |
| `placement` | brain `GET /v1/placement[/{shard}]` | Full shard map, or one shard's assignment. |
| `route_key` | brain `GET /v1/route?key=` | Which shard owns this key? |
| `node_status` | node `GET /v1/status` | Node version + consensus state. |
| `observe` | node `GET /v1/observe/{what}` | `locks` / `semaphores` / `elections` / `shards` / `metrics` (org-scoped). |
| `kv_get` | node `GET /v1/kv?key=` or `?prefix=` | Read a KV entry or list a prefix. |
| `lock_get` | node `GET /v1/locks?key=` | Who holds this lock? Fencing token, wait queue. |
| `services` | node `GET /v1/services[/{name}]` | Service discovery: services or live instances. |
| `file_lease` | agent control plane `GET /v1/file-leases` | Which agent holds the lease on (repository, path)? |
| `cloudflare_zones` | Cloudflare `GET /zones` | Zones on the account: name, id, status, nameservers. |
| `cloudflare_dns_records` | Cloudflare `GET /zones/{id}/dns_records` | DNS records in a zone (name or id), paginated. |
| `cloudflare_dns_upsert` **⚠︎ write** | Cloudflare `POST`/`PUT .../dns_records` | Create-or-update a record (type,name). **Gated.** |
| `cloudflare_dns_delete` **⚠︎ write** | Cloudflare `DELETE .../dns_records/{id}` | Delete a record by id. **Gated.** |
| `domain_registrar_status` | RDAP `GET /domain/{d}` | Registrar, nameservers, status, expiry (via rdap.org). |
| `dns_check` | resolver (hickory) | Verify live DNS; `preset:"fiducia"` checks the whole cutover. |
| `k8s_contexts` | `kubectl config get-contexts` | Known contexts + which are allowed. |
| `k8s_workloads` | `kubectl get deploy,sts,pods -o json` | Deployments/statefulsets ready/desired + images + pods. |
| `k8s_rollout_status` | `kubectl rollout status --watch=false` | Current rollout state of a deployment/statefulset. |
| `k8s_events` | `kubectl get events -o json` | Most recent events in a namespace (default 30). |
| `k8s_service_endpoints` | `kubectl get endpoints -o json` | Ready / not-ready backend addresses for a service. |

**⚠︎ write** marks the only two mutating tools. Both refuse to run unless
`FIDUCIA_MCP_ALLOW_MUTATIONS=1`; without it they return an error explaining the
gate and never call the API.

## Configuration (env)

| Variable | Default | Purpose |
|---|---|---|
| `FIDUCIA_NODE_URL` | `http://localhost:8090` | Node data plane (or the LB URL in bearer mode). |
| `FIDUCIA_BRAIN_URL` | `http://localhost:8095` | Brain control plane. |
| `FIDUCIA_AGENT_CONTROL_PLANE_URL` | unset | ai-agent control plane; required only for `file_lease`. |
| `FIDUCIA_INTERNAL_SECRET` | unset | Trusted-hop secret → `x-fiducia-internal-auth` (node + brain). |
| `FIDUCIA_ORG_ID` | unset | Tenant → `x-fiducia-org-id`; required for direct node calls. |
| `FIDUCIA_CONTROL_PLANE_SECRET` | falls back to `FIDUCIA_INTERNAL_SECRET` | → `x-internal-auth` on the agent control plane. |
| `FIDUCIA_API_KEY` | unset | Bearer mode: node-plane calls send `Authorization: Bearer` instead of internal headers — point `FIDUCIA_NODE_URL` at the load balancer. |
| `CLOUDFLARE_API_TOKEN` | unset | Cloudflare v4 API token (`Zone:Read` + `DNS:Edit`) → `Authorization: Bearer`. Required for the `cloudflare_*` tools; never logged. |
| `FIDUCIA_MCP_ALLOW_MUTATIONS` | unset | Set to `1` to unlock the two Cloudflare DNS write tools. Anything else keeps them (and the whole server) read-only. |
| `FIDUCIA_K8S_CONTEXTS` | unset | Optional CSV allowlist restricting which kubectl contexts the `k8s_*` tools may use. Unset = any known context. |
| `KUBECONFIG` | kubectl default | Honored transparently — the `k8s_*` tools shell out to `kubectl`, which reads it. |

Two ways to reach the data plane:

1. **Direct / port-forward (internal mode):** set `FIDUCIA_INTERNAL_SECRET` +
   `FIDUCIA_ORG_ID`; the server attaches the same trusted-hop headers the LB
   would inject.
2. **Through the load balancer (bearer mode):** set `FIDUCIA_API_KEY` and
   point `FIDUCIA_NODE_URL` at the LB; the LB authenticates the key and
   injects org scoping itself. Brain calls still need the internal secret.

Missing credentials never crash the server — the affected tool returns an
error naming the env var to set.

## Domains

The fiducia.cloud domains are registered at **Squarespace, which exposes no
public DNS write API**. So this server doesn't try to manage DNS at the
registrar — it *verifies* the live state from the outside and leaves writes to
Cloudflare:

- `domain_registrar_status` reads registrar, nameservers, status, and expiry
  over **RDAP** (`rdap.org`, following one redirect to the authoritative
  registry). No credentials needed.
- `dns_check` resolves records with [hickory-resolver] (system config, falling
  back to `1.1.1.1` / `8.8.8.8`) and compares them to expectations, reporting
  per-record **PASS / PENDING / MISMATCH**. `preset:"fiducia"` checks the whole
  cutover in one shot: `fiducia.cloud` + `www` → GitHub Pages, `app.` + `admin.`
  → the Hetzner edge (`95.217.171.250`), and whether `fiducia.cloud`'s
  nameservers are `*.ns.cloudflare.com`.

Once the registrable domain's nameservers point at Cloudflare, actual DNS
writes go through the gated `cloudflare_dns_upsert` / `cloudflare_dns_delete`
tools. All resolver lookups sit behind a small `Resolve` trait, so the tests
inject a mock and run fully offline.

[hickory-resolver]: https://crates.io/crates/hickory-resolver

## Kubernetes

The `k8s_*` tools shell out to **`kubectl`** (no `kube-rs` dependency —
`kubectl` already carries the operator's kubeconfig, contexts, and auth
plugins, and honors `$KUBECONFIG`). Guardrails:

- **Read-only verbs only:** `config get-contexts`, `get … -o json`,
  `rollout status --watch=false`, `top`. Nothing applies, scales, or deletes.
- **Context validation:** every `--context` is checked against
  `kubectl config get-contexts -o name` before use, and can be further
  restricted with `FIDUCIA_K8S_CONTEXTS`.
- **argv, never a shell string** (no word-splitting), and a **15s timeout** per
  call. Large JSON summaries are truncated to ~32KB with a note.

## Install & register

```sh
cargo install --path .   # installs `fiducia-mcp` into ~/.cargo/bin

# Claude Code:
claude mcp add fiducia \
  --env FIDUCIA_INTERNAL_SECRET=... \
  --env FIDUCIA_ORG_ID=... \
  -- fiducia-mcp
```

Or in a project `.mcp.json`:

```json
{
  "mcpServers": {
    "fiducia": {
      "command": "fiducia-mcp",
      "env": {
        "FIDUCIA_NODE_URL": "http://localhost:8090",
        "FIDUCIA_BRAIN_URL": "http://localhost:8095",
        "FIDUCIA_INTERNAL_SECRET": "…",
        "FIDUCIA_ORG_ID": "…"
      }
    }
  }
}
```

## Development

Building requires the sibling checkout `../fiducia-clients` (org convention:
repos live side by side under the `fiducia.cloud` workspace or as
`fiducia-monorepo/apps/*` submodules).

```sh
cargo test --locked
cargo run --locked   # then paste MCP JSON-RPC on stdin, e.g. an initialize request
```

## Container

Build from this repository; the Docker build reproduces the sibling client
dependency at its reviewed commit:

```sh
docker build --tag fiducia-mcp:local .
```

The runtime is an explicit non-root tool runner (UID/GID 65532) because the
read-only Kubernetes diagnostics invoke a checksum-verified `kubectl`. The MCP
server still communicates over stdio, and its stdout remains reserved for the
MCP protocol.

Built on the official Rust MCP SDK ([rmcp](https://crates.io/crates/rmcp)).

## OpenTelemetry

Set `OTEL_EXPORTER_OTLP_ENDPOINT` to export explicit OTLP/gRPC traces and
metrics; use `RUST_LOG` for filtering. Each MCP tool call gets a named span,
call counter, duration histogram, and error flag. Arguments, results, and
secrets are never recorded. JSON logs stay on stderr and stdout stays reserved
for MCP framing. Instrumentation is explicit Rust code—no monkey patching.
