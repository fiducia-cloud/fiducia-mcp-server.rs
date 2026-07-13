//! Checking domains from the outside.
//!
//! The fiducia.cloud domains are registered at Squarespace, which exposes no
//! public DNS write API. So instead of *managing* DNS here we *verify* it:
//!
//! - `registrar_status` reads registrar/nameserver/expiry facts over RDAP.
//! - `dns_check` resolves live records and compares them to expectations
//!   (with a built-in `preset:"fiducia"` for the GitHub Pages + Hetzner edge +
//!   Cloudflare-nameserver cutover).
//!
//! Actual DNS writes happen through Cloudflare (see `cloudflare.rs`) once the
//! registrable domain's nameservers point at Cloudflare.
//!
//! All lookups go through the [`Resolve`] trait so tests inject a mock resolver
//! and run fully offline; the real implementation is [`SystemResolver`].

use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;

pub const RDAP_BASE: &str = "https://rdap.org";

/// GitHub Pages apex A records for a `*.github.io` site.
const GITHUB_PAGES_IPS: [&str; 4] = [
    "185.199.108.153",
    "185.199.109.153",
    "185.199.110.153",
    "185.199.111.153",
];
const FIDUCIA_PAGES_CNAME: &str = "fiducia-cloud.github.io";
const FIDUCIA_EDGE_IP: &str = "95.217.171.250";
const CLOUDFLARE_NS_SUFFIX: &str = ".ns.cloudflare.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DnsType {
    A,
    Aaaa,
    Cname,
    Ns,
    Txt,
    Mx,
}

