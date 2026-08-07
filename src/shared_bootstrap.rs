//! Shared, version-neutral startup policy pinned to `mcp-rust-libs`.

use ore_mcp_bootstrap::runtime::{IdentityError, ServerIdentity};

pub const SERVICE_NAME: &str = "fiducia-mcp";
pub const SERVICE_NAMESPACE: &str = "fiducia-cloud";

pub fn stdio_identity() -> Result<ServerIdentity, IdentityError> {
    ServerIdentity::stdio(SERVICE_NAME, SERVICE_NAMESPACE)
}

#[must_use]
pub fn environment_resource_attributes() -> Vec<(String, String)> {
    ore_mcp_bootstrap::telemetry::environment_resource_attributes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_preserves_the_existing_fiducia_contract() {
        let identity = stdio_identity().expect("valid canonical identity");
        assert_eq!(identity.service_name(), SERVICE_NAME);
        assert_eq!(identity.service_namespace(), SERVICE_NAMESPACE);
        assert_eq!(identity.transport(), "stdio");
    }

    #[test]
    fn shared_attributes_reject_credentials_and_identity_overrides() {
        assert_eq!(
            ore_mcp_bootstrap::telemetry::resource_attribute_pairs(
                "plane=coordination,api_key=secret,service.name=spoof,cloud.region=us-east-1",
            ),
            vec![
                ("plane".to_owned(), "coordination".to_owned()),
                ("cloud.region".to_owned(), "us-east-1".to_owned()),
            ]
        );
    }
}
