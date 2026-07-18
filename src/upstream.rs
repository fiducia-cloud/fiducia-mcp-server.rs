//! Upstream HTTP access: configuration (env) + one JSON GET helper per plane.
//!
//! Three planes, three auth conventions (see each service's internal_auth):
//! - node  (data plane):    `x-fiducia-internal-auth` + `x-fiducia-org-id`
//! - brain (control plane): `x-fiducia-internal-auth` only (not tenant-scoped)
//! - agent control plane:   `x-internal-auth`
//!
//! Alternatively, when `FIDUCIA_API_KEY` is set, node-plane calls send
//! `Authorization: Bearer <key>` instead — for going through the load
//! balancer, which verifies the key and injects the trusted-hop headers
//! itself (and strips any client-supplied ones).

use fiducia_client::FiduciaClient;
use std::sync::Arc;
use std::time::Duration;

pub const NODE_URL_ENV: &str = "FIDUCIA_NODE_URL";
pub const BRAIN_URL_ENV: &str = "FIDUCIA_BRAIN_URL";
pub const AGENT_CP_URL_ENV: &str = "FIDUCIA_AGENT_CONTROL_PLANE_URL";
pub const INTERNAL_SECRET_ENV: &str = "FIDUCIA_INTERNAL_SECRET";
pub const ORG_ID_ENV: &str = "FIDUCIA_ORG_ID";
pub const CONTROL_PLANE_SECRET_ENV: &str = "FIDUCIA_CONTROL_PLANE_SECRET";
pub const API_KEY_ENV: &str = "FIDUCIA_API_KEY";

const DEFAULT_NODE_URL: &str = "http://localhost:8090";
const DEFAULT_BRAIN_URL: &str = "http://localhost:8095";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    Node,
    Brain,
    AgentControlPlane,
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub node_url: String,
    pub brain_url: String,
    pub agent_cp_url: Option<String>,
    pub internal_secret: Option<String>,
    pub org_id: Option<String>,
    pub control_plane_secret: Option<String>,
    pub api_key: Option<String>,
}

pub(crate) fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    pub fn from_env() -> Self {
        Self {
            node_url: env_nonempty(NODE_URL_ENV).unwrap_or_else(|| DEFAULT_NODE_URL.into()),
            brain_url: env_nonempty(BRAIN_URL_ENV).unwrap_or_else(|| DEFAULT_BRAIN_URL.into()),
            agent_cp_url: env_nonempty(AGENT_CP_URL_ENV),
            internal_secret: env_nonempty(INTERNAL_SECRET_ENV),
            org_id: env_nonempty(ORG_ID_ENV),
            // The agent control plane checks FIDUCIA_CONTROL_PLANE_SECRET and
            // falls back to FIDUCIA_INTERNAL_SECRET; mirror that here.
            control_plane_secret: env_nonempty(CONTROL_PLANE_SECRET_ENV)
                .or_else(|| env_nonempty(INTERNAL_SECRET_ENV)),
            api_key: env_nonempty(API_KEY_ENV),
        }
    }

    pub fn base_url(&self, plane: Plane) -> Result<&str, String> {
        let url = match plane {
            Plane::Node => self.node_url.as_str(),
            Plane::Brain => self.brain_url.as_str(),
            Plane::AgentControlPlane => self.agent_cp_url.as_deref().ok_or_else(|| {
                format!(
                    "{AGENT_CP_URL_ENV} is not set; point it at the \
                         fiducia-ai-agent-control-plane base URL to use file-lease tools"
                )
            })?,
        };
        validate_base_url(url)?;
        Ok(url.trim_end_matches('/'))
    }

    /// Headers to attach for a given plane, as (name, value) pairs.
    pub fn headers(&self, plane: Plane) -> Result<Vec<(&'static str, String)>, String> {
        let mut out = Vec::new();
        match plane {
            Plane::Node => {
                // Bearer mode (via the LB) wins when an API key is configured;
                // the LB strips internal headers from clients anyway.
                if let Some(key) = &self.api_key {
                    out.push(("authorization", format!("Bearer {key}")));
                    return Ok(out);
                }
                let secret = self.internal_secret.as_ref().ok_or_else(|| {
                    format!(
                        "no credentials for the node data plane: set {API_KEY_ENV} \
                         (via load balancer) or {INTERNAL_SECRET_ENV} + {ORG_ID_ENV} \
                         (direct to a node)"
                    )
                })?;
                let org = self.org_id.as_ref().ok_or_else(|| {
                    format!("{ORG_ID_ENV} is required for direct node calls (x-fiducia-org-id)")
                })?;
                out.push(("x-fiducia-internal-auth", secret.clone()));
                out.push(("x-fiducia-org-id", org.clone()));
            }
            Plane::Brain => {
                let secret = self.internal_secret.as_ref().ok_or_else(|| {
                    format!("{INTERNAL_SECRET_ENV} is required for brain (control plane) calls")
                })?;
                out.push(("x-fiducia-internal-auth", secret.clone()));
            }
            Plane::AgentControlPlane => {
                let secret = self.control_plane_secret.as_ref().ok_or_else(|| {
                    format!(
                        "{CONTROL_PLANE_SECRET_ENV} (or {INTERNAL_SECRET_ENV}) is required \
                         for ai-agent control plane calls"
                    )
                })?;
                out.push(("x-internal-auth", secret.clone()));
            }
        }
        Ok(out)
    }
}

