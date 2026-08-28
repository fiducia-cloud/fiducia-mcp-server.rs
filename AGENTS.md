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
- **Bound every upstream body before parsing or returning it.** Raw diagnostic
  HTTP reads are capped at 4 MiB, including chunked responses; blocking SDK
  error bodies are truncated before becoming model-visible. New HTTP helpers
  must preserve or tighten those limits rather than calling unbounded `text()`
  or `bytes()` methods. Prefer the existing `Response::chunk()` loop so response
  bounding does not require expanding the locked dependency graph.
- **kubectl is read-only.** Build argv as a `Vec<String>` (never a shell
  string), validate every `--context` against `kubectl config get-contexts`,
  and keep the 15s timeout. Add only read-only verbs.

## Where things live

- `src/upstream.rs` — env config, per-plane base URLs + auth headers,
  `get_json` (raw HTTP), and `node_call`: authenticated node data-plane calls
  go through the official `fiducia-client` crate (path dep
  `../fiducia-clients/clients/rust`, blocking ureq → `spawn_blocking`) in both
  trusted-hop and bearer modes. Prefer extending `fiducia-client` instead of
  adding node-plane raw HTTP fallbacks.
- `src/server.rs` — the `#[tool_router]` impl; one tool per question.
  Upstream failures return `CallToolResult::error(...)`, not `Err(...)`, so
  the model sees the message and can react.
- `src/cloudflare.rs` — Cloudflare v4 API (bearer token). Read-only zones/records
  plus the two gated DNS write tools; maps CF's error envelope without leaking
  the token.
- `src/domains.rs` — RDAP registrar lookup + `dns_check`. All DNS lookups go
  through the `Resolve` trait (real: `SystemResolver` over hickory-resolver;
  tests: a mock), so checks run offline.
- `src/k8s.rs` — read-only `kubectl` wrapper (argv builder, context validation,
  15s timeout, JSON summarizers). Tests stub `kubectl` via a temp script on `PATH`.
- `src/repo_map.rs` — embedded org/architecture map served by `repo_map`.
  **Update it when repos are added/renamed/archived** (last sync 2026-08-01).
  Prose companion: `docs/platform-architecture.md`.

## Checks

```sh
cargo fmt --check && cargo clippy -- -D warnings && cargo test
```

The locked CI path must also pass without rewriting `Cargo.lock`.

Smoke-test the wire without an MCP client:

```sh
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | cargo run --quiet
```

## Syncing with the remote

"Sync with the remote" (or just "sync") is a **two-way** exchange — pull the
remote's commits down **and** push yours up. It is never push-only, and a clean
local tree does not by itself mean "synced": you are done only once local and
the remote hold the same commits.

To sync:

1. **Commit your work first** (`git add` + `git commit`) so the tree is clean —
   pull/merge only into a clean tree. `git pull` / `git merge` aborts when an
   incoming change touches a file you have edited, and even when it doesn't it
   buries the merge in your uncommitted work. (Can't commit yet? `git stash`,
   then `git stash pop` after step 3.)
2. `git fetch --all --prune` — safe any time; it only updates tracking refs.
3. `git pull` (fetch + merge) — or `git merge` the upstream branch — to
   integrate the remote's commits.
4. `git push` to publish yours.

Integrate with **`git merge` / `git pull`**. **Never `git rebase` to sync** — it
rewrites history and breaks shared branches.

<!-- BEGIN ores-agents-pointer: managed by ORESoftware/my-ai; edit there, not here -->

## Canonical agent instructions

Before doing anything else in this repository, also read:

    .ores/agents/AGENTS.md

That path is a symlink to `~/codes/oresoftware/my-ai/AGENTS.md`, whose canonical copy is
<https://github.com/ORESoftware/my-ai/blob/main/AGENTS.md>.

It exists at a fixed path *inside* the repository because some agents cannot walk up past
the repository root, so machine-wide instructions one or more directories above are
invisible to them. This pointer plus that path make the same file reachable from a working
directory anywhere in the tree.

The symlink is deliberately **not committed**: it names an absolute path that is only valid
on a machine with `~/codes/oresoftware/my-ai` checked out, so committing it would produce a
broken link for everyone else and for CI. `.ores/` is git-ignored for that reason. If
`.ores/agents/AGENTS.md` is missing on your machine, create it with:

    mkdir -p .ores/agents
    ln -sfn "$HOME/codes/oresoftware/my-ai/AGENTS.md" .ores/agents/AGENTS.md

or run `~/codes/oresoftware/my-ai/scripts/link-repo-agents.sh` once to do it for every git
repository under `~/codes`, and `--check` to verify them.

A missing `.ores/agents/AGENTS.md` is a setup gap on the reader's machine, never a reason to
skip the canonical instructions: fetch them from the URL above instead.

<!-- END ores-agents-pointer -->
