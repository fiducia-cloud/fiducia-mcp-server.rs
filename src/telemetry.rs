//! Explicit, stdio-safe OpenTelemetry for the MCP server.
//!
//! MCP owns stdout, so the fallback tracing layer always writes structured
//! logs to stderr. OTLP traces and metrics are enabled only when
//! `OTEL_EXPORTER_OTLP_ENDPOINT` is configured. No runtime or library APIs are
//! patched; tool routes are wrapped explicitly when the server is built.

use std::{sync::Arc, time::Instant};

use opentelemetry::{global, KeyValue};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    metrics::{PeriodicReader, SdkMeterProvider},
    runtime,
    trace::{Tracer, TracerProvider},
    Resource,
};
use rmcp::{handler::server::tool::ToolRouter, service::MaybeSend};
use tracing::{field, Instrument};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

const EXPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Owns the SDK providers so their final batches can be flushed on shutdown.
pub struct TelemetryGuard {
    tracer_provider: Option<TracerProvider>,
    meter_provider: Option<SdkMeterProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        let tracer_provider = self.tracer_provider.take();
        let meter_provider = self.meter_provider.take();
        if tracer_provider.is_none() && meter_provider.is_none() {
            return;
        }

        if std::thread::spawn(move || {
            if let Some(provider) = meter_provider {
                let _ = provider.shutdown();
            }
            if let Some(provider) = tracer_provider {
                let _ = provider.shutdown();
            }
        })
        .join()
        .is_err()
        {
            eprintln!("telemetry: shutdown flush panicked; final batches may be incomplete");
        }
    }
}

/// Install structured stderr logs and optional OTLP trace/metric exporters.
///
/// Exporter failures fail open to stderr-only telemetry. Error details are not
/// printed because an OTLP endpoint or header can contain credentials.
pub fn init(service_name: &'static str, service_namespace: &'static str) -> TelemetryGuard {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,hyper=warn"));
    let resource = resource(service_name, service_namespace);
    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .filter(|value| !value.trim().is_empty());

    let (tracer_provider, tracer) = endpoint
        .as_deref()
        .and_then(|endpoint| build_tracer_provider(endpoint, resource.clone()).ok())
        .map_or((None, None), |(provider, tracer)| {
            global::set_tracer_provider(provider.clone());
            (Some(provider), Some(tracer))
        });

    let meter_provider = endpoint
        .as_deref()
        .and_then(|endpoint| build_meter_provider(endpoint, resource).ok());
    if let Some(provider) = meter_provider.as_ref() {
        global::set_meter_provider(provider.clone());
    }

    install_subscriber(filter, tracer);
    tracing::info!(
        service.name = service_name,
        service.namespace = service_namespace,
        otel.trace_exporter = tracer_provider.is_some(),
        otel.metric_exporter = meter_provider.is_some(),
        log.stream = "stderr",
        "MCP telemetry initialized"
    );

    TelemetryGuard {
        tracer_provider,
        meter_provider,
    }
}

fn build_tracer_provider(
    endpoint: &str,
    resource: Resource,
) -> Result<(TracerProvider, Tracer), ()> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|_| ())?;
    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, runtime::Tokio)
        .with_resource(resource)
        .build();
    use opentelemetry::trace::TracerProvider as _;
    let tracer = provider.tracer("mcp-server");
    Ok((provider, tracer))
}

fn build_meter_provider(endpoint: &str, resource: Resource) -> Result<SdkMeterProvider, ()> {
    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .with_timeout(EXPORT_TIMEOUT)
        .build()
        .map_err(|_| ())?;
    let reader = PeriodicReader::builder(exporter, runtime::Tokio).build();
    Ok(SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(resource)
        .build())
}

fn install_subscriber(filter: EnvFilter, tracer: Option<Tracer>) {
    let result = match tracer {
        Some(tracer) => tracing_subscriber::registry()
            .with(filter)
            .with(stderr_json_layer())
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .try_init(),
        None => tracing_subscriber::registry()
            .with(filter)
            .with(stderr_json_layer())
            .try_init(),
    };
    if result.is_err() {
        eprintln!("telemetry: subscriber already initialized; keeping existing subscriber");
    }
}

fn stderr_json_layer<S>() -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    tracing_subscriber::fmt::layer()
        .json()
        .flatten_event(true)
        .with_ansi(false)
        .with_current_span(true)
        .with_span_list(true)
        .with_target(true)
        .with_writer(std::io::stderr)
}

