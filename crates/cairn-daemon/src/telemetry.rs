//! Tracing / OpenTelemetry initialisation.
//!
//! [`init_tracing`] builds a layered `tracing_subscriber::Registry`:
//!
//! * **Stderr fmt layer** — style controlled by [`LogFormat`].
//! * **OTLP layer** — activated when `OTEL_EXPORTER_OTLP_ENDPOINT` or
//!   `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` is set.
//!
//! The caller must hold the returned [`opentelemetry_sdk::trace::SdkTracerProvider`]
//! (if any) until shutdown so spans are flushed.

use crate::config::LogFormat;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Extract a remote OTel context from a [`CallContext`], if present.
///
/// Parses the `trace_context` field as a W3C `traceparent` header
/// (`00-<trace_id>-<span_id>-<flags>`) and returns an [`opentelemetry::Context`]
/// carrying the decoded `SpanContext`.  Returns `None` when the call context is
/// absent, the trace-context string is missing, or the traceparent is malformed.
pub fn extract_remote_context(
    ctx: &Option<cairn_protocol::cairn::daemon::types::CallContext>,
) -> Option<opentelemetry::Context> {
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState,
    };

    let traceparent = ctx.as_ref()?.trace_context.as_deref()?;
    let parts: Vec<&str> = traceparent.split('-').collect();
    if parts.len() != 4 {
        return None;
    }
    let trace_id = TraceId::from_hex(parts[1]).ok()?;
    let span_id = SpanId::from_hex(parts[2]).ok()?;
    let flags = u8::from_str_radix(parts[3], 16).ok()?;

    let sc = SpanContext::new(
        trace_id,
        span_id,
        TraceFlags::new(flags),
        true, // remote
        TraceState::NONE,
    );

    // Build an OTel Context with this remote span as the "current" span.
    let remote_ctx = opentelemetry::Context::new().with_remote_span_context(sc);
    Some(remote_ctx)
}

/// Add a span link from `span` to the remote parent encoded in `call_ctx`.
///
/// This is the daemon-side equivalent of cairn-pty's `add_trace_link`:
/// it parses the traceparent, builds a `SpanContext`, and attaches it as
/// a link so distributed-trace tooling can correlate client ↔ daemon spans.
pub fn link_remote_context(
    span: &tracing::Span,
    call_ctx: &Option<cairn_protocol::cairn::daemon::types::CallContext>,
) {
    use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState};
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let Some(traceparent) = call_ctx.as_ref().and_then(|c| c.trace_context.as_deref()) else {
        return;
    };

    let parts: Vec<&str> = traceparent.split('-').collect();
    if parts.len() == 4
        && let (Ok(trace_id), Ok(span_id), Ok(flags)) = (
            TraceId::from_hex(parts[1]),
            SpanId::from_hex(parts[2]),
            u8::from_str_radix(parts[3], 16),
        )
    {
        let sc = SpanContext::new(
            trace_id,
            span_id,
            TraceFlags::new(flags),
            true,
            TraceState::NONE,
        );
        span.add_link(sc);
    }
}

/// Concrete OTel layer type produced by [`build_otel_layer`].
type OtelLayer =
    tracing_opentelemetry::OpenTelemetryLayer<Registry, opentelemetry_sdk::trace::SdkTracer>;

/// Initialise the global `tracing` subscriber.
///
/// Returns `Some(SdkTracerProvider)` when an OTLP exporter was configured; the
/// caller must keep it alive until the process is ready to shut down (dropping
/// it flushes pending spans).
pub fn init_tracing(filter: &str, format: LogFormat) -> anyhow::Result<Option<SdkTracerProvider>> {
    let env_filter = EnvFilter::new(filter);

    let (provider, otel_layer) = build_otel_layer()?;

    // Collect all optional layers into a Vec so they compose at the Registry
    // level without nested Layered wrappers that break trait bounds.
    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = Vec::new();

    if let Some(otel) = otel_layer {
        layers.push(Box::new(otel));
    }

    // Build the fmt layer — each format variant produces a different concrete
    // type, so we box them to a common trait object.
    match format {
        LogFormat::Pretty => layers.push(Box::new(
            tracing_subscriber::fmt::layer()
                .pretty()
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_writer(std::io::stderr),
        )),
        LogFormat::Compact => layers.push(Box::new(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_writer(std::io::stderr),
        )),
        LogFormat::Json => layers.push(Box::new(
            tracing_subscriber::fmt::layer()
                .json()
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_writer(std::io::stderr),
        )),
        LogFormat::Full => layers.push(Box::new(
            tracing_subscriber::fmt::layer()
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_writer(std::io::stderr),
        )),
        LogFormat::Off => {}
    }

    // Vec<Box<dyn Layer<Registry>>> implements Layer<Registry>, so it goes on
    // first, then EnvFilter wraps the whole stack (it is generic over any
    // Subscriber).
    tracing_subscriber::registry()
        .with(layers)
        .with(env_filter)
        .init();

    Ok(provider)
}

/// If an OTLP endpoint env var is set, build a `TracerProvider` + tracing
/// layer.  Returns `(None, None)` when neither env var is present.
fn build_otel_layer() -> anyhow::Result<(Option<SdkTracerProvider>, Option<OtelLayer>)> {
    let has_endpoint = std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_some()
        || std::env::var_os("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").is_some();

    if !has_endpoint {
        return Ok((None, None));
    }

    // The OTLP exporter reads its own env vars (endpoint, headers, protocol)
    // so we only need defaults here.
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()?;

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .build();

    let tracer = provider.tracer("cairn-daemon");
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);

    Ok((Some(provider), Some(layer)))
}
