const MAIN_RS: &str = include_str!("../src/main.rs");
const LIB_RS: &str = include_str!("../src/lib.rs");
const RUNTIME_RS: &str = include_str!("../src/runtime.rs");
const TELEMETRY_RS: &str = include_str!("../src/telemetry.rs");
const OBSERVABILITY: &str = include_str!("../OBSERVABILITY.md");

#[test]
fn main_remains_a_thin_bootstrap_and_runtime_owns_stdio() {
    assert!(
        MAIN_RS.lines().count() <= 8,
        "main.rs must remain a thin Tokio bootstrap"
    );
    assert!(MAIN_RS.contains("fiducia_mcp_server::runtime::run_stdio"));

    for lifecycle_detail in [
        "process_log_filter",
        "telemetry::init",
        "Config::from_env",
        "FiduciaMcp",
        "Upstream",
        "serve(stdio())",
        "mcp.server",
    ] {
        assert!(
            !MAIN_RS.contains(lifecycle_detail),
            "process lifecycle must remain outside main.rs: {lifecycle_detail}"
        );
        assert!(
            RUNTIME_RS.contains(lifecycle_detail),
            "runtime.rs must own lifecycle detail: {lifecycle_detail}"
        );
    }
}

#[test]
fn runtime_is_exported_and_does_not_absorb_tool_implementations() {
    assert!(LIB_RS.contains("pub mod runtime;"));
    assert!(!RUNTIME_RS.contains("#[tool"));
    assert!(!RUNTIME_RS.contains("serde_json::json"));
    assert!(!RUNTIME_RS.contains("FIDUCIA_API_KEY="));
    assert!(!RUNTIME_RS.contains("FIDUCIA_INTERNAL_SECRET="));
}

#[test]
fn telemetry_is_stderr_only_bounded_and_low_cardinality() {
    for required in [
        "MAX_OTLP_ENDPOINT_BYTES",
        "MAX_RESOURCE_ATTRIBUTES_RAW_BYTES",
        "MAX_RESOURCE_ATTRIBUTES",
        "sanitize_otlp_endpoint",
        "reserved_resource_attribute_key",
        "sensitive_attribute_key",
        "with_writer(std::io::stderr)",
        "mcp.server.tool.calls",
        "mcp.server.tool.duration",
        "mcp.tool.name",
    ] {
        assert!(
            TELEMETRY_RS.contains(required),
            "missing telemetry contract marker: {required}"
        );
    }

    for source in [MAIN_RS, RUNTIME_RS, TELEMETRY_RS] {
        for line in source.lines().map(str::trim_start) {
            assert!(!line.starts_with("print!("), "stdout print is forbidden");
            assert!(
                !line.starts_with("println!("),
                "stdout println is forbidden"
            );
        }
    }

    assert!(!TELEMETRY_RS.contains("context.arguments"));
    assert!(!TELEMETRY_RS.contains("response.body"));
}

#[test]
fn observability_documentation_matches_the_implementation() {
    for required in [
        "stdout for JSON-RPC frames",
        "src/runtime.rs",
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        "at most 16 KiB raw input",
        "at most 32 accepted attributes",
        "mcp.server.tool.calls",
        "mcp.server.tool.duration",
        "Loki",
        "Prometheus",
        "Grafana",
    ] {
        assert!(
            OBSERVABILITY.contains(required),
            "missing observability documentation: {required}"
        );
    }
}