pub struct Upstream {
    client: reqwest::Client,
    /// Official Rust client for the node data plane, present in internal mode
    /// (secret + org id, no API key). It is blocking (`ureq`), so every call
    /// runs on `spawn_blocking`. Bearer mode uses the hardened reqwest client
    /// because the pinned canonical client has no bearer constructor.
    node_client: Option<Arc<FiduciaClient>>,
    pub config: Config,
}

impl Upstream {
    pub fn new(config: Config) -> Self {
        let client = reqwest::Client::builder()
            // No diagnostic request needs to follow a redirect. In particular,
            // brain/control-plane trusted-hop headers must never be replayed to
            // a Location chosen by an upstream peer.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        let node_client = match (&config.api_key, &config.internal_secret, &config.org_id) {
            (None, Some(secret), Some(org)) if validate_base_url(&config.node_url).is_ok() => {
                let mut c = FiduciaClient::internal(&config.node_url, secret, org);
                c.request_timeout = Some(Duration::from_secs(15));
                Some(Arc::new(c))
            }
            _ => None,
        };
        Self {
            client,
            node_client,
            config,
        }
    }

    /// Call the node data plane. Internal mode goes through fiducia-client on
    /// the blocking pool; otherwise bearer mode (or an unconfigured server,
    /// which yields guidance from `headers`) uses the hardened raw GET path.
    pub async fn node_call<F>(
        &self,
        call: F,
        fallback_path: &str,
    ) -> Result<serde_json::Value, String>
    where
        F: FnOnce(&FiduciaClient) -> Result<serde_json::Value, fiducia_client::Error>
            + Send
            + 'static,
    {
        validate_base_url(&self.config.node_url)?;
        match &self.node_client {
            Some(node_client) => {
                let node_client = Arc::clone(node_client);
                tokio::task::spawn_blocking(move || call(&node_client))
                    .await
                    .map_err(|e| format!("node client task failed: {e}"))?
                    .map_err(format_client_error)
            }
            None => self.get_json(Plane::Node, fallback_path).await,
        }
    }

    /// GET `<plane base>/<path_and_query>` with the plane's auth headers and
    /// return the response body as JSON. Non-2xx responses are reported as
    /// errors but still carry the upstream body — services here return
    /// structured JSON errors worth showing to the model.
    pub async fn get_json(
        &self,
        plane: Plane,
        path_and_query: &str,
    ) -> Result<serde_json::Value, String> {
        let base = self.config.base_url(plane)?;
        let url = format!("{base}{path_and_query}");
        let mut req = self.client.get(&url);
        for (name, value) in self.config.headers(plane)? {
            req = req.header(name, value);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("request to {url} failed: {e}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .map_err(|e| format!("reading response from {url} failed: {e}"))?;
        let json: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|_| serde_json::Value::String(body.clone()));
        if status.is_success() {
            Ok(json)
        } else {
            Err(format!("{url} returned {status}: {json}"))
        }
    }
}

