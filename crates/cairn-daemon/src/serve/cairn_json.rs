//! `/cairn.json` — the server-defined client config document. Served
//! alongside the SPA and on every `ws://` listener regardless of `--web-ui`
//! (see the design spec's "/cairn.json" section for the wire contract).
//!
//! This module holds the pure data/rendering logic; the axum handler that
//! wires it to a request lives in `serve::http`.

use axum::http::HeaderMap;
use serde_json::{Map, Value, json};

/// WebTransport listener facts needed to render `/cairn.json`, captured
/// right after bind so a configured port of `0` still reports the real
/// bound port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WtInfo {
    pub(crate) port: u16,
    /// `None` when the listener uses a user-supplied cert (`--wt-cert`/
    /// `--wt-key`) — only the self-signed cert path needs client-side
    /// pinning via a published hash.
    pub(crate) cert_hash: Option<String>,
}

/// Cross-listener facts assembled once at startup, after every `--listen`
/// listener is bound, and shared by every HTTP surface that serves
/// `/cairn.json`: every `ws://` listener, plus the dedicated
/// `--web-ui=host:port` listener if configured.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CairnJsonInfo {
    /// Bound port of the first configured `ws://` listener, if any. Used to
    /// build an absolute `ws://` URL when rendering for the dedicated UI
    /// listener, which has no `/ws` of its own. When more than one `ws://`
    /// listener is configured (e.g. distinct, non-dual-stack addresses),
    /// the first one wins — an edge case the design spec doesn't cover.
    pub(crate) ws_port: Option<u16>,
    /// Same "first wins" rule as `ws_port` for multiple heterogeneous
    /// WebTransport listeners (dual-stack `localhost` expansion shares one
    /// port across two addresses, so that common case is unaffected).
    pub(crate) wt: Option<WtInfo>,
}

/// Render the `/cairn.json` document.
///
/// `is_ws_listener` is `true` when rendering for a `ws://` listener's own
/// HTTP server, whose `/ws` route is same-origin (relative path `"/ws"`);
/// `false` for the dedicated `--web-ui=host:port` listener, which has no
/// `/ws` of its own and instead reports an absolute URL to a configured
/// `ws://` listener if one exists, or omits `websocket` entirely if not.
///
/// `host` is the caller-resolved request host (see `request_host` below),
/// used to build absolute URLs for the WebTransport endpoint and (on the
/// dedicated listener) the WebSocket endpoint.
pub(crate) fn render(info: &CairnJsonInfo, is_ws_listener: bool, host: &str) -> Value {
    let mut endpoints = Map::new();

    if is_ws_listener {
        endpoints.insert("websocket".to_string(), json!("/ws"));
    } else if let Some(port) = info.ws_port {
        endpoints.insert(
            "websocket".to_string(),
            json!(format!("ws://{host}:{port}/ws")),
        );
    }

    if let Some(wt) = &info.wt {
        let mut wt_doc = Map::new();
        wt_doc.insert(
            "url".to_string(),
            json!(format!("https://{host}:{}", wt.port)),
        );
        if let Some(hash) = &wt.cert_hash {
            wt_doc.insert("certHash".to_string(), json!(hash));
        }
        endpoints.insert("webtransport".to_string(), Value::Object(wt_doc));
    }

    json!({ "endpoints": Value::Object(endpoints) })
}

/// Extract the request's `Host` header, stripped of any port suffix (an
/// IPv6 literal keeps its brackets, e.g. `[::1]:8080` -> `[::1]`, so it
/// remains a valid URL host when re-combined with a different port). Falls
/// back to `"localhost"` when the header is absent or unparseable — should
/// not happen for HTTP/1.1 browser requests, but keeps this infallible.
pub(crate) fn request_host(headers: &HeaderMap) -> String {
    let raw = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    strip_port(raw).to_string()
}

fn strip_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        // IPv6 literal, e.g. "[::1]:8080" or "[::1]".
        if let Some(end) = rest.find(']') {
            return &host[..end + 2];
        }
        return host;
    }
    host.rsplit_once(':').map_or(host, |(h, _port)| h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_only_renders_relative_websocket_only() {
        let info = CairnJsonInfo {
            ws_port: Some(8080),
            wt: None,
        };
        let doc = render(&info, true, "127.0.0.1");
        assert_eq!(doc, json!({"endpoints": {"websocket": "/ws"}}));
    }

    #[test]
    fn ws_and_wt_render_both_relative_and_absolute() {
        let info = CairnJsonInfo {
            ws_port: Some(8080),
            wt: Some(WtInfo {
                port: 4433,
                cert_hash: Some("abcd".to_string()),
            }),
        };
        let doc = render(&info, true, "127.0.0.1");
        assert_eq!(
            doc,
            json!({
                "endpoints": {
                    "websocket": "/ws",
                    "webtransport": {"url": "https://127.0.0.1:4433", "certHash": "abcd"}
                }
            })
        );
    }

    #[test]
    fn user_supplied_cert_omits_hash() {
        let info = CairnJsonInfo {
            ws_port: None,
            wt: Some(WtInfo {
                port: 4433,
                cert_hash: None,
            }),
        };
        // `is_ws_listener: false` here so the only thing under test is the
        // WT hash-omission rule — `is_ws_listener: true` would also add a
        // "/ws" entry regardless of `ws_port`, which isn't what this test is
        // about (see `dedicated_listener_*` tests for that axis).
        let doc = render(&info, false, "example.com");
        assert_eq!(
            doc,
            json!({"endpoints": {"webtransport": {"url": "https://example.com:4433"}}})
        );
    }

    #[test]
    fn dedicated_listener_reports_absolute_ws_url() {
        let info = CairnJsonInfo {
            ws_port: Some(8080),
            wt: None,
        };
        let doc = render(&info, false, "192.168.1.5");
        assert_eq!(
            doc,
            json!({"endpoints": {"websocket": "ws://192.168.1.5:8080/ws"}})
        );
    }

    #[test]
    fn dedicated_listener_omits_websocket_when_none_configured() {
        let info = CairnJsonInfo::default();
        let doc = render(&info, false, "192.168.1.5");
        assert_eq!(doc, json!({"endpoints": {}}));
    }

    #[test]
    fn strip_port_handles_ipv4_ipv6_and_hostnames() {
        assert_eq!(strip_port("127.0.0.1:8080"), "127.0.0.1");
        assert_eq!(strip_port("[::1]:8080"), "[::1]");
        assert_eq!(strip_port("[::1]"), "[::1]");
        assert_eq!(strip_port("example.com"), "example.com");
    }

    #[test]
    fn request_host_falls_back_when_header_absent() {
        assert_eq!(request_host(&HeaderMap::new()), "localhost");
    }
}