impl DnsType {
    pub fn parse(s: &str) -> Result<Self, String> {
        Ok(match s.trim().to_ascii_uppercase().as_str() {
            "A" => Self::A,
            "AAAA" => Self::Aaaa,
            "CNAME" => Self::Cname,
            "NS" => Self::Ns,
            "TXT" => Self::Txt,
            "MX" => Self::Mx,
            other => return Err(format!("unsupported DNS record type {other:?}")),
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::Aaaa => "AAAA",
            Self::Cname => "CNAME",
            Self::Ns => "NS",
            Self::Txt => "TXT",
            Self::Mx => "MX",
        }
    }
}

/// A boxed, `Send` future — keeps the trait object-safe so a `&dyn Resolve`
/// (real or mock) can be passed straight into the tool logic.
pub type ResolveFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send + 'a>>;

/// Minimal DNS resolution surface. `Ok(vec![])` means "no such record" (a
/// PENDING result); `Err` is reserved for resolver/transport failures.
pub trait Resolve: Send + Sync {
    fn lookup<'a>(&'a self, name: &'a str, record: DnsType) -> ResolveFuture<'a>;
}

// ------------------------------------------------------------------ real resolver

use hickory_resolver::config::{NameServerConfigGroup, ResolverConfig};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::proto::rr::RecordType;
use hickory_resolver::TokioResolver;

/// System-config resolver with a fallback to Cloudflare (1.1.1.1) and Google
/// (8.8.8.8): the system servers are tried first, and any resolver/transport
/// error (but not an authoritative "no records") falls through to the public
/// fallback so checks still work where `/etc/resolv.conf` is empty.
pub struct SystemResolver {
    primary: Option<TokioResolver>,
    fallback: TokioResolver,
}

impl Default for SystemResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemResolver {
    pub fn new() -> Self {
        let primary = TokioResolver::builder_tokio().ok().map(|b| b.build());
        let mut group = NameServerConfigGroup::cloudflare();
        group.merge(NameServerConfigGroup::google());
        let config = ResolverConfig::from_parts(None, vec![], group);
        let fallback =
            TokioResolver::builder_with_config(config, TokioConnectionProvider::default()).build();
        Self { primary, fallback }
    }
}

fn record_type(t: DnsType) -> RecordType {
    match t {
        DnsType::A => RecordType::A,
        DnsType::Aaaa => RecordType::AAAA,
        DnsType::Cname => RecordType::CNAME,
        DnsType::Ns => RecordType::NS,
        DnsType::Txt => RecordType::TXT,
        DnsType::Mx => RecordType::MX,
    }
}

/// Append a trailing dot so the resolver never applies local search domains.
fn fqdn(name: &str) -> String {
    let name = name.trim();
    if name.ends_with('.') {
        name.to_string()
    } else {
        format!("{name}.")
    }
}

enum QueryOutcome {
    Values(Vec<String>),
    /// Authoritative "no such record" — treated as an empty, non-error result.
    Empty,
    /// Resolver/transport failure — worth trying the fallback for.
    Soft(String),
}

async fn query(resolver: &TokioResolver, name: &str, rt: RecordType) -> QueryOutcome {
    match resolver.lookup(name.to_string(), rt).await {
        Ok(lookup) => {
            let values = lookup
                .record_iter()
                .filter(|rec| rec.record_type() == rt)
                .map(|rec| rec.data().to_string().trim_end_matches('.').to_string())
                .collect();
            QueryOutcome::Values(values)
        }
        Err(e) if e.is_nx_domain() || e.is_no_records_found() => QueryOutcome::Empty,
        Err(e) => QueryOutcome::Soft(format!("DNS lookup for {name} {rt} failed: {e}")),
    }
}

impl Resolve for SystemResolver {
    fn lookup<'a>(&'a self, name: &'a str, record: DnsType) -> ResolveFuture<'a> {
        Box::pin(async move {
            let rt = record_type(record);
            let name = fqdn(name);
            if let Some(primary) = &self.primary {
                match query(primary, &name, rt).await {
                    QueryOutcome::Values(v) => return Ok(v),
                    QueryOutcome::Empty => return Ok(Vec::new()),
                    QueryOutcome::Soft(_) => { /* fall through to the public fallback */ }
                }
            }
            match query(&self.fallback, &name, rt).await {
                QueryOutcome::Values(v) => Ok(v),
                QueryOutcome::Empty => Ok(Vec::new()),
                QueryOutcome::Soft(msg) => Err(msg),
            }
        })
    }
}

// ------------------------------------------------------------------ RDAP

/// `domain_registrar_status`: read registrar, nameservers, status, and expiry
/// over RDAP. Follows exactly ONE redirect from the bootstrap server
/// (`rdap.org`) to the authoritative registry. `client` MUST be built with
/// redirects disabled so the hop is observable.
pub async fn registrar_status(
    client: &reqwest::Client,
    rdap_base: &str,
    domain: &str,
) -> Result<Value, String> {
    let domain = domain.trim().trim_end_matches('.');
    if domain.is_empty() {
        return Err("`domain` is required".to_string());
    }
    let url = format!("{}/domain/{}", rdap_base.trim_end_matches('/'), domain);
    let resp = client
        .get(&url)
        .header("accept", "application/rdap+json")
        .send()
        .await
        .map_err(|e| format!("RDAP request to {url} failed: {e}"))?;

    let resp = if resp.status().is_redirection() {
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                format!("RDAP {url} returned {} without a Location header", resp.status())
            })?;
        let next = resolve_location(rdap_base, location);
        client
            .get(&next)
            .header("accept", "application/rdap+json")
            .send()
            .await
            .map_err(|e| format!("RDAP redirect to {next} failed: {e}"))?
    } else {
        resp
    };

    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("reading RDAP response for {domain} failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("RDAP lookup for {domain} returned {status}"));
    }
    let json: Value =
        serde_json::from_str(&text).map_err(|e| format!("RDAP response was not JSON: {e}"))?;
    Ok(parse_rdap(domain, &json))
}

/// Resolve an RDAP `Location` (absolute, or relative to the bootstrap base).
fn resolve_location(base: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        location.to_string()
    } else {
        let base = base.trim_end_matches('/');
        if let Some(rest) = location.strip_prefix('/') {
            format!("{base}/{rest}")
        } else {
            format!("{base}/{location}")
        }
    }
}

