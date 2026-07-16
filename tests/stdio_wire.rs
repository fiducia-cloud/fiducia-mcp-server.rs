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
