# Agent guidelines — fiducia-mcp-server.rs

MCP server exposing read-only fiducia.cloud diagnostics over stdio. See
README.md for the tool table and env configuration.

## Hard rules

- **stdout is the MCP wire.** Never print or log to stdout in the binary path;
  logging goes to stderr (`tracing_subscriber` with `.with_writer(std::io::stderr)`).
  This is also why the crate does not use `fiducia-telemetry` — its fallback
  logger writes to stdout.
- **Tools stay read-only.** Do not add tools that acquire/release locks or
  leases, write KV, or change placement/scale. Fenced mutations belong to
  `fiducia-client` / the `fiducia` CLI, where fencing tokens are handled
  end-to-end. If a write tool is ever justified, it needs explicit operator
  sign-off first.
- **Auth headers are per-plane and easy to mix up** (see `src/upstream.rs`):
  node = `x-fiducia-internal-auth` + `x-fiducia-org-id`; brain =
  `x-fiducia-internal-auth` only; ai-agent control plane = `x-internal-auth`.
  Keep the unit tests in `upstream.rs` in sync with any change.
- Never log secret values; log only set/unset (see `main.rs`).

## Where things live

- `src/upstream.rs` — env config, per-plane base URLs + auth headers, the one
  `get_json` helper, `urlencode`.
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