fn parse_rdap(domain: &str, json: &Value) -> Value {
    let nameservers: Vec<String> = json
        .get("nameservers")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|ns| ns.get("ldhName").and_then(Value::as_str))
                .map(|s| s.trim_end_matches('.').to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    let status: Vec<String> = json
        .get("status")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let expiry = json
        .get("events")
        .and_then(Value::as_array)
        .and_then(|events| {
            events
                .iter()
                .find(|e| e.get("eventAction").and_then(Value::as_str) == Some("expiration"))
                .and_then(|e| e.get("eventDate").and_then(Value::as_str))
                .map(str::to_string)
        });
    json!({
        "domain": domain,
        "registrar": rdap_registrar(json),
        "nameservers": nameservers,
        "status": status,
        "expiry": expiry,
    })
}

/// Extract the registrar name: the `registrar`-role entity's vCard `fn`,
/// falling back to its handle.
fn rdap_registrar(json: &Value) -> Value {
    let Some(entities) = json.get("entities").and_then(Value::as_array) else {
        return Value::Null;
    };
    for entity in entities {
        let is_registrar = entity
            .get("roles")
            .and_then(Value::as_array)
            .map(|roles| roles.iter().any(|r| r.as_str() == Some("registrar")))
            .unwrap_or(false);
        if !is_registrar {
            continue;
        }
        if let Some(name) = vcard_fn(entity) {
            return json!(name);
        }
        if let Some(handle) = entity.get("handle").and_then(Value::as_str) {
            return json!(handle);
        }
    }
    Value::Null
}

/// Pull the `fn` (formatted name) property out of an entity's `vcardArray`.
fn vcard_fn(entity: &Value) -> Option<String> {
    let props = entity.get("vcardArray")?.as_array()?.get(1)?.as_array()?;
    for prop in props {
        let Some(prop) = prop.as_array() else { continue };
        if prop.first().and_then(Value::as_str) == Some("fn") {
            return prop.get(3).and_then(Value::as_str).map(str::to_string);
        }
    }
    None
}

// ------------------------------------------------------------------ dns_check

/// Inputs for `dns_check`, mirroring the tool schema.
pub struct DnsCheckInput {
    pub name: Option<String>,
    pub record_type: Option<String>,
    pub values: Option<Vec<String>>,
    pub preset: Option<String>,
}

pub async fn dns_check(resolver: &dyn Resolve, input: DnsCheckInput) -> Result<Value, String> {
    if let Some(preset) = input.preset.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        return match preset {
            "fiducia" => Ok(preset_fiducia(resolver).await),
            other => Err(format!(
                "unknown preset {other:?}; the only built-in preset is \"fiducia\""
            )),
        };
    }

    let Some(name) = input.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) else {
        return Err(
            "provide `name` (optionally with `type` + `values`), or preset:\"fiducia\"".to_string(),
        );
    };
    // Default to an A-record presence check when no type is given.
    let rtype = match input.record_type.as_deref() {
        Some(t) => DnsType::parse(t)?,
        None => DnsType::A,
    };
    let check = check_values(resolver, name, rtype, input.values.as_deref()).await;
    Ok(json!({ "name": name, "type": rtype.as_str(), "checks": [check] }))
}

/// Classify observed vs expected values. PASS when at least one expected value
/// is observed (DNS commonly returns several records); PENDING when nothing
/// resolves yet; MISMATCH when records exist but none match.
fn classify(found: &[String], expected: Option<&[String]>) -> &'static str {
    match expected {
        None => {
            if found.is_empty() {
                "PENDING"
            } else {
                "PASS"
            }
        }
        Some(expected) => {
            if found.is_empty() {
                "PENDING"
            } else if expected
                .iter()
                .any(|e| found.iter().any(|f| f.eq_ignore_ascii_case(e)))
            {
                "PASS"
            } else {
                "MISMATCH"
            }
        }
    }
}