fn validate_base_url(raw: &str) -> Result<(), String> {
    let raw = raw.trim();
    if raw.len() > 2_048 || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("upstream base URL contains invalid characters".to_string());
    }
    let parsed =
        reqwest::Url::parse(raw).map_err(|_| "upstream base URL is invalid".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host().is_none() {
        return Err("upstream base URL must use http(s) and include a host".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("upstream base URL must not contain credentials".to_string());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("upstream base URL must not contain a query or fragment".to_string());
    }
    let host = parsed.host_str().unwrap_or_default();
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let blocked = match ip {
            std::net::IpAddr::V4(ip) => ip.is_link_local() || ip.is_unspecified(),
            std::net::IpAddr::V6(ip) => ip.is_unicast_link_local() || ip.is_unspecified(),
        };
        if blocked {
            return Err("cloud metadata endpoints are not allowed".to_string());
        }
    }
    if matches!(
        host.trim_end_matches('.').to_ascii_lowercase().as_str(),
        "metadata.google.internal" | "metadata.azure.internal"
    ) {
        return Err("cloud metadata endpoints are not allowed".to_string());
    }
    Ok(())
}

/// Render a fiducia-client error the same way `get_json` renders raw HTTP
/// failures: keep the upstream JSON body — it's structured and worth showing.
fn format_client_error(err: fiducia_client::Error) -> String {
    match err {
        fiducia_client::Error::Http { status, body } => {
            let body = body
                .map(|b| b.to_string())
                .unwrap_or_else(|| "(empty body)".to_string());
            format!("node returned {status}: {body}")
        }
        fiducia_client::Error::Transport(message) => format!("node request failed: {message}"),
    }
}

/// Percent-encode a value for use inside a query string.
pub fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            node_url: "http://node:8090/".into(),
            brain_url: "http://brain:8095".into(),
            agent_cp_url: None,
            internal_secret: Some("s3cret".into()),
            org_id: Some("org-1".into()),
            control_plane_secret: Some("cp-s3cret".into()),
            api_key: None,
        }
    }

    #[test]
    fn base_url_trims_trailing_slash() {
        assert_eq!(cfg().base_url(Plane::Node).unwrap(), "http://node:8090");
        assert_eq!(cfg().base_url(Plane::Brain).unwrap(), "http://brain:8095");
    }

    #[test]
    fn agent_cp_requires_url() {
        let err = cfg().base_url(Plane::AgentControlPlane).unwrap_err();
        assert!(err.contains(AGENT_CP_URL_ENV));
    }

    #[test]
    fn node_headers_internal_mode() {
        let headers = cfg().headers(Plane::Node).unwrap();
        assert_eq!(
            headers,
            vec![
                ("x-fiducia-internal-auth", "s3cret".to_string()),
                ("x-fiducia-org-id", "org-1".to_string()),
            ]
        );
    }

    #[test]
    fn node_headers_bearer_mode_wins() {
        let mut c = cfg();
        c.api_key = Some("fk_live_abc".into());
        let headers = c.headers(Plane::Node).unwrap();
        assert_eq!(
            headers,
            vec![("authorization", "Bearer fk_live_abc".to_string())]
        );
    }

    #[test]
    fn node_headers_require_org_in_internal_mode() {
        let mut c = cfg();
        c.org_id = None;
        let err = c.headers(Plane::Node).unwrap_err();
        assert!(err.contains(ORG_ID_ENV));
    }

    #[test]
    fn brain_headers_no_org_scope() {
        let headers = cfg().headers(Plane::Brain).unwrap();
        assert_eq!(
            headers,
            vec![("x-fiducia-internal-auth", "s3cret".to_string())]
        );
    }

    #[test]
    fn agent_cp_uses_x_internal_auth() {
        let headers = cfg().headers(Plane::AgentControlPlane).unwrap();
        assert_eq!(headers, vec![("x-internal-auth", "cp-s3cret".to_string())]);
    }

    #[test]
    fn node_client_is_built_for_internal_mode_only() {
        assert!(Upstream::new(cfg()).node_client.is_some());

        let mut bearer = cfg();
        bearer.api_key = Some("fk_live_abc".into());
        assert!(
            Upstream::new(bearer).node_client.is_none(),
            "bearer mode must use the redirect-safe reqwest client"
        );

        let mut bare = cfg();
        bare.internal_secret = None;
        bare.control_plane_secret = None;
        assert!(Upstream::new(bare).node_client.is_none());
    }

    #[test]
    fn urlencode_reserved_chars() {
        assert_eq!(urlencode("orders/checkout"), "orders%2Fcheckout");
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(urlencode("plain-key_1.2~x"), "plain-key_1.2~x");
    }
}
