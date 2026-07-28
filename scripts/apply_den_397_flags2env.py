#!/usr/bin/env python3
from pathlib import Path

root = Path(__file__).resolve().parents[1]
telemetry = root / "src" / "telemetry.rs"
text = telemetry.read_text(encoding="utf-8")
old = '''pub fn init(service_name: &'static str, service_namespace: &'static str) -> TelemetryGuard {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,hyper=warn"));
    let resource = resource(service_name, service_namespace);
'''
new = '''pub fn init(
    service_name: &'static str,
    service_namespace: &'static str,
    filter: EnvFilter,
) -> TelemetryGuard {
    let resource = resource(service_name, service_namespace);
'''
if text.count(old) != 1:
    raise SystemExit("telemetry init marker mismatch")
telemetry.write_text(text.replace(old, new, 1), encoding="utf-8")

readme = root / "README.md"
text = readme.read_text(encoding="utf-8")
old = '''Binary: `fiducia-mcp` — speaks MCP over **stdio** (stdout is the wire; all
logs go to stderr).
'''
new = '''Binary: `fiducia-mcp` — speaks MCP over **stdio** (stdout is the wire; all
logs go to stderr).

The binary audits `.cli-flags.toml` before telemetry or MCP startup. The only
accepted process flag is `--log-filter`; upstream URLs, tenant identifiers,
mutation gates, Kubernetes configuration, exporter settings, and every
credential remain environment-only. Installed binaries discover the contract
from the current directory, executable directory, or
`../share/fiducia-mcp-server`; set `FIDUCIA_FLAGS_CONFIG` for an explicit path.
'''
if text.count(old) != 1:
    raise SystemExit("README binary marker mismatch")
readme.write_text(text.replace(old, new, 1), encoding="utf-8")