async fn check_values(
    resolver: &dyn Resolve,
    name: &str,
    rtype: DnsType,
    expected: Option<&[String]>,
) -> Value {
    match resolver.lookup(name, rtype).await {
        Err(e) => json!({
            "name": name, "type": rtype.as_str(), "status": "ERROR", "error": e
        }),
        Ok(found) => json!({
            "name": name,
            "type": rtype.as_str(),
            "expected": expected,
            "found": found,
            "status": classify(&found, expected),
        }),
    }
}

/// The `fiducia` preset: verify the whole cutover in one shot.
async fn preset_fiducia(resolver: &dyn Resolve) -> Value {
    let mut checks = Vec::new();
    checks.push(github_pages_check(resolver, "fiducia.cloud").await);
    checks.push(github_pages_check(resolver, "www.fiducia.cloud").await);
    checks.push(edge_check(resolver, "app.fiducia.cloud").await);
    checks.push(edge_check(resolver, "admin.fiducia.cloud").await);
    checks.push(cloudflare_ns_check(resolver, "fiducia.cloud").await);

    let mut pass = 0;
    let mut pending = 0;
    let mut mismatch = 0;
    for c in &checks {
        match c.get("status").and_then(Value::as_str) {
            Some("PASS") => pass += 1,
            Some("PENDING") => pending += 1,
            _ => mismatch += 1,
        }
    }
    json!({
        "preset": "fiducia",
        "checks": checks,
        "summary": { "pass": pass, "pending": pending, "mismatch": mismatch },
    })
}

async fn github_pages_check(resolver: &dyn Resolve, name: &str) -> Value {
    let a = resolver.lookup(name, DnsType::A).await.unwrap_or_default();
    let cname = resolver.lookup(name, DnsType::Cname).await.unwrap_or_default();
    let a_ok = a.iter().any(|ip| GITHUB_PAGES_IPS.contains(&ip.as_str()));
    let cname_ok = cname
        .iter()
        .any(|c| c.eq_ignore_ascii_case(FIDUCIA_PAGES_CNAME));
    let status = if a_ok || cname_ok {
        "PASS"
    } else if a.is_empty() && cname.is_empty() {
        "PENDING"
    } else {
        "MISMATCH"
    };
    json!({
        "name": name,
        "expect": "GitHub Pages (A 185.199.108-111.153 or CNAME fiducia-cloud.github.io)",
        "found": { "A": a, "CNAME": cname },
        "status": status,
    })
}

async fn edge_check(resolver: &dyn Resolve, name: &str) -> Value {
    let a = resolver.lookup(name, DnsType::A).await;
    match a {
        Err(e) => json!({ "name": name, "expect": FIDUCIA_EDGE_IP, "status": "ERROR", "error": e }),
        Ok(found) => json!({
            "name": name,
            "expect": format!("A {FIDUCIA_EDGE_IP} (Hetzner edge)"),
            "found": found,
            "status": classify(&found, Some(&[FIDUCIA_EDGE_IP.to_string()])),
        }),
    }
}

