const MANIFEST: &str = include_str!("../Cargo.toml");
const TELEMETRY: &str = include_str!("../src/telemetry.rs");

#[test]
fn shared_bootstrap_dependency_is_immutable() {
    assert!(MANIFEST.contains("ore-mcp-bootstrap"));
    assert!(MANIFEST.contains("rev = \"a5c1ba9c50493ac625dd2fb175af21263d0d2801\""));
    assert!(!MANIFEST.contains("ore-mcp-bootstrap = { git") || !MANIFEST.contains("branch ="));
    assert!(!MANIFEST.contains("ore-mcp-bootstrap = { git") || !MANIFEST.contains("tag ="));
}

#[test]
fn production_telemetry_delegates_shared_policy() {
    assert!(TELEMETRY.contains("ore_mcp_bootstrap::runtime::ServerIdentity::stdio"));
    assert!(TELEMETRY.contains("ore_mcp_bootstrap::telemetry::resource_attribute_pairs"));
    assert!(TELEMETRY.contains("reserved_resource_attribute_key"));
    assert!(TELEMETRY.contains("MAX_RESOURCE_ATTRIBUTES"));
    assert!(!TELEMETRY.contains("fn valid_attribute_key"));
    assert!(!TELEMETRY.contains("fn sensitive_attribute_key"));
}
