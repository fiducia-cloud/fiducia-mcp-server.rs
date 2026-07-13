# fiducia-mcp-server.rs

MCP (Model Context Protocol) server for fiducia.cloud. Gives coding agents
(Claude Code, Codex, anything MCP-capable) read-only diagnostic tools over the
fiducia stack, plus an embedded map of the whole org so agents can orient
without cloning every repo.

Binary: `fiducia-mcp` — speaks MCP over **stdio** (stdout is the wire; all
logs go to stderr).

## Tools

All tools are **read-only** (HTTP GETs). Mutations — acquiring locks, moving
shards, releasing leases — deliberately stay with the real clients
(`fiducia-client`, `fiducia` CLI) where fencing tokens are handled properly.

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

Two ways to reach the data plane:

1. **Direct / port-forward (internal mode):** set `FIDUCIA_INTERNAL_SECRET` +
   `FIDUCIA_ORG_ID`; the server attaches the same trusted-hop headers the LB
   would inject.
2. **Through the load balancer (bearer mode):** set `FIDUCIA_API_KEY` and
   point `FIDUCIA_NODE_URL` at the LB; the LB authenticates the key and
   injects org scoping itself. Brain calls still need the internal secret.

Missing credentials never crash the server — the affected tool returns an
error naming the env var to set.

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

```sh
cargo test
cargo run   # then paste MCP JSON-RPC on stdin, e.g. an initialize request
```

Built on the official Rust MCP SDK ([rmcp](https://crates.io/crates/rmcp)).
