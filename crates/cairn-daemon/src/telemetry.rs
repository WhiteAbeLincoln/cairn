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
                .with_writer(std::io::stderr),
        )),
        LogFormat::Compact => layers.push(Box::new(
            tracing_subscriber::fmt::layer()
                .compact()
                .with_ansi(false)
                .with_writer(std::io::stderr),
        )),
        LogFormat::Json => layers.push(Box::new(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(std::io::stderr),
        )),
        LogFormat::Full => layers.push(Box::new(
            tracing_subscriber::fmt::layer().with_writer(std::io::stderr),
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
