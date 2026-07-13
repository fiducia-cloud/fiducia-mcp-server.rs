//! Cloudflare v4 REST API tools: list zones + DNS records (read-only) and,
//! behind a mutation gate, create-or-update and delete DNS records.
//!
//! Auth is `Authorization: Bearer $CLOUDFLARE_API_TOKEN`. The token is only
//! ever attached as a request header — it is NEVER logged, echoed into an
//! error, or interpolated into any string. Cloudflare's own error envelope
//! (`{success:false, errors:[{code,message}]}`) is surfaced verbatim; it does
//! not contain the token.
//!
//! Mutations (`dns_upsert`, `dns_delete`) additionally require
//! `FIDUCIA_MCP_ALLOW_MUTATIONS=1`; without it the tool returns an error
//! explaining the gate and never touches the API.

use serde_json::{json, Value};
use std::time::Duration;

use crate::upstream::{env_nonempty, urlencode};

pub const CF_TOKEN_ENV: &str = "CLOUDFLARE_API_TOKEN";
pub const ALLOW_MUTATIONS_ENV: &str = "FIDUCIA_MCP_ALLOW_MUTATIONS";
pub const CF_API_BASE: &str = "https://api.cloudflare.com/client/v4";

/// Record types `dns_upsert` will write; anything else is rejected up front.
const UPSERT_TYPES: [&str; 5] = ["A", "AAAA", "CNAME", "TXT", "MX"];
/// Types for which Cloudflare accepts the `proxied` flag (orange cloud).
const PROXYABLE: [&str; 3] = ["A", "AAAA", "CNAME"];

/// Owned inputs for `dns_upsert` (built by the server from the tool schema).
pub struct UpsertParams {
    pub zone: String,
    pub record_type: String,
    pub name: String,
    pub content: String,
    /// TTL in seconds; `1` means "automatic". Defaults to 1.
    pub ttl: Option<i64>,
    /// Proxy through Cloudflare (only honored for A/AAAA/CNAME). Defaults to false.
    pub proxied: Option<bool>,
}

pub struct Cloudflare {
    client: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl Cloudflare {
    /// Build from the environment: reads `CLOUDFLARE_API_TOKEN` (may be unset —
    /// the tools then return a "set the env var" error rather than crashing).
    pub fn from_env() -> Self {
        Self::with_base(CF_API_BASE.to_string(), env_nonempty(CF_TOKEN_ENV))
    }

    /// Construct against an explicit base URL + token. Used by tests to point
    /// at a local mock; production goes through [`Cloudflare::from_env`].
    pub fn with_base(base: String, token: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Self {
            client,
            base: base.trim_end_matches('/').to_string(),
            token,
        }
    }

    fn token(&self) -> Result<&str, String> {
        self.token.as_deref().ok_or_else(|| {
            format!(
                "{CF_TOKEN_ENV} is not set; create a Cloudflare API token \
                 (Zone:Read + DNS:Edit) and export it to use the cloudflare_* tools"
            )
        })
    }

    /// Send one request, attach the bearer token, and unwrap Cloudflare's
    /// success envelope. Non-`success` responses (network 2xx or not) become a
    /// readable error built only from `errors[].{code,message}` — never the token.
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, String> {
        let token = self.token()?;
        let url = format!("{}{}", self.base, path);
        let mut req = self.client.request(method, &url).bearer_auth(token);
        if let Some(body) = &body {
            req = req.json(body);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("Cloudflare request to {url} failed: {e}"))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("reading Cloudflare response from {url} failed: {e}"))?;
        let json: Value =
            serde_json::from_str(&text).unwrap_or_else(|_| Value::String(text.clone()));
        let success = json
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if success {
            Ok(json)
        } else {
            Err(cf_error(&json, status))
        }
    }

