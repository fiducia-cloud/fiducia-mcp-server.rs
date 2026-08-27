//! Library half of fiducia-mcp-server: upstream HTTP access, the tool
//! surface, embedded org map, telemetry, and process lifecycle. The
//! `fiducia-mcp` binary is intentionally only a Tokio bootstrap.

pub mod cloudflare;
pub mod domains;
pub mod env_map;
pub mod flags;
pub mod k8s;
pub mod repo_map;
pub mod runtime;
pub mod server;
pub mod shared_bootstrap;
pub mod telemetry;
pub mod upstream;

pub use env_map::{env_value, get_env_map, EnvMap};
