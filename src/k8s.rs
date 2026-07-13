//! Kubernetes diagnostics by shelling out to `kubectl` — deliberately NOT
//! kube-rs. kubectl already carries the operator's kubeconfig, contexts, and
//! auth exec plugins, and honors `$KUBECONFIG`.
//!
//! Hard rules enforced here:
//! - argv is always a `Vec<String>` — never a shell string, so nothing is word-split.
//! - only read-only verbs (`config get-contexts`, `get -o json`, `rollout
//!   status --watch=false`, `top`) are ever invoked.
//! - every `--context` is validated against `kubectl config get-contexts -o name`
//!   before use, and optionally restricted further by `FIDUCIA_K8S_CONTEXTS`.
//! - every call is wrapped in a 15s timeout.
//! - large summaries are truncated to ~32KB with a note.

use serde_json::{json, Value};
use std::time::Duration;
use tokio::process::Command;

use crate::upstream::env_nonempty;

pub const K8S_CONTEXTS_ENV: &str = "FIDUCIA_K8S_CONTEXTS";
pub const DEFAULT_NAMESPACE: &str = "fiducia";
const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_OUTPUT: usize = 32 * 1024;

#[derive(Debug)]
struct RunOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

/// Spawn `kubectl <args>` with the standard 15s timeout. `args` is passed
/// verbatim as argv, never through a shell.
async fn run_kubectl(args: &[String]) -> Result<RunOutput, String> {
    run_kubectl_with(args, TIMEOUT).await
}

/// As [`run_kubectl`], but with a caller-chosen timeout (tests use a short one
/// to exercise the timeout branch without waiting 15s). `kill_on_drop` ensures
/// a timed-out child is reaped rather than leaked.
async fn run_kubectl_with(args: &[String], timeout: Duration) -> Result<RunOutput, String> {
    let fut = Command::new("kubectl").args(args).kill_on_drop(true).output();
    match tokio::time::timeout(timeout, fut).await {
        Err(_) => Err(format!(
            "kubectl timed out after {}s: kubectl {}",
            timeout.as_secs().max(1),
            args.join(" ")
        )),
        Ok(Err(e)) => Err(format!(
            "failed to run kubectl (is it installed and on PATH?): {e}"
        )),
        Ok(Ok(out)) => Ok(RunOutput {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }),
    }
}

/// Run kubectl and parse stdout as JSON; a non-zero exit becomes an error
/// carrying stderr.
async fn run_json(args: &[String]) -> Result<Value, String> {
    let out = run_kubectl(args).await?;
    if !out.success {
        return Err(format!(
            "kubectl {} failed: {}",
            args.join(" "),
            out.stderr.trim()
        ));
    }
    serde_json::from_str(&out.stdout)
        .map_err(|e| format!("kubectl {} did not return JSON: {e}", args.join(" ")))
}

/// Contexts allowed by `FIDUCIA_K8S_CONTEXTS` (csv), or `None` when unset.
fn context_allowlist() -> Option<Vec<String>> {
    env_nonempty(K8S_CONTEXTS_ENV).map(|s| {
        s.split(',')
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .collect()
    })
}

