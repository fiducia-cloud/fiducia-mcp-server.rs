# Rust MCP modularization delivery — DEN-957

**Organization:** `fiducia-cloud`  
**Repository:** [`fiducia-cloud/fiducia-mcp-server.rs`](https://github.com/fiducia-cloud/fiducia-mcp-server.rs)  
**Canonical pull request:** [#16](https://github.com/fiducia-cloud/fiducia-mcp-server.rs/pull/16)  
**Reviewed head:** `e16451f8d74065153da912a128f4d50b9d66c18f`  
**Merge commit:** [`77cdf2a06dfe46d8235100d5a5dce6719c78b76a`](https://github.com/fiducia-cloud/fiducia-mcp-server.rs/commit/77cdf2a06dfe46d8235100d5a5dce6719c78b76a)  
**Shared bootstrap revision:** `a5c1ba9c50493ac625dd2fb175af21263d0d2801`  
**Linear project:** [fiducia-cloud](https://linear.app/denman/project/fiducia-cloud-8fd5e1bec9d3)  
**Linear delivery document:** [DEN-957 — fiducia-cloud](https://linear.app/denman/document/rust-mcp-modularization-delivery-den-957-fiducia-cloud-80f444f9063e)  
**Parent issue:** DEN-957

## Delivered boundary

The server consumes the immutable shared bootstrap for version-neutral service
identity and secret-safe telemetry resource policy. Product-specific tools,
schemas, authorization, SDK/exporter versions, external clients, timeouts,
response limits, and stdio lifecycle behavior remain owned by this repository.

## Project routing

- Canonical GitHub Project title: `fiducia-cloud-project`
- GitHub Project route: `https://github.com/orgs/fiducia-cloud/projects/1`
- Planning authority: the Linear project and linked Linear issue
- Implementation authority: the canonical GitHub issue or pull request
- Completion evidence: reviewed head plus merge commit

## Validation policy

A workflow is successful only when a runner checks out the source and executes
the required commands. A job rejected before checkout is an infrastructure
admission failure and must not be represented as passing source validation.
Superseded pull requests are closed and excluded from fleet counts.

## Follow-up ownership

Future shared-policy changes begin in `ORESoftware/mcp-rust-libs`, receive an
immutable revision, and then roll through one reviewable consumer PR per
repository. Product behavior changes remain separate from shared-policy
migrations.
