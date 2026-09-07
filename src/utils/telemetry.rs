//! OTLP trace export (lore.md §1.7) — compiled ONLY with `--features otlp`;
//! the default binary contains none of this module.
//!
//! PRIVACY ALLOWLIST (lore.md §7.9 "OTLP telemetry span（1.7）" row, `:787`
//! as of 2026-08-27; enforced STRUCTURALLY): only the vetted
//! `libra::telemetry` target is exported — a per-layer `Targets` filter means
//! no other span or event in the codebase can leak, whatever it carries. The
//! one exported span holds the canonical subcommand name, the duration, and
//! on failure the stable `LBR-*` code. Resource attributes are exactly
//! `service.name` + `service.version` (built from an EMPTY resource builder —
//! the default builder would honor `OTEL_RESOURCE_ATTRIBUTES` and violate the
//! allowlist). No remote URLs, tokens, paths, refs, or identities — ever.
//!
//! GATING: the layer is installed only when the crate is compiled with the
//! feature AND `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (or the generic
//! `OTEL_EXPORTER_OTLP_ENDPOINT`) is set AND `OTEL_SDK_DISABLED` is not
//! `true`. There is NO default endpoint: off means nothing leaves the
//! machine. Endpoints must be https (http allowed for loopback only — local
//! collectors).
//!
//! LIFECYCLE: the exporter uses http-proto over BLOCKING reqwest (no tonic —
//! init and shutdown run on the runtime-free main thread; the tokio runtime
//! lives and dies inside the CLI worker thread). `shutdown()` flushes with
//! the SDK's bounded timeout; failures warn and never affect the command's
//! exit code. KNOWN LIMIT: plumbing commands that terminate via
//! `std::process::exit` inside dispatch skip the flush — their spans are
//! lost (documented under lore.md §1.7).

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::{Registry, filter::Targets, layer::Layer};

/// The only target the OTLP layer exports.
pub const TELEMETRY_TARGET: &str = "libra::telemetry";

static PROVIDER: std::sync::Mutex<Option<SdkTracerProvider>> = std::sync::Mutex::new(None);

/// Resolve the export endpoint from the standard OTel env vars. `None` =
/// telemetry stays off (no default endpoint, ever).
pub fn resolve_endpoint() -> Option<String> {
    if std::env::var("OTEL_SDK_DISABLED")
        .map(|value| value.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return None;
    }
    let raw = std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .or_else(|_| std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT"))
        .ok()?;
    let raw = raw.trim().to_string();
    if raw.is_empty() {
        return None;
    }
    // https required; http tolerated for loopback collectors only.
    match url::Url::parse(&raw) {
        Ok(url) if url.scheme() == "https" => Some(raw),
        Ok(url)
            if url.scheme() == "http"
                && url.host_str().is_some_and(|host| {
                    host == "localhost"
                        || host
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_loopback())
                }) =>
        {
            Some(raw)
        }
        _ => {
            eprintln!(
                "warning: ignoring OTLP endpoint (must be https, or http to loopback); \
                 telemetry disabled"
            );
            None
        }
    }
}

/// Build the OTLP layer when the endpoint gate passes. Failures warn and
/// return `None` — telemetry must never break a command.
pub fn try_build_layer() -> Option<Box<dyn Layer<Registry> + Send + Sync>> {
    let endpoint = resolve_endpoint()?;
    use opentelemetry_otlp::WithExportConfig;
    let exporter = match opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
    {
        Ok(exporter) => exporter,
        Err(error) => {
            eprintln!("warning: failed to build the OTLP exporter; telemetry disabled: {error}");
            return None;
        }
    };
    // EMPTY builder + explicit attributes only (the default builder honors
    // OTEL_RESOURCE_ATTRIBUTES / OTEL_SERVICE_NAME — allowlist violation).
    let resource = opentelemetry_sdk::Resource::builder_empty()
        .with_attributes([
            opentelemetry::KeyValue::new("service.name", "libra"),
            opentelemetry::KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
        ])
        .build();
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build();
    let tracer = provider.tracer("libra");
    *PROVIDER.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(provider);
    let layer = tracing_opentelemetry::layer()
        .with_tracer(tracer)
        // Code-location/thread attributes are off — allowlist only.
        .with_location(false)
        .with_threads(false)
        .with_tracked_inactivity(false)
        .with_filter(Targets::new().with_target(TELEMETRY_TARGET, tracing::Level::INFO));
    Some(Box::new(layer))
}

/// Flush and shut the provider down (bounded by the SDK's export timeout).
/// Warn-only: a dead collector must never change a command's outcome.
pub fn shutdown() {
    let provider = PROVIDER
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take();
    if let Some(provider) = provider
        && let Err(error) = provider.shutdown()
    {
        eprintln!("warning: OTLP telemetry flush failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env-var tests mutate process state — keep them in ONE test so they
    // cannot race each other (cargo test runs tests in parallel threads).
    #[test]
    fn endpoint_gating_matrix() {
        let clear = || {
            unsafe {
                std::env::remove_var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT");
                std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
                std::env::remove_var("OTEL_SDK_DISABLED");
            };
        };
        clear();
        assert!(resolve_endpoint().is_none(), "unset => off");
        unsafe { std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", "https://collector.example") };
        assert_eq!(
            resolve_endpoint().as_deref(),
            Some("https://collector.example")
        );
        // Traces-specific endpoint wins.
        unsafe {
            std::env::set_var(
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                "https://traces.example",
            )
        };
        assert_eq!(
            resolve_endpoint().as_deref(),
            Some("https://traces.example")
        );
        // Kill switch.
        unsafe { std::env::set_var("OTEL_SDK_DISABLED", "true") };
        assert!(resolve_endpoint().is_none(), "OTEL_SDK_DISABLED wins");
        unsafe { std::env::remove_var("OTEL_SDK_DISABLED") };
        // Loopback http allowed; non-loopback http refused.
        unsafe {
            std::env::set_var(
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                "http://127.0.0.1:4318",
            )
        };
        assert!(resolve_endpoint().is_some(), "loopback http ok");
        unsafe {
            std::env::set_var(
                "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
                "http://collector.example",
            )
        };
        assert!(
            resolve_endpoint().is_none(),
            "cleartext non-loopback refused"
        );
        clear();
    }
}
