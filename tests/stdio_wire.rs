use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const INTERNAL_SECRET: &str = "wire-test-internal-secret";
const API_KEY: &str = "wire-test-api-key";
const CLOUDFLARE_TOKEN: &str = "wire-test-cloudflare-token";

fn wait_with_timeout(mut child: Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().expect("collect child output"),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let output = child
                    .wait_with_output()
                    .expect("collect timed-out child output");
                panic!(
                    "fiducia-mcp did not exit within {:?}\nstdout:\n{}\nstderr:\n{}",
                    timeout,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(err) => panic!("wait for fiducia-mcp: {err}"),
        }
    }
}

fn responses_by_id(stdout: &str) -> BTreeMap<i64, Value> {
    let mut responses = BTreeMap::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line)
            .unwrap_or_else(|err| panic!("non-JSON stdout line: {line:?}: {err}"));
        let id = value
            .get("id")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("stdout JSON-RPC response has no numeric id: {value}"));
        responses.insert(id, value);
    }
    responses
}

#[test]
fn binary_stdio_keeps_protocol_stdout_and_blocks_mutations_by_default() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fiducia-mcp"))
        .env("RUST_LOG", "info")
        .env("FIDUCIA_INTERNAL_SECRET", INTERNAL_SECRET)
        .env("FIDUCIA_API_KEY", API_KEY)
        .env("CLOUDFLARE_API_TOKEN", CLOUDFLARE_TOKEN)
        .env_remove("FIDUCIA_MCP_ALLOW_MUTATIONS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fiducia-mcp binary");

    let mut stdin = child.stdin.take().expect("child stdin");
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "wire-test", "version": "0" }
        }
    });
    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    let list_tools = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    });
    let gated_mutation = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "cloudflare_dns_upsert",
            "arguments": {
                "zone": "fiducia.cloud",
                "type": "TXT",
                "name": "_wire-test.fiducia.cloud",
                "content": "should-not-be-written"
            }
        }
    });

    for message in [initialize, initialized, list_tools, gated_mutation] {
        writeln!(stdin, "{message}").expect("write JSON-RPC request");
    }
    drop(stdin);

    let output = wait_with_timeout(child, Duration::from_secs(10));
    assert!(
        output.status.success(),
        "fiducia-mcp exited unsuccessfully: {:?}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout utf8");
    let stderr = String::from_utf8(output.stderr).expect("stderr utf8");

    assert!(
        stderr.contains("starting fiducia-mcp"),
        "startup log should be written to stderr"
    );
    assert!(
        !stdout.contains("starting fiducia-mcp"),
        "stdout must stay reserved for JSON-RPC frames"
    );
    for secret in [INTERNAL_SECRET, API_KEY, CLOUDFLARE_TOKEN] {
        assert!(!stdout.contains(secret), "stdout leaked secret {secret:?}");
        assert!(!stderr.contains(secret), "stderr leaked secret {secret:?}");
    }

    let responses = responses_by_id(&stdout);
    assert_eq!(
        responses
            .get(&1)
            .and_then(|v| v.pointer("/result/serverInfo/name")),
        Some(&Value::String("fiducia-mcp-server".to_string()))
    );
    assert!(
        responses
            .get(&2)
            .and_then(|v| v.pointer("/result/tools"))
            .and_then(Value::as_array)
            .is_some_and(|tools| tools.iter().any(|tool| {
                tool.get("name").and_then(Value::as_str) == Some("cloudflare_dns_upsert")
            })),
        "tools/list should expose cloudflare_dns_upsert"
    );

    let mutation = responses.get(&3).expect("mutation response");
    assert_eq!(
        mutation.pointer("/result/isError").and_then(Value::as_bool),
        Some(true),
        "mutation tool should return a tool error result: {mutation}"
    );
    let mutation_text = mutation.to_string();
    assert!(mutation_text.contains("mutations are disabled"));
    assert!(mutation_text.contains("FIDUCIA_MCP_ALLOW_MUTATIONS"));
}


// End-to-end crossing *both* transports: an external JSON-RPC stdio client
// drives the compiled MCP binary, whose production reqwest client calls a real
// loopback Brain server. Deliberately no mocked fetch, cloud state, or tokens.
#[derive(Debug, Clone)]
struct BrainWireObservation {
    path: String,
    internal_auth: Option<String>,
    authorization: Option<String>,
    tenant_id: Option<String>,
}

#[derive(Clone)]
struct BrainWireState {
    observations: std::sync::Arc<std::sync::Mutex<Vec<BrainWireObservation>>>,
    redirect_to: Option<String>,
}

async fn brain_wire_handler(
    axum::extract::State(state): axum::extract::State<BrainWireState>,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let value = |header: &str| {
        headers
            .get(header)
            .and_then(|item| item.to_str().ok())
            .map(str::to_owned)
    };
    state
        .observations
        .lock()
        .expect("brain observation mutex")
        .push(BrainWireObservation {
            path: uri.to_string(),
            internal_auth: value("x-fiducia-internal-auth"),
            authorization: value("authorization"),
            tenant_id: value("x-fiducia-org-id"),
        });
    if let Some(target) = &state.redirect_to {
        return (
            axum::http::StatusCode::TEMPORARY_REDIRECT,
            [(axum::http::header::LOCATION, target.clone())],
        )
            .into_response();
    }
    axum::Json(json!({
        "nodes": [{ "node_id": "brain-node-a", "health": "healthy" }]
    }))
    .into_response()
}

