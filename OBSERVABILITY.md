# Observability contract

This document defines the operational telemetry boundary for `fiducia-mcp`.

## Process and transport ownership

MCP owns stdout for JSON-RPC frames. Logs, exporter diagnostics, and shutdown warnings remain on stderr. `src/runtime.rs` owns startup-flag processing, telemetry initialization, environment-derived upstream configuration, server construction, lifecycle spans, and stdio service ownership. `src/main.rs` is only the Tokio bootstrap.

The only accepted process flag is `--log-filter`; `.cli-flags.toml` is audited before telemetry or MCP startup. Upstream URLs, tenant identifiers, mutation gates, Kubernetes configuration, exporter settings, and credentials remain environment-only. Startup diagnostics report only whether sensitive configuration is present, never its value.

Tool implementations are instrumented through the generated router. Runtime or library APIs are not monkey-patched.

## OpenTelemetry activation

OTLP trace and metric export is opt-in through `OTEL_EXPORTER_OTLP_ENDPOINT`. The endpoint must:

- be no more than 2 KiB;
- use `http` or `https`;
- include a host;
- contain no embedded username or password;
- contain no query string or fragment; and
- contain no control characters.

Invalid endpoints fail open to stderr-only telemetry. Endpoint values and exporter errors are not logged because collector configuration can be sensitive. Collector authentication belongs in the standard OpenTelemetry header environment variables, never in the endpoint URL.

## Resource identity

The process owns these resource attributes and does not permit `OTEL_RESOURCE_ATTRIBUTES` to override them:

- `service.name=fiducia-mcp`
- `service.namespace=fiducia-cloud`
- `service.version=<crate version>`
- `deployment.environment`, when `DEPLOYMENT_ENV` is valid
- `k8s.namespace.name`, when `POD_NAMESPACE` is valid
- `k8s.pod.name`, when `POD_NAME` is valid
- `k8s.node.name`, when `NODE_NAME` is valid
- `host.name`, when `HOSTNAME` is valid

Additional `OTEL_RESOURCE_ATTRIBUTES` use a bounded contract:

- at most 16 KiB raw input;
- at most 32 accepted attributes;
- keys are 1–128 ASCII alphanumeric, `.`, `_`, or `-` characters;
- values are 1–256 characters and contain no control characters;
- the first valid duplicate key wins; and
- keys suggesting credentials, tokens, sessions, email addresses, cookies, passwords, private/signing keys, or authorization data are discarded.

## MCP spans and metrics

Every tool call is wrapped explicitly at the generated tool-router boundary.

Process span:

- name: `mcp.server`
- `rpc.system=mcp`
- `transport=stdio`

Tool span:

- name: `mcp.tool.call`
- `rpc.system=mcp`
- `rpc.method=tools/call`
- `mcp.tool.name=<registered tool name>`
- `mcp.tool.error=<boolean>`
- `otel.status_code=OK|ERROR`

Metrics:

- `mcp.server.tool.calls`, counter, unit `{call}`
- `mcp.server.tool.duration`, histogram, unit `ms`

Only the registered tool name and error state are recorded. Tool arguments, response bodies, lock or KV contents, local paths supplied by callers, bearer values, Fiducia API keys, internal/control-plane secrets, organization identifiers, Cloudflare tokens, and Kubernetes credentials are never attached to spans or metrics.

## Backend compatibility

OTLP data can be sent through an OpenTelemetry Collector to Tempo or another trace backend and to a Prometheus-compatible metrics backend. Structured stderr JSON can be collected by the Kubernetes logging agent and forwarded to Loki. Grafana can correlate these streams through the process-owned service attributes above.

## Validation and shutdown

Primary CI denies formatting and Clippy warnings, builds and tests the locked release tree with the pinned sibling client, exercises the complete MCP lifecycle in the non-root container, enforces the runtime/telemetry architecture, and runs the dependency audit.

The telemetry guard owns trace and metric providers and attempts to flush both during process shutdown. Flush failures are non-fatal and emit only a generic stderr warning.
