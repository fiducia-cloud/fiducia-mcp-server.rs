# MCP server source

Read-only diagnostic tools and their upstream adapters. The binary's stdout is
reserved exclusively for MCP JSON-RPC, so diagnostics must go to stderr and
must never expose credentials. Mutating tools remain prohibited except the two
explicitly gated Cloudflare DNS operations documented in `AGENTS.md`.
