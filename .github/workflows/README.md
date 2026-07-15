# workflows

GitHub Actions for the read-only Fiducia MCP diagnostics server.

- `ci.yml` checks out the MCP server plus its sibling `fiducia-clients` and
  transitive `fiducia-interfaces` path dependencies at reviewed commit SHAs,
  then runs formatting, warnings-as-errors Clippy, locked tests, a complete
  non-root tool-runner image build, and `cargo audit`.
- The workflow has read-only repository permissions, discards checkout
  credentials, cancels superseded runs, and has a bounded runtime.
- This repository never deploys infrastructure. Cluster deployment belongs to
  `fiducia-monorepo`; the MCP server remains a diagnostics surface.

## Security baseline

Every executable workflow uses explicit least-privilege permissions, immutable
third-party action or container references, non-persisted checkout credentials,
concurrency control, and a job timeout. The main CI workflow validates this
directory with the digest-pinned actionlint container. Environment mutation is
forbidden unless this README documents a repository-specific platform exception.
