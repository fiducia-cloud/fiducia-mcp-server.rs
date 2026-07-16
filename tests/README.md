# Integration tests

End-to-end tests for the `fiducia-mcp` binary, rather than individual tool
implementations.

- `stdio_wire.rs` starts the real binary and verifies that stdout contains only
  JSON-RPC frames, startup logging stays on stderr without secrets, and the
  Cloudflare DNS mutation gate rejects writes unless it is explicitly enabled.

Run these with `cargo test --locked --all-targets` from the repository root.