async fn cloudflare_ns_check(resolver: &dyn Resolve, name: &str) -> Value {
    let ns = resolver.lookup(name, DnsType::Ns).await.unwrap_or_default();
    let status = if ns.is_empty() {
        "PENDING"
    } else if ns
        .iter()
        .all(|n| n.to_ascii_lowercase().ends_with(CLOUDFLARE_NS_SUFFIX))
    {
        "PASS"
    } else {
        "MISMATCH"
    };
    json!({
        "name": name,
        "expect": "nameservers are *.ns.cloudflare.com",
        "found": ns,
        "status": status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::Path,
        http::{HeaderMap, StatusCode},
        routing::get,
        Json, Router,
    };
    use std::collections::HashMap;

    /// Mock resolver: answers from a canned `(name-with-trailing-dot, type) → values` map.
    struct MockResolver {
        records: HashMap<(String, DnsType), Vec<String>>,
    }
    impl MockResolver {
        fn new() -> Self {
            Self { records: HashMap::new() }
        }
        fn with(mut self, name: &str, t: DnsType, values: &[&str]) -> Self {
            self.records.insert(
                (fqdn(name), t),
                values.iter().map(|s| s.to_string()).collect(),
            );
            self
        }
    }
    impl Resolve for MockResolver {
        fn lookup<'a>(&'a self, name: &'a str, record: DnsType) -> ResolveFuture<'a> {
            let key = (fqdn(name), record);
            let values = self.records.get(&key).cloned().unwrap_or_default();
            Box::pin(async move { Ok(values) })
        }
    }

    fn status_of(check: &Value) -> &str {
        check.get("status").and_then(Value::as_str).unwrap_or("?")
    }

    #[tokio::test]
    async fn preset_all_correct_passes() {
        let resolver = MockResolver::new()
            .with("fiducia.cloud", DnsType::A, &["185.199.108.153"])
            .with("www.fiducia.cloud", DnsType::Cname, &["fiducia-cloud.github.io"])
            .with("app.fiducia.cloud", DnsType::A, &["95.217.171.250"])
            .with("admin.fiducia.cloud", DnsType::A, &["95.217.171.250"])
            .with(
                "fiducia.cloud",
                DnsType::Ns,
                &["ana.ns.cloudflare.com", "bob.ns.cloudflare.com"],
            );
        let out = dns_check(
            &resolver,
            DnsCheckInput { name: None, record_type: None, values: None, preset: Some("fiducia".into()) },
        )
        .await
        .unwrap();
        assert_eq!(out["summary"]["pass"], 5, "all five checks pass: {out}");
        assert_eq!(out["summary"]["pending"], 0);
        assert_eq!(out["summary"]["mismatch"], 0);
    }

    #[tokio::test]
    async fn preset_reports_pending_and_mismatch() {
        // app missing entirely → PENDING; admin wrong IP → MISMATCH; NS still at
        // the registrar → MISMATCH. GitHub Pages records present → PASS.
        let resolver = MockResolver::new()
            .with("fiducia.cloud", DnsType::A, &["185.199.109.153"])
            .with("www.fiducia.cloud", DnsType::A, &["185.199.110.153"])
            .with("admin.fiducia.cloud", DnsType::A, &["203.0.113.9"])
            .with("fiducia.cloud", DnsType::Ns, &["ns1.squarespacedns.com"]);
        let out = dns_check(
            &resolver,
            DnsCheckInput { name: None, record_type: None, values: None, preset: Some("fiducia".into()) },
        )
        .await
        .unwrap();
        let checks = out["checks"].as_array().unwrap();
        let by_name = |n: &str| checks.iter().find(|c| c["name"] == n).unwrap();
        assert_eq!(status_of(by_name("fiducia.cloud")), "PASS");
        assert_eq!(status_of(by_name("www.fiducia.cloud")), "PASS");
        assert_eq!(status_of(by_name("app.fiducia.cloud")), "PENDING");
        assert_eq!(status_of(by_name("admin.fiducia.cloud")), "MISMATCH");
        assert_eq!(status_of(by_name("fiducia.cloud")), "PASS"); // GH check
        // The NS check is keyed on the same name; find it by its expectation.
        let ns_check = checks
            .iter()
            .find(|c| c["expect"].as_str().unwrap_or("").contains("cloudflare"))
            .unwrap();
        assert_eq!(status_of(ns_check), "MISMATCH");
        assert_eq!(out["summary"]["pass"], 2);
        assert_eq!(out["summary"]["pending"], 1);
        assert_eq!(out["summary"]["mismatch"], 2);
    }

    #[tokio::test]
    async fn cloudflare_ns_detection() {
        let good = MockResolver::new().with(
            "fiducia.cloud",
            DnsType::Ns,
            &["ana.ns.cloudflare.com", "bob.ns.cloudflare.com"],
        );
        assert_eq!(status_of(&cloudflare_ns_check(&good, "fiducia.cloud").await), "PASS");

        let mixed = MockResolver::new().with(
            "fiducia.cloud",
            DnsType::Ns,
            &["ana.ns.cloudflare.com", "ns1.squarespacedns.com"],
        );
        assert_eq!(status_of(&cloudflare_ns_check(&mixed, "fiducia.cloud").await), "MISMATCH");

        let none = MockResolver::new();
        assert_eq!(status_of(&cloudflare_ns_check(&none, "fiducia.cloud").await), "PENDING");
    }

    #[tokio::test]
    async fn custom_check_matches_expected_value() {
        let resolver = MockResolver::new().with("x.fiducia.cloud", DnsType::A, &["10.0.0.1"]);
        let out = dns_check(
            &resolver,
            DnsCheckInput {
                name: Some("x.fiducia.cloud".into()),
                record_type: Some("A".into()),
                values: Some(vec!["10.0.0.1".into()]),
                preset: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(status_of(&out["checks"][0]), "PASS");
    }

    #[tokio::test]
    async fn dns_check_requires_name_or_preset() {
        let resolver = MockResolver::new();
        let err = dns_check(
            &resolver,
            DnsCheckInput { name: None, record_type: None, values: None, preset: None },
        )
        .await
        .unwrap_err();
        assert!(err.contains("preset"));
    }

    // ---- RDAP over an axum mock that includes a redirect hop ----

    fn canned_rdap() -> Value {
        json!({
            "objectClassName": "domain",
            "ldhName": "fiducia.cloud",
            "status": ["client transfer prohibited", "active"],
            "nameservers": [
                { "objectClassName": "nameserver", "ldhName": "ana.ns.cloudflare.com" },
                { "objectClassName": "nameserver", "ldhName": "bob.ns.cloudflare.com" }
            ],
            "entities": [{
                "objectClassName": "entity",
                "handle": "123",
                "roles": ["registrar"],
                "vcardArray": ["vcard", [
                    ["version", {}, "text", "4.0"],
                    ["fn", {}, "text", "Squarespace Domains II LLC"]
                ]]
            }],
            "events": [
                { "eventAction": "registration", "eventDate": "2020-01-01T00:00:00Z" },
                { "eventAction": "expiration", "eventDate": "2027-01-01T00:00:00Z" }
            ]
        })
    }

    async fn spawn(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn no_redirect_client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn rdap_follows_one_redirect_and_parses() {
        // /domain/{d} 302 → /registry/domain/{d} which serves the canned JSON.
        let app = Router::new()
            .route(
                "/domain/{d}",
                get(|Path(d): Path<String>| async move {
                    (
                        StatusCode::FOUND,
                        [(reqwest::header::LOCATION.as_str(), format!("/registry/domain/{d}"))],
                        "",
                    )
                }),
            )
            .route(
                "/registry/domain/{_d}",
                get(|_headers: HeaderMap| async move { Json(canned_rdap()) }),
            );
        let base = spawn(app).await;
        let out = registrar_status(&no_redirect_client(), &base, "fiducia.cloud")
            .await
            .unwrap();
        assert_eq!(out["registrar"], "Squarespace Domains II LLC");
        assert_eq!(out["nameservers"][0], "ana.ns.cloudflare.com");
        assert_eq!(out["nameservers"][1], "bob.ns.cloudflare.com");
        assert_eq!(out["expiry"], "2027-01-01T00:00:00Z");
        assert!(out["status"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "active"));
    }

    #[test]
    fn resolve_location_absolute_and_relative() {
        assert_eq!(resolve_location("http://x", "https://y/z"), "https://y/z");
        assert_eq!(resolve_location("http://x/", "/a/b"), "http://x/a/b");
        assert_eq!(resolve_location("http://x", "a/b"), "http://x/a/b");
    }
}
