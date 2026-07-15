# src

MCP server internals: `server.rs` (stdio JSON-RPC loop — stdout is the wire,
so logs go to stderr) and `repo_map.rs` (the fleet knowledge map the tools
serve). Keep stdout writes confined to the protocol layer.