/// All contexts kubectl knows about (`config get-contexts -o name`).
async fn available_contexts() -> Result<Vec<String>, String> {
    let out = run_kubectl(&[
        "config".into(),
        "get-contexts".into(),
        "-o".into(),
        "name".into(),
    ])
    .await?;
    if !out.success {
        return Err(format!(
            "kubectl config get-contexts failed: {}",
            out.stderr.trim()
        ));
    }
    Ok(out
        .stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Reject a context that is unknown to kubectl or excluded by the allowlist.
async fn validate_context(context: &str) -> Result<String, String> {
    let context = context.trim();
    if context.is_empty() {
        return Err("`context` is required".to_string());
    }
    if let Some(allow) = context_allowlist() {
        if !allow.iter().any(|a| a == context) {
            return Err(format!(
                "context {context:?} is not permitted by {K8S_CONTEXTS_ENV}; allowed: {allow:?}"
            ));
        }
    }
    let available = available_contexts().await?;
    if !available.iter().any(|a| a == context) {
        return Err(format!(
            "unknown kubectl context {context:?}; known contexts: {available:?}"
        ));
    }
    Ok(context.to_string())
}

fn namespace_or_default(namespace: &str) -> String {
    let ns = namespace.trim();
    if ns.is_empty() {
        DEFAULT_NAMESPACE.to_string()
    } else {
        ns.to_string()
    }
}

/// Base argv for a namespaced command: `--context <ctx> -n <ns>`.
fn scoped(context: &str, namespace: &str) -> Vec<String> {
    vec![
        "--context".to_string(),
        context.to_string(),
        "-n".to_string(),
        namespace.to_string(),
    ]
}

/// Truncate an oversized value to a preview with a note.
fn cap_output(value: Value) -> Value {
    let serialized = serde_json::to_string(&value).unwrap_or_default();
    if serialized.len() > MAX_OUTPUT {
        json!({
            "truncated": true,
            "note": format!("output exceeded {MAX_OUTPUT} bytes; showing a truncated preview"),
            "preview": serialized.chars().take(MAX_OUTPUT).collect::<String>(),
        })
    } else {
        value
    }
}

// ------------------------------------------------------------------ tools

/// `k8s_contexts`: list known contexts and mark which are allowed.
pub async fn contexts() -> Result<Value, String> {
    let available = available_contexts().await?;
    let allow = context_allowlist();
    let list: Vec<Value> = available
        .iter()
        .map(|c| {
            let allowed = allow.as_ref().map(|a| a.iter().any(|x| x == c)).unwrap_or(true);
            json!({ "name": c, "allowed": allowed })
        })
        .collect();
    Ok(json!({ "contexts": list, "restricted_to": allow }))
}

/// `k8s_workloads`: deployments/statefulsets (ready/desired + images) plus the
/// pod list (phase/restarts/node/created) in a namespace.
pub async fn workloads(context: &str, namespace: &str) -> Result<Value, String> {
    let context = validate_context(context).await?;
    let namespace = namespace_or_default(namespace);
    let base = scoped(&context, &namespace);

    let mut workload_args = base.clone();
    workload_args.extend([
        "get".into(),
        "deployments,statefulsets".into(),
        "-o".into(),
        "json".into(),
    ]);
    let workloads_json = run_json(&workload_args).await?;

    let mut pod_args = base;
    pod_args.extend(["get".into(), "pods".into(), "-o".into(), "json".into()]);
    let pods_json = run_json(&pod_args).await?;

    Ok(cap_output(json!({
        "context": context,
        "namespace": namespace,
        "workloads": summarize_workloads(&workloads_json),
        "pods": summarize_pods(&pods_json),
    })))
}

/// `k8s_rollout_status`: current rollout state (non-blocking, `--watch=false`).
pub async fn rollout_status(
    context: &str,
    kind: &str,
    name: &str,
    namespace: &str,
) -> Result<Value, String> {
    let context = validate_context(context).await?;
    let kind = kind.trim().to_ascii_lowercase();
    if kind != "deployment" && kind != "statefulset" {
        return Err(format!(
            "`kind` must be \"deployment\" or \"statefulset\", got {kind:?}"
        ));
    }
    let name = name.trim();
    if name.is_empty() {
        return Err("`name` is required".to_string());
    }
    let namespace = namespace_or_default(namespace);
    let mut args = scoped(&context, &namespace);
    args.extend([
        "rollout".into(),
        "status".into(),
        format!("{kind}/{name}"),
        "--watch=false".into(),
    ]);
    let out = run_kubectl(&args).await?;
    let message = if out.stdout.trim().is_empty() {
        out.stderr.trim().to_string()
    } else {
        out.stdout.trim().to_string()
    };
    Ok(json!({
        "context": context,
        "namespace": namespace,
        "kind": kind,
        "name": name,
        "complete": out.success,
        "message": message,
    }))
}

/// `k8s_events`: the most recent `last` events by lastTimestamp (newest first).
pub async fn events(context: &str, namespace: &str, last: usize) -> Result<Value, String> {
    let context = validate_context(context).await?;
    let namespace = namespace_or_default(namespace);
    let last = if last == 0 { 30 } else { last };
    let mut args = scoped(&context, &namespace);
    args.extend(["get".into(), "events".into(), "-o".into(), "json".into()]);
    let json_out = run_json(&args).await?;

    let mut items: Vec<&Value> = json_out
        .get("items")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    // RFC3339 timestamps sort lexicographically == chronologically.
    items.sort_by_cached_key(|e| event_ts(e));
    let recent: Vec<Value> = items
        .iter()
        .rev()
        .take(last)
        .map(|e| summarize_event(e))
        .collect();

    Ok(cap_output(json!({
        "context": context,
        "namespace": namespace,
        "count": recent.len(),
        "events": recent,
    })))
}

/// `k8s_service_endpoints`: ready/not-ready backend addresses for a service.
pub async fn service_endpoints(
    context: &str,
    namespace: &str,
    service: &str,
) -> Result<Value, String> {
    let context = validate_context(context).await?;
    let service = service.trim();
    if service.is_empty() {
        return Err("`service` is required".to_string());
    }
    let namespace = namespace_or_default(namespace);
    let mut args = scoped(&context, &namespace);
    args.extend([
        "get".into(),
        "endpoints".into(),
        service.into(),
        "-o".into(),
        "json".into(),
    ]);
    let json_out = run_json(&args).await?;

    let mut ready = Vec::new();
    let mut not_ready = Vec::new();
    if let Some(subsets) = json_out.get("subsets").and_then(Value::as_array) {
        for subset in subsets {
            collect_addresses(subset.get("addresses"), &mut ready);
            collect_addresses(subset.get("notReadyAddresses"), &mut not_ready);
        }
    }
    Ok(json!({
        "context": context,
        "namespace": namespace,
        "service": service,
        "ready": ready,
        "notReady": not_ready,
    }))
}

// ------------------------------------------------------------------ summarizers

fn summarize_workloads(v: &Value) -> Vec<Value> {
    v.get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|it| {
                    let images: Vec<&str> = it
                        .pointer("/spec/template/spec/containers")
                        .and_then(Value::as_array)
                        .map(|cs| {
                            cs.iter()
                                .filter_map(|c| c.get("image").and_then(Value::as_str))
                                .collect()
                        })
                        .unwrap_or_default();
                    json!({
                        "kind": it.get("kind").and_then(Value::as_str).unwrap_or("?"),
                        "name": it.pointer("/metadata/name").and_then(Value::as_str).unwrap_or("?"),
                        "ready": it.pointer("/status/readyReplicas").and_then(Value::as_u64).unwrap_or(0),
                        "desired": it.pointer("/spec/replicas").and_then(Value::as_u64),
                        "images": images,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn summarize_pods(v: &Value) -> Vec<Value> {
    v.get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|p| {
                    let restarts: u64 = p
                        .pointer("/status/containerStatuses")
                        .and_then(Value::as_array)
                        .map(|cs| {
                            cs.iter()
                                .filter_map(|c| c.get("restartCount").and_then(Value::as_u64))
                                .sum()
                        })
                        .unwrap_or(0);
                    json!({
                        "name": p.pointer("/metadata/name").and_then(Value::as_str).unwrap_or("?"),
                        "phase": p.pointer("/status/phase").and_then(Value::as_str).unwrap_or("?"),
                        "restarts": restarts,
                        "node": p.pointer("/spec/nodeName").and_then(Value::as_str),
                        "created": p.pointer("/metadata/creationTimestamp").and_then(Value::as_str),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn event_ts(event: &Value) -> String {
    event
        .get("lastTimestamp")
        .and_then(Value::as_str)
        .or_else(|| event.get("eventTime").and_then(Value::as_str))
        .or_else(|| event.pointer("/metadata/creationTimestamp").and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

fn summarize_event(event: &Value) -> Value {
    let object = format!(
        "{}/{}",
        event
            .pointer("/involvedObject/kind")
            .and_then(Value::as_str)
            .unwrap_or("?"),
        event
            .pointer("/involvedObject/name")
            .and_then(Value::as_str)
            .unwrap_or("?"),
    );
    json!({
        "lastTimestamp": event_ts(event),
        "type": event.get("type").and_then(Value::as_str),
        "reason": event.get("reason").and_then(Value::as_str),
        "object": object,
        "count": event.get("count").and_then(Value::as_u64),
        "message": event.get("message").and_then(Value::as_str),
    })
}

fn collect_addresses(addresses: Option<&Value>, out: &mut Vec<Value>) {
    if let Some(arr) = addresses.and_then(Value::as_array) {
        for a in arr {
            out.push(json!({
                "ip": a.get("ip").and_then(Value::as_str),
                "target": a.pointer("/targetRef/name").and_then(Value::as_str),
                "node": a.get("nodeName").and_then(Value::as_str),
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Serializes tests that mutate `PATH` / `FIDUCIA_K8S_CONTEXTS` (process-global).
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    static TEMP_SEQ: AtomicUsize = AtomicUsize::new(0);

    /// Minimal std-only temp directory (removed on drop) — avoids a dev-dep.
    struct TempDir {
        path: PathBuf,
    }
    impl TempDir {
        fn new() -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "fiducia-mcp-kubectl-{}-{}",
                std::process::id(),
                TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
        fn path(&self) -> &Path {
            &self.path
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Installs a stub `kubectl` on `PATH` and restores `PATH` +
    /// `FIDUCIA_K8S_CONTEXTS` on drop. `script` is the body after `#!/bin/sh`.
    struct KubectlStub {
        _lock: std::sync::MutexGuard<'static, ()>,
        _dir: TempDir,
        prev_path: Option<String>,
        prev_contexts: Option<String>,
    }

    impl KubectlStub {
        fn install(script: &str) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = TempDir::new();
            let path = dir.path().join("kubectl");
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "#!/bin/sh").unwrap();
            f.write_all(script.as_bytes()).unwrap();
            drop(f);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            let prev_path = std::env::var("PATH").ok();
            let prev_contexts = std::env::var(K8S_CONTEXTS_ENV).ok();
            let new_path = match &prev_path {
                Some(p) => format!("{}:{}", dir.path().display(), p),
                None => dir.path().display().to_string(),
            };
            std::env::set_var("PATH", new_path);
            std::env::remove_var(K8S_CONTEXTS_ENV);
            Self { _lock: lock, _dir: dir, prev_path, prev_contexts }
        }

        fn restrict(&self, csv: &str) {
            std::env::set_var(K8S_CONTEXTS_ENV, csv);
        }
    }

    impl Drop for KubectlStub {
        fn drop(&mut self) {
            match &self.prev_path {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
            match &self.prev_contexts {
                Some(c) => std::env::set_var(K8S_CONTEXTS_ENV, c),
                None => std::env::remove_var(K8S_CONTEXTS_ENV),
            }
        }
    }

    /// A stub that answers `config get-contexts -o name` with two contexts and
    /// echoes recorded argv for anything else (so we can assert on it).
    const CONTEXTS_STUB: &str = r#"
if [ "$1" = "config" ] && [ "$2" = "get-contexts" ]; then
  printf 'gke_fiducia_prod\naws_fiducia_staging\n'
  exit 0
fi
echo "unexpected: $@" >&2
exit 1
"#;

    #[tokio::test]
    async fn contexts_lists_and_marks_allowed() {
        let stub = KubectlStub::install(CONTEXTS_STUB);
        stub.restrict("gke_fiducia_prod");
        let out = contexts().await.unwrap();
        let list = out["contexts"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        let prod = list.iter().find(|c| c["name"] == "gke_fiducia_prod").unwrap();
        let staging = list.iter().find(|c| c["name"] == "aws_fiducia_staging").unwrap();
        assert_eq!(prod["allowed"], true);
        assert_eq!(staging["allowed"], false);
    }

    #[tokio::test]
    async fn unknown_context_is_rejected() {
        let _stub = KubectlStub::install(CONTEXTS_STUB);
        let err = workloads("does_not_exist", "fiducia").await.unwrap_err();
        assert!(err.contains("unknown kubectl context"), "{err}");
    }

    #[tokio::test]
    async fn allowlist_blocks_valid_but_disallowed_context() {
        let stub = KubectlStub::install(CONTEXTS_STUB);
        // The context exists in kubectl, but is not in the allowlist.
        stub.restrict("gke_fiducia_prod");
        let err = workloads("aws_fiducia_staging", "fiducia").await.unwrap_err();
        assert!(err.contains(K8S_CONTEXTS_ENV), "{err}");
        assert!(err.contains("not permitted"), "{err}");
    }

    const WORKLOADS_STUB: &str = r#"
if [ "$1" = "config" ] && [ "$2" = "get-contexts" ]; then
  printf 'gke_fiducia_prod\n'
  exit 0
fi
# args include: --context gke_fiducia_prod -n fiducia get <kinds> -o json
for a in "$@"; do
  if [ "$a" = "deployments,statefulsets" ]; then
    echo '{"items":[{"kind":"Deployment","metadata":{"name":"fiducia-node"},"spec":{"replicas":3,"template":{"spec":{"containers":[{"image":"fiducia-node:1.2.3"}]}}},"status":{"readyReplicas":2}}]}'
    exit 0
  fi
  if [ "$a" = "pods" ]; then
    echo '{"items":[{"metadata":{"name":"fiducia-node-0","creationTimestamp":"2026-07-13T00:00:00Z"},"spec":{"nodeName":"node-a"},"status":{"phase":"Running","containerStatuses":[{"restartCount":4}]}}]}'
    exit 0
  fi
done
echo "unexpected: $@" >&2
exit 1
"#;

    #[tokio::test]
    async fn workloads_summarized_from_canned_json() {
        let _stub = KubectlStub::install(WORKLOADS_STUB);
        let out = workloads("gke_fiducia_prod", "").await.unwrap();
        assert_eq!(out["namespace"], "fiducia"); // default namespace
        let wl = &out["workloads"][0];
        assert_eq!(wl["kind"], "Deployment");
        assert_eq!(wl["name"], "fiducia-node");
        assert_eq!(wl["ready"], 2);
        assert_eq!(wl["desired"], 3);
        assert_eq!(wl["images"][0], "fiducia-node:1.2.3");
        let pod = &out["pods"][0];
        assert_eq!(pod["name"], "fiducia-node-0");
        assert_eq!(pod["phase"], "Running");
        assert_eq!(pod["restarts"], 4);
        assert_eq!(pod["node"], "node-a");
    }

    const SLOW_STUB: &str = r#"
if [ "$1" = "config" ] && [ "$2" = "get-contexts" ]; then
  printf 'gke_fiducia_prod\n'
  exit 0
fi
sleep 5
echo '{"items":[]}'
"#;

    #[tokio::test]
    async fn kubectl_call_times_out() {
        let _stub = KubectlStub::install(SLOW_STUB);
        // Drive the real timeout branch of run_kubectl_with with a short deadline
        // (the stub sleeps 5s). We get OUR error string, not a hang.
        let err = run_kubectl_with(&["get".into(), "pods".into()], Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(err.contains("kubectl get pods"), "names the command: {err}");
    }
}
