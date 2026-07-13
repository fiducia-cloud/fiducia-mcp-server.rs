//! Library half of fiducia-mcp-server: upstream HTTP access, the tool
//! surface, and the embedded org map. The `fiducia-mcp` binary wires this
//! to the MCP stdio transport.

pub mod cloudflare;
pub mod domains;
pub mod k8s;
pub mod repo_map;
pub mod server;
pub mod upstream;