    /// Resolve a zone *name* or *id* to a zone id (names go through
    /// `GET /zones?name=`; a 32-char hex string is taken as an id directly).
    async fn zone_id(&self, zone: &str) -> Result<String, String> {
        let zone = zone.trim();
        if is_zone_id(zone) {
            return Ok(zone.to_string());
        }
        let path = format!("/zones?name={}", urlencode(zone));
        let resp = self.send(reqwest::Method::GET, &path, None).await?;
        resp.get("result")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|z| z.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("no Cloudflare zone found for name {zone:?}"))
    }

    /// `GET /zones` → `[{name,id,status,name_servers}]`.
    pub async fn zones(&self) -> Result<Value, String> {
        let resp = self
            .send(reqwest::Method::GET, "/zones?per_page=50", None)
            .await?;
        let zones: Vec<Value> = resp
            .get("result")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(|z| {
                        json!({
                            "name": z.get("name"),
                            "id": z.get("id"),
                            "status": z.get("status"),
                            "name_servers": z.get("name_servers"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({ "count": zones.len(), "zones": zones }))
    }

    /// `GET /zones/{id}/dns_records` with pagination (per_page=100, following
    /// `result_info.total_pages`) → `[{id,type,name,content,proxied,ttl}]`.
    pub async fn dns_records(&self, zone: &str) -> Result<Value, String> {
        let id = self.zone_id(zone).await?;
        let mut records = Vec::new();
        let mut page = 1u32;
        loop {
            let path = format!("/zones/{id}/dns_records?per_page=100&page={page}");
            let resp = self.send(reqwest::Method::GET, &path, None).await?;
            if let Some(arr) = resp.get("result").and_then(Value::as_array) {
                records.extend(arr.iter().map(summarize_record));
            }
            let total_pages = resp
                .get("result_info")
                .and_then(|ri| ri.get("total_pages"))
                .and_then(Value::as_u64)
                .unwrap_or(1) as u32;
            if page >= total_pages {
                break;
            }
            page += 1;
        }
        Ok(json!({ "zone_id": id, "count": records.len(), "records": records }))
    }

    /// Create-or-update a DNS record, matched on `(type, name)`. **Gated** by
    /// `FIDUCIA_MCP_ALLOW_MUTATIONS=1`.
    pub async fn dns_upsert(&self, params: UpsertParams) -> Result<Value, String> {
        mutation_gate()?;
        let record_type = params.record_type.trim().to_ascii_uppercase();
        if !UPSERT_TYPES.contains(&record_type.as_str()) {
            return Err(format!(
                "record type {record_type:?} is not allowed for dns_upsert; \
                 allowed types: {UPSERT_TYPES:?}"
            ));
        }
        if params.name.trim().is_empty() || params.content.trim().is_empty() {
            return Err("both `name` and `content` are required for dns_upsert".to_string());
        }
        let id = self.zone_id(&params.zone).await?;
        let ttl = params.ttl.unwrap_or(1);
        let mut body = json!({
            "type": record_type,
            "name": params.name.trim(),
            "content": params.content.trim(),
            "ttl": ttl,
        });
        if PROXYABLE.contains(&record_type.as_str()) {
            body["proxied"] = json!(params.proxied.unwrap_or(false));
        }

        // Find an existing record with the same (type, name) to decide POST vs PUT.
        let find = format!(
            "/zones/{id}/dns_records?type={}&name={}",
            urlencode(&record_type),
            urlencode(params.name.trim())
        );
        let existing = self.send(reqwest::Method::GET, &find, None).await?;
        let existing_id = existing
            .get("result")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|r| r.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string);

        let (resp, action) = match existing_id {
            Some(rec) => {
                let path = format!("/zones/{id}/dns_records/{rec}");
                (self.send(reqwest::Method::PUT, &path, Some(body)).await?, "updated")
            }
            None => {
                let path = format!("/zones/{id}/dns_records");
                (self.send(reqwest::Method::POST, &path, Some(body)).await?, "created")
            }
        };
        let record = resp.get("result").map(summarize_record).unwrap_or(Value::Null);
        Ok(json!({ "action": action, "zone_id": id, "record": record }))
    }

    /// Delete a DNS record by explicit id. **Gated** by `FIDUCIA_MCP_ALLOW_MUTATIONS=1`.
    pub async fn dns_delete(&self, zone: &str, record_id: &str) -> Result<Value, String> {
        mutation_gate()?;
        let record_id = record_id.trim();
        if record_id.is_empty() {
            return Err("`record_id` is required for cloudflare_dns_delete".to_string());
        }
        let id = self.zone_id(zone).await?;
        let path = format!("/zones/{id}/dns_records/{record_id}");
        let resp = self.send(reqwest::Method::DELETE, &path, None).await?;
        Ok(json!({
            "action": "deleted",
            "zone_id": id,
            "result": resp.get("result").cloned().unwrap_or(Value::Null),
        }))
    }
}

/// True when `FIDUCIA_MCP_ALLOW_MUTATIONS=1` — the only switch that unlocks writes.
pub fn mutations_allowed() -> bool {
    env_nonempty(ALLOW_MUTATIONS_ENV).as_deref() == Some("1")
}

fn mutation_gate() -> Result<(), String> {
    if mutations_allowed() {
        Ok(())
    } else {
        Err(format!(
            "mutations are disabled (this server is read-only by default). \
             Set {ALLOW_MUTATIONS_ENV}=1 to allow Cloudflare DNS writes — the \
             only mutating tools here."
        ))
    }
}

/// A Cloudflare zone id is 32 lowercase hex characters; a domain name is not.
fn is_zone_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Pick the fields we expose for a DNS record.
fn summarize_record(r: &Value) -> Value {
    json!({
        "id": r.get("id"),
        "type": r.get("type"),
        "name": r.get("name"),
        "content": r.get("content"),
        "proxied": r.get("proxied"),
        "ttl": r.get("ttl"),
    })
}

/// Map Cloudflare's error envelope to a readable string. Only `code`+`message`
/// from `errors[]` are used, so the token can never appear here.
fn cf_error(json: &Value, status: reqwest::StatusCode) -> String {
    if let Some(errors) = json.get("errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            let parts: Vec<String> = errors
                .iter()
                .map(|e| {
                    let code = e.get("code").and_then(Value::as_i64).unwrap_or(0);
                    let msg = e
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("(no message)");
                    format!("[{code}] {msg}")
                })
                .collect();
            return format!("Cloudflare API error ({status}): {}", parts.join("; "));
        }
    }
    format!("Cloudflare API error ({status})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::Query,
        http::{HeaderMap, StatusCode},
        routing::{delete, get, post, put},
        Json, Router,
    };
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// Serializes tests that mutate `FIDUCIA_MCP_ALLOW_MUTATIONS` (process-global).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct MutationGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
    }
    impl MutationGuard {
        fn set(value: Option<&str>) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var(ALLOW_MUTATIONS_ENV).ok();
            match value {
                Some(v) => std::env::set_var(ALLOW_MUTATIONS_ENV, v),
                None => std::env::remove_var(ALLOW_MUTATIONS_ENV),
            }
            Self { _lock: lock, prev }
        }
    }
    impl Drop for MutationGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(ALLOW_MUTATIONS_ENV, v),
                None => std::env::remove_var(ALLOW_MUTATIONS_ENV),
            }
        }
    }

    async fn spawn(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn require_bearer(headers: &HeaderMap, seen: &Mutex<Option<String>>) -> bool {
        let got = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        *seen.lock().unwrap() = got.clone();
        got.as_deref() == Some("Bearer test-token")
    }

    #[tokio::test]
    async fn zones_sends_bearer_and_maps_fields() {
        let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let seen2 = Arc::clone(&seen);
        let app = Router::new().route(
            "/zones",
            get(move |headers: HeaderMap| {
                let seen = Arc::clone(&seen2);
                async move {
                    if !require_bearer(&headers, &seen) {
                        return (StatusCode::FORBIDDEN, Json(json!({"success": false, "errors": []})));
                    }
                    (
                        StatusCode::OK,
                        Json(json!({
                            "success": true,
                            "errors": [],
                            "result": [{
                                "name": "fiducia.cloud", "id": "z1", "status": "active",
                                "name_servers": ["a.ns.cloudflare.com", "b.ns.cloudflare.com"],
                                "extra": "dropped"
                            }]
                        })),
                    )
                }
            }),
        );
        let base = spawn(app).await;
        let cf = Cloudflare::with_base(base, Some("test-token".into()));
        let out = cf.zones().await.unwrap();
        assert_eq!(seen.lock().unwrap().as_deref(), Some("Bearer test-token"));
        assert_eq!(out["count"], 1);
        assert_eq!(out["zones"][0]["name"], "fiducia.cloud");
        assert_eq!(out["zones"][0]["id"], "z1");
        assert!(out["zones"][0].get("extra").is_none(), "only the listed fields are kept");
    }

    #[tokio::test]
    async fn dns_records_follows_pagination() {
        let app = Router::new().route(
            "/zones/{id}/dns_records",
            get(|Query(q): Query<HashMap<String, String>>| async move {
                let page: u32 = q.get("page").and_then(|p| p.parse().ok()).unwrap_or(1);
                let rec = json!({
                    "id": format!("r{page}"), "type": "A", "name": format!("h{page}.fiducia.cloud"),
                    "content": "1.2.3.4", "proxied": false, "ttl": 1
                });
                Json(json!({
                    "success": true, "errors": [], "result": [rec],
                    "result_info": { "page": page, "total_pages": 2 }
                }))
            }),
        );
        let base = spawn(app).await;
        let cf = Cloudflare::with_base(base, Some("test-token".into()));
        // 32-hex zone id so no name resolution round-trip is needed.
        let out = cf.dns_records("0123456789abcdef0123456789abcdef").await.unwrap();
        assert_eq!(out["count"], 2, "both pages accumulated");
        assert_eq!(out["records"][0]["id"], "r1");
        assert_eq!(out["records"][1]["id"], "r2");
    }

    #[tokio::test]
    async fn error_envelope_is_mapped_without_token() {
        let app = Router::new().route(
            "/zones",
            get(|| async {
                (
                    StatusCode::FORBIDDEN,
                    Json(json!({
                        "success": false,
                        "errors": [{ "code": 9109, "message": "Invalid access token" }]
                    })),
                )
            }),
        );
        let base = spawn(app).await;
        let cf = Cloudflare::with_base(base, Some("super-secret-token".into()));
        let err = cf.zones().await.unwrap_err();
        assert!(err.contains("9109"), "surfaces the CF error code: {err}");
        assert!(err.contains("Invalid access token"), "surfaces the message: {err}");
        assert!(!err.contains("super-secret-token"), "must never leak the token: {err}");
    }

    #[tokio::test]
    async fn missing_token_names_the_env_var() {
        let cf = Cloudflare::with_base("http://127.0.0.1:1".into(), None);
        let err = cf.zones().await.unwrap_err();
        assert!(err.contains(CF_TOKEN_ENV));
    }

    fn upsert_mock(calls: Arc<Mutex<Vec<String>>>, existing: bool) -> Router {
        let c_find = Arc::clone(&calls);
        let c_post = Arc::clone(&calls);
        let c_put = Arc::clone(&calls);
        Router::new()
            .route(
                "/zones/{id}/dns_records",
                get(move |Query(q): Query<HashMap<String, String>>| {
                    let calls = Arc::clone(&c_find);
                    async move {
                        calls.lock().unwrap().push(format!(
                            "FIND {}/{}",
                            q.get("type").cloned().unwrap_or_default(),
                            q.get("name").cloned().unwrap_or_default()
                        ));
                        let result = if existing {
                            json!([{ "id": "existing-rec", "type": "A", "name": q.get("name") }])
                        } else {
                            json!([])
                        };
                        Json(json!({ "success": true, "errors": [], "result": result }))
                    }
                })
                .post(move |Json(body): Json<Value>| {
                    let calls = Arc::clone(&c_post);
                    async move {
                        calls.lock().unwrap().push("POST".into());
                        Json(json!({ "success": true, "errors": [], "result": {
                            "id": "new-rec", "type": body["type"], "name": body["name"],
                            "content": body["content"], "proxied": body.get("proxied"), "ttl": body["ttl"]
                        }}))
                    }
                }),
            )
            .route(
                "/zones/{id}/dns_records/{rec}",
                put(move |Json(body): Json<Value>| {
                    let calls = Arc::clone(&c_put);
                    async move {
                        calls.lock().unwrap().push("PUT".into());
                        Json(json!({ "success": true, "errors": [], "result": {
                            "id": "existing-rec", "type": body["type"], "name": body["name"],
                            "content": body["content"], "proxied": body.get("proxied"), "ttl": body["ttl"]
                        }}))
                    }
                }),
            )
    }

    #[tokio::test]
    async fn upsert_creates_when_absent() {
        let _g = MutationGuard::set(Some("1"));
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let base = spawn(upsert_mock(Arc::clone(&calls), false)).await;
        let cf = Cloudflare::with_base(base, Some("test-token".into()));
        let out = cf
            .dns_upsert(UpsertParams {
                zone: "0123456789abcdef0123456789abcdef".into(),
                record_type: "a".into(), // lowercase → normalized
                name: "app.fiducia.cloud".into(),
                content: "95.217.171.250".into(),
                ttl: None,
                proxied: None,
            })
            .await
            .unwrap();
        assert_eq!(out["action"], "created");
        assert_eq!(out["record"]["id"], "new-rec");
        let calls = calls.lock().unwrap();
        assert!(calls.iter().any(|c| c.starts_with("FIND A/app.fiducia.cloud")));
        assert!(calls.contains(&"POST".to_string()));
        assert!(!calls.contains(&"PUT".to_string()));
    }

    #[tokio::test]
    async fn upsert_updates_when_present() {
        let _g = MutationGuard::set(Some("1"));
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let base = spawn(upsert_mock(Arc::clone(&calls), true)).await;
        let cf = Cloudflare::with_base(base, Some("test-token".into()));
        let out = cf
            .dns_upsert(UpsertParams {
                zone: "0123456789abcdef0123456789abcdef".into(),
                record_type: "A".into(),
                name: "app.fiducia.cloud".into(),
                content: "95.217.171.250".into(),
                ttl: Some(300),
                proxied: Some(true),
            })
            .await
            .unwrap();
        assert_eq!(out["action"], "updated");
        assert_eq!(out["record"]["id"], "existing-rec");
        let calls = calls.lock().unwrap();
        assert!(calls.contains(&"PUT".to_string()));
        assert!(!calls.contains(&"POST".to_string()));
    }

    #[tokio::test]
    async fn upsert_rejects_disallowed_type() {
        let _g = MutationGuard::set(Some("1"));
        let cf = Cloudflare::with_base("http://127.0.0.1:1".into(), Some("test-token".into()));
        let err = cf
            .dns_upsert(UpsertParams {
                zone: "0123456789abcdef0123456789abcdef".into(),
                record_type: "NS".into(),
                name: "x.fiducia.cloud".into(),
                content: "a.ns.cloudflare.com".into(),
                ttl: None,
                proxied: None,
            })
            .await
            .unwrap_err();
        assert!(err.contains("NS"));
        assert!(err.contains("not allowed"));
    }

    #[tokio::test]
    async fn upsert_denied_without_gate_and_never_calls_api() {
        let _g = MutationGuard::set(None);
        let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let base = spawn(upsert_mock(Arc::clone(&calls), false)).await;
        let cf = Cloudflare::with_base(base, Some("test-token".into()));
        let err = cf
            .dns_upsert(UpsertParams {
                zone: "0123456789abcdef0123456789abcdef".into(),
                record_type: "A".into(),
                name: "app.fiducia.cloud".into(),
                content: "95.217.171.250".into(),
                ttl: None,
                proxied: None,
            })
            .await
            .unwrap_err();
        assert!(err.contains(ALLOW_MUTATIONS_ENV), "explains the gate: {err}");
        assert!(calls.lock().unwrap().is_empty(), "gate blocks before any HTTP call");
    }

    #[tokio::test]
    async fn delete_requires_gate() {
        let _g = MutationGuard::set(None);
        let cf = Cloudflare::with_base("http://127.0.0.1:1".into(), Some("test-token".into()));
        let err = cf
            .dns_delete("0123456789abcdef0123456789abcdef", "rec-1")
            .await
            .unwrap_err();
        assert!(err.contains(ALLOW_MUTATIONS_ENV));
    }

    #[tokio::test]
    async fn delete_hits_record_path_when_gated() {
        let _g = MutationGuard::set(Some("1"));
        let hit: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let hit2 = Arc::clone(&hit);
        let app = Router::new().route(
            "/zones/{id}/dns_records/{rec}",
            delete(move |axum::extract::Path((_id, rec)): axum::extract::Path<(String, String)>| {
                let hit = Arc::clone(&hit2);
                async move {
                    *hit.lock().unwrap() = Some(rec.clone());
                    Json(json!({ "success": true, "errors": [], "result": { "id": rec } }))
                }
            }),
        );
        let base = spawn(app).await;
        let cf = Cloudflare::with_base(base, Some("test-token".into()));
        let out = cf
            .dns_delete("0123456789abcdef0123456789abcdef", "rec-42")
            .await
            .unwrap();
        assert_eq!(out["action"], "deleted");
        assert_eq!(hit.lock().unwrap().as_deref(), Some("rec-42"));
    }

    #[test]
    fn zone_id_detection() {
        assert!(is_zone_id("0123456789abcdef0123456789abcdef"));
        assert!(!is_zone_id("fiducia.cloud"));
        assert!(!is_zone_id("0123456789abcdef")); // too short
    }
}
