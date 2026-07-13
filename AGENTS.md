# Agent guidelines — fiducia-mcp-server.rs

MCP server exposing read-only fiducia.cloud diagnostics over stdio. See
README.md for the tool table and env configuration.

## Hard rules

- **stdout is the MCP wire.** Never print or log to stdout in the binary path;
  logging goes to stderr (`tracing_subscriber` with `.with_writer(std::io::stderr)`).
  This is also why the crate does not use `fiducia-telemetry` — its fallback
  logger writes to stdout.
- **Tools stay read-only** — with exactly two sanctioned exceptions:
  `cloudflare_dns_upsert` and `cloudflare_dns_delete`, both gated behind
  `FIDUCIA_MCP_ALLOW_MUTATIONS=1` (they return a gate error otherwise and never
  call the API). Do NOT add tools that acquire/release locks or leases, write
  KV, change placement/scale, or apply/scale/delete Kubernetes objects. Cluster
  mutations belong to `fiducia-client` / the `fiducia` CLI, where fencing tokens
  are handled end-to-end. Any further write tool needs explicit operator
  sign-off and must reuse the same mutation gate.
- **Auth headers are per-plane and easy to mix up** (see `src/upstream.rs`):
  node = `x-fiducia-internal-auth` + `x-fiducia-org-id`; brain =
  `x-fiducia-internal-auth` only; ai-agent control plane = `x-internal-auth`.
  Keep the unit tests in `upstream.rs` in sync with any change.
- Never log secret values; log only set/unset (see `main.rs`). This includes
  `CLOUDFLARE_API_TOKEN` — it is only ever attached as a bearer header and must
  never appear in a log line, error string, or tool result.
- **kubectl is read-only.** Build argv as a `Vec<String>` (never a shell
  string), validate every `--context` against `kubectl config get-contexts`,
  and keep the 15s timeout. Add only read-only verbs.

## Where things live

- `src/upstream.rs` — env config, per-plane base URLs + auth headers,
  `get_json` (raw HTTP), and `node_call`: node data-plane calls go through
  the official `fiducia-client` crate (path dep `../fiducia-clients/clients/rust`,
  blocking ureq → `spawn_blocking`) in internal mode, falling back to raw
  HTTP for bearer mode and `/v1/observe/*` (no client coverage). Prefer
  extending via `fiducia-client` methods when they exist.
- `src/server.rs` — the `#[tool_router]` impl; one tool per question.
  Upstream failures return `CallToolResult::error(...)`, not `Err(...)`, so
  the model sees the message and can react.
- `src/repo_map.rs` — embedded org/architecture map served by `repo_map`.
  **Update it when repos are added/renamed/archived** (last sync 2026-07).

## Checks

```sh
cargo fmt --check && cargo clippy -- -D warnings && cargo test
```

Smoke-test the wire without an MCP client:

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | cargo run --quiet
```