async fn start_brain_wire(
    redirect_to: Option<String>,
) -> (String, BrainWireState, tokio::task::JoinHandle<()>) {
    let state = BrainWireState {
        observations: Default::default(),
        redirect_to,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind disposable Brain");
    let origin = format!("http://{}", listener.local_addr().expect("Brain socket"));
    let router = axum::Router::new()
        .fallback(axum::routing::get(brain_wire_handler))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("Brain fixture serve");
    });
    (origin, state, server)
}

fn call_real_stdio_for_brain(brain_url: &str) -> BTreeMap<i64, Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fiducia-mcp"))
        .env("FIDUCIA_BRAIN_URL", brain_url)
        .env("FIDUCIA_INTERNAL_SECRET", INTERNAL_SECRET)
        .env_remove("FIDUCIA_API_KEY")
        .env_remove("FIDUCIA_CONTROL_PLANE_SECRET")
        .env_remove("OTEL_EXPORTER_OTLP_ENDPOINT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch production MCP stdio server");
    let mut stdin = child.stdin.take().expect("MCP stdin");
    for frame in [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "e2e-stdio-to-brain", "version": "1" }
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": { "name": "cluster_nodes", "arguments": {} }
        }),
    ] {
        writeln!(stdin, "{frame}").expect("write MCP JSON-RPC frame");
    }
    drop(stdin);
    let output = wait_with_timeout(child, Duration::from_secs(15));
    assert!(
        output.status.success(),
        "MCP exit {:?}: stderr = {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("MCP stdout is UTF-8");
    assert!(
        stdout.lines().filter(|line| !line.is_empty()).all(|line| serde_json::from_str::<Value>(line).is_ok()),
        "stdout must contain only JSON-RPC frames"
    );
    assert!(!stdout.contains(INTERNAL_SECRET), "stdio client must not receive server secret");
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains(INTERNAL_SECRET),
        "server diagnostics must not reveal trusted-hop credentials"
    );
    responses_by_id(&stdout)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_stdio_mcp_to_brain_http_auth_and_redirect_containment() {
    let (brain_url, brain_state, brain_task) = start_brain_wire(None).await;
    let responses = tokio::task::spawn_blocking(move || call_real_stdio_for_brain(&brain_url))
        .await
        .expect("stdio blocking task");
    assert_eq!(
        responses.get(&1).and_then(|v| v.pointer("/result/serverInfo/name")).and_then(Value::as_str),
        Some("fiducia-mcp-server")
    );
    let result = responses.get(&2).expect("cluster_nodes tool reply");
    assert_ne!(result.pointer("/result/isError").and_then(Value::as_bool), Some(true));
    let content = result.pointer("/result/content/0/text").and_then(Value::as_str).expect("tool content");
    let body: Value = serde_json::from_str(content).expect("MCP result JSON");
    assert_eq!(body["nodes"][0]["node_id"], "brain-node-a");
    let requests = brain_state.observations.lock().expect("brain observations");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/nodes");
    assert_eq!(requests[0].internal_auth.as_deref(), Some(INTERNAL_SECRET));
    assert_eq!(requests[0].authorization, None);
    assert_eq!(requests[0].tenant_id, None, "Brain auth must not inherit node tenant");
    drop(requests);
    brain_task.abort();

    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let attacker_hits = std::sync::Arc::clone(&hits);
    let attacker_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind attacker test server");
    let attacker_origin = format!("http://{}", attacker_listener.local_addr().expect("attacker port"));
    let attacker_router = axum::Router::new().fallback(axum::routing::get(move || {
        let attacker_hits = std::sync::Arc::clone(&attacker_hits);
        async move {
            attacker_hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            axum::Json(json!({ "stolen": true }))
        }
    }));
    let attacker_task = tokio::spawn(async move {
        axum::serve(attacker_listener, attacker_router)
            .await
            .expect("serve attacker test listener");
    });
    let (redirect_brain_url, redirect_state, redirect_task) =
        start_brain_wire(Some(format!("{attacker_origin}/credential-capture"))).await;
    let denied = tokio::task::spawn_blocking(move || call_real_stdio_for_brain(&redirect_brain_url))
        .await
        .expect("redirect stdio blocking task");
    let error_result = denied.get(&2).expect("redirect error result");
    assert_eq!(error_result.pointer("/result/isError").and_then(Value::as_bool), Some(true));
    let text = error_result.pointer("/result/content/0/text").and_then(Value::as_str).expect("error text");
    assert!(text.contains("307"), "a redirect must be reported as an error: {text}");
    assert_eq!(
        hits.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "MCP must not send Brain trusted-hop auth to redirect destinations"
    );
    let redirect_requests = redirect_state.observations.lock().expect("redirect observations");
    assert_eq!(redirect_requests.len(), 1);
    assert_eq!(redirect_requests[0].internal_auth.as_deref(), Some(INTERNAL_SECRET));
    drop(redirect_requests);
    redirect_task.abort();
    attacker_task.abort();
}
