const MANIFEST: &str = include_str!("../Cargo.toml");
const LOCKFILE: &str = include_str!("../Cargo.lock");
const TELEMETRY: &str = include_str!("../src/telemetry.rs");
const OBSERVABILITY: &str = include_str!("../OBSERVABILITY.md");
const SHARED_REVISION: &str = "a5c1ba9c50493ac625dd2fb175af21263d0d2801";

#[test]
fn shared_bootstrap_dependency_and_lockfile_are_immutable() {
    assert!(MANIFEST.contains("ore-mcp-bootstrap"));
    assert!(MANIFEST.contains(&format!("rev = \"{SHARED_REVISION}\"")));
    assert!(!MANIFEST.contains("branch ="));
    assert!(!MANIFEST.contains("tag ="));

    assert!(LOCKFILE.contains("name = \"ore-mcp-bootstrap\""));
    assert!(LOCKFILE.contains(SHARED_REVISION));
}

#[test]
fn canonical_stdio_identity_is_validated_before_resource_creation() {
    assert!(TELEMETRY.contains("ore_mcp_bootstrap::runtime::ServerIdentity::stdio"));
    assert!(TELEMETRY.contains("identity.service_name()"));
    assert!(TELEMETRY.contains("identity.service_namespace()"));
    assert!(OBSERVABILITY.contains("service.name=fiducia-mcp"));
    assert!(OBSERVABILITY.contains("service.namespace=fiducia-cloud"));
}

#[test]
fn production_telemetry_delegates_shared_policy_and_retains_local_caps() {
    assert!(TELEMETRY.contains("ore_mcp_bootstrap::telemetry::MAX_RESOURCE_ATTRIBUTE_BYTES"));
    assert!(TELEMETRY.contains("ore_mcp_bootstrap::telemetry::resource_attribute_pairs"));
    assert!(TELEMETRY.contains("const MAX_RESOURCE_ATTRIBUTES: usize = 32"));
    assert!(TELEMETRY.contains("reserved_resource_attribute_key"));
    assert!(!TELEMETRY.contains("fn valid_attribute_key"));
    assert!(!TELEMETRY.contains("fn sensitive_attribute_key"));

    assert!(OBSERVABILITY.contains("at most 8 KiB raw input"));
    assert!(OBSERVABILITY.contains("at most 32 accepted attributes"));
    assert!(OBSERVABILITY.contains("the first valid duplicate key wins"));
}

#[test]
fn tool_telemetry_contract_stays_low_cardinality_and_stderr_safe() {
    assert!(TELEMETRY.contains("with_writer(std::io::stderr)"));
    assert!(TELEMETRY.contains("mcp.server.tool.calls"));
    assert!(TELEMETRY.contains("mcp.server.tool.duration"));
    assert!(TELEMETRY.contains("mcp.tool.name"));
    assert!(TELEMETRY.contains("mcp.tool.error"));

    for forbidden in [
        "context.arguments",
        "request.body",
        "response.body",
        "tool.result",
    ] {
        assert!(
            !TELEMETRY.contains(forbidden),
            "high-cardinality telemetry field is forbidden: {forbidden}"
        );
    }
}