fn resource(service_name: &str, service_namespace: &str) -> Resource {
    let mut attributes = vec![
        KeyValue::new("service.name", service_name.to_string()),
        KeyValue::new("service.namespace", service_namespace.to_string()),
        KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
    ];
    push_env_attribute(&mut attributes, "DEPLOYMENT_ENV", "deployment.environment");
    push_env_attribute(&mut attributes, "POD_NAMESPACE", "k8s.namespace.name");
    push_env_attribute(&mut attributes, "POD_NAME", "k8s.pod.name");
    push_env_attribute(&mut attributes, "NODE_NAME", "k8s.node.name");
    push_env_attribute(&mut attributes, "HOSTNAME", "host.name");

    if let Ok(raw) = std::env::var("OTEL_RESOURCE_ATTRIBUTES") {
        attributes
            .extend(resource_attribute_pairs(&raw).map(|(key, value)| KeyValue::new(key, value)));
    }
    Resource::new(attributes)
}

fn push_env_attribute(attributes: &mut Vec<KeyValue>, env_name: &str, key: &'static str) {
    if let Ok(value) = std::env::var(env_name) {
        let value = value.trim();
        if valid_attribute_value(value) {
            attributes.push(KeyValue::new(key, value.to_string()));
        }
    }
}

fn resource_attribute_pairs(raw: &str) -> impl Iterator<Item = (String, String)> + '_ {
    raw.split(',').filter_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        let key = key.trim();
        let value = value.trim();
        if valid_attribute_key(key)
            && valid_attribute_value(value)
            && !sensitive_attribute_key(key)
            && !matches!(key, "service.name" | "service.namespace")
        {
            Some((key.to_string(), value.to_string()))
        } else {
            None
        }
    })
}

fn valid_attribute_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_attribute_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn sensitive_attribute_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['-', '.'], "_");
    [
        "authorization",
        "bearer",
        "cookie",
        "credential",
        "email",
        "jwt",
        "passphrase",
        "passwd",
        "password",
        "private_key",
        "pwd",
        "secret",
        "session",
        "signing_key",
        "token",
        "api_key",
        "apikey",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

/// Add one explicit span and two low-cardinality metrics around every tool.
/// Tool arguments and result bodies are deliberately never recorded.
pub fn instrument_tool_router<S>(mut router: ToolRouter<S>) -> ToolRouter<S>
where
    S: MaybeSend + 'static,
{
    let meter = global::meter("mcp-server");
    let calls = meter
        .u64_counter("mcp.server.tool.calls")
        .with_description("Number of MCP tool calls completed")
        .with_unit("{call}")
        .build();
    let duration = meter
        .f64_histogram("mcp.server.tool.duration")
        .with_description("MCP tool call duration")
        .with_unit("ms")
        .build();

    for route in router.map.values_mut() {
        let original = Arc::clone(&route.call);
        let tool_name = route.attr.name.clone();
        let calls = calls.clone();
        let duration = duration.clone();
        route.call = Arc::new(move |context| {
            let original = Arc::clone(&original);
            let tool_name = tool_name.clone();
            let calls = calls.clone();
            let duration = duration.clone();
            Box::pin(async move {
                let started = Instant::now();
                let span = tracing::info_span!(
                    "mcp.tool.call",
                    rpc.system = "mcp",
                    rpc.method = "tools/call",
                    mcp.tool.name = %tool_name,
                    otel.status_code = field::Empty,
                    mcp.tool.error = field::Empty,
                );
                async move {
                    let result = (original)(context).await;
                    let is_error = result
                        .as_ref()
                        .map_or(true, |output| output.is_error.unwrap_or(false));
                    let elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0;
                    let attributes = [
                        KeyValue::new("mcp.tool.name", tool_name.to_string()),
                        KeyValue::new("mcp.tool.error", is_error),
                    ];
                    calls.add(1, &attributes);
                    duration.record(elapsed_ms, &attributes);
                    tracing::Span::current()
                        .record("otel.status_code", if is_error { "ERROR" } else { "OK" });
                    tracing::Span::current().record("mcp.tool.error", is_error);
                    result
                }
                .instrument(span)
                .await
            })
        });
    }
    router
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_attributes_reject_secrets_and_identity_overrides() {
        let attributes = resource_attribute_pairs(
            "team=simulation,api.token=nope,service.name=spoof,cloud.region=us-east-1",
        )
        .collect::<Vec<_>>();
        assert_eq!(
            attributes,
            vec![
                ("team".to_string(), "simulation".to_string()),
                ("cloud.region".to_string(), "us-east-1".to_string()),
            ]
        );
    }

    #[test]
    fn resource_attributes_reject_controls_and_oversized_values() {
        let long = "x".repeat(257);
        let raw = format!("good=value,bad=line\nfeed,long={long}");
        assert_eq!(
            resource_attribute_pairs(&raw).collect::<Vec<_>>(),
            vec![("good".to_string(), "value".to_string())]
        );
    }
}
