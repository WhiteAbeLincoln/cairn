//! wRPC adapters for the reusable HTTP proxy backend.

use std::sync::Arc;

use cairn_http_proxy as backend;
use cairn_protocol::cairn::daemon::types::Error as WireError;
use cairn_protocol::exports::cairn::daemon::http_proxy as wire;
use futures::StreamExt as _;
use futures::stream::{self, BoxStream};

use crate::daemon::Daemon;

pub fn intercept(
    daemon: &Daemon,
    id: String,
    actions: BoxStream<'static, Vec<wire::InterceptorAction>>,
) -> BoxStream<'static, Vec<wire::InterceptorEvent>> {
    let Some(entry) = daemon.registry.resolve(&id) else {
        return one_interceptor_error("session.not_found", "no such session");
    };
    let Some(proxy) = entry.proxy() else {
        return one_interceptor_error(
            "session.proxy_disabled",
            "HTTP proxying is not enabled for this session",
        );
    };
    let attachment = match proxy.attach_interceptor() {
        Ok(attachment) => attachment,
        Err(error) => {
            return one_interceptor_error("proxy.interceptor_attached", &error.to_string());
        }
    };

    run_intercept(proxy, attachment, actions)
}

/// Drive the bidirectional interceptor stream: forward `InterceptorEvent`s to
/// the client while applying the client's `InterceptorAction`s.
///
/// Applying an action can block on the exchange's bounded `pending` channel
/// (legitimate backpressure when a synthetic-response consumer is slow). That
/// backpressure must never head-of-line-block event delivery for *other*
/// exchanges, so action application runs as its own pinned future that the
/// select loop polls concurrently with `attachment.recv()`. A stalled
/// `apply_action` therefore leaves the loop free to keep delivering events.
fn run_intercept(
    proxy: Arc<backend::ProxySession>,
    mut attachment: backend::InterceptorAttachment,
    mut actions: BoxStream<'static, Vec<wire::InterceptorAction>>,
) -> BoxStream<'static, Vec<wire::InterceptorEvent>> {
    Box::pin(async_stream::stream! {
        // Errors produced while applying actions are surfaced to the client
        // through this channel so the action pump never needs to `yield` itself.
        let (errors_tx, mut errors_rx) = tokio::sync::mpsc::channel::<WireError>(32);
        let pump = async move {
            while let Some(batch) = actions.next().await {
                for action in batch {
                    let action = match interceptor_action_from_wire(action) {
                        Ok(action) => action,
                        Err(error) => {
                            if errors_tx.send(error).await.is_err() {
                                return;
                            }
                            continue;
                        }
                    };
                    if let Err(error) = proxy.apply_action(action).await {
                        let error = wire_error("proxy.invalid_action", error.to_string());
                        if errors_tx.send(error).await.is_err() {
                            return;
                        }
                    }
                }
            }
        };
        let mut pump = std::pin::pin!(pump);
        loop {
            tokio::select! {
                event = attachment.recv() => match event {
                    Some(event) => yield vec![interceptor_event_to_wire(event)],
                    None => break,
                },
                Some(error) = errors_rx.recv() => yield vec![wire::InterceptorEvent::Error(error)],
                // The action pump completes only when the client closes its
                // action stream; when it does, end interception. The loop breaks
                // here, so the completed future is never polled again.
                () = &mut pump => break,
            }
        }
    })
}

pub fn watch(daemon: &Daemon, id: String) -> BoxStream<'static, Vec<wire::ObservationEvent>> {
    let Some(entry) = daemon.registry.resolve(&id) else {
        return Box::pin(stream::once(async {
            vec![wire::ObservationEvent::Error(wire_error(
                "session.not_found",
                "no such session",
            ))]
        }));
    };
    let Some(proxy) = entry.proxy() else {
        return Box::pin(stream::once(async {
            vec![wire::ObservationEvent::Error(wire_error(
                "session.proxy_disabled",
                "HTTP proxying is not enabled for this session",
            ))]
        }));
    };
    let (snapshot, mut receiver) = proxy.subscribe_observations();

    Box::pin(async_stream::stream! {
        yield vec![wire::ObservationEvent::Snapshot(
            snapshot.into_iter().map(observed_exchange_to_wire).collect(),
        )];
        loop {
            match receiver.recv().await {
                Ok(event) => yield vec![observation_event_to_wire(event)],
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let (snapshot, replacement) = proxy.subscribe_observations();
                    receiver = replacement;
                    yield vec![wire::ObservationEvent::Snapshot(
                        snapshot.into_iter().map(observed_exchange_to_wire).collect(),
                    )];
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

fn one_interceptor_error(
    code: &str,
    message: &str,
) -> BoxStream<'static, Vec<wire::InterceptorEvent>> {
    let event = wire::InterceptorEvent::Error(wire_error(code, message));
    Box::pin(stream::once(async move { vec![event] }))
}

fn interceptor_action_from_wire(
    action: wire::InterceptorAction,
) -> Result<backend::InterceptorAction, WireError> {
    match action {
        wire::InterceptorAction::Forward(id) => Ok(backend::InterceptorAction::Forward(id)),
        wire::InterceptorAction::ResponseStart(start) => Ok(
            backend::InterceptorAction::ResponseStart(backend::ResponseStart {
                id: start.id,
                head: response_head_from_wire(start.head),
            }),
        ),
        wire::InterceptorAction::ResponseBody(chunk) => Ok(
            backend::InterceptorAction::ResponseBody(backend::BodyChunk {
                id: chunk.id,
                bytes: chunk.bytes,
            }),
        ),
        wire::InterceptorAction::ResponseEnd(id) => Ok(backend::InterceptorAction::ResponseEnd(id)),
        wire::InterceptorAction::Fail((id, error)) => Ok(backend::InterceptorAction::Fail(
            id,
            backend::ProxyFailure {
                code: error.code,
                message: error.message,
            },
        )),
    }
}

fn interceptor_event_to_wire(event: backend::InterceptorEvent) -> wire::InterceptorEvent {
    match event {
        backend::InterceptorEvent::Request(request) => {
            wire::InterceptorEvent::Request(wire::InterceptedRequest {
                id: request.id,
                head: request_head_to_wire(request.head),
                body: request.body,
            })
        }
        backend::InterceptorEvent::Cancelled(id) => wire::InterceptorEvent::Cancelled(id),
    }
}

fn observation_event_to_wire(event: backend::ObservationEvent) -> wire::ObservationEvent {
    match event {
        backend::ObservationEvent::Snapshot(exchanges) => wire::ObservationEvent::Snapshot(
            exchanges
                .into_iter()
                .map(observed_exchange_to_wire)
                .collect(),
        ),
        backend::ObservationEvent::RequestStart(start) => {
            wire::ObservationEvent::RequestStart(wire::RequestStarted {
                id: start.id,
                head: request_head_to_wire(start.head),
                unix_ms: start.unix_ms,
            })
        }
        backend::ObservationEvent::RequestBody(chunk) => {
            wire::ObservationEvent::RequestBody(body_chunk_to_wire(chunk))
        }
        backend::ObservationEvent::RequestEnd(end) => {
            wire::ObservationEvent::RequestEnd(body_ended_to_wire(end))
        }
        backend::ObservationEvent::ResponseStart(start) => {
            wire::ObservationEvent::ResponseStart(wire::ResponseStarted {
                id: start.id,
                head: response_head_to_wire(start.head),
            })
        }
        backend::ObservationEvent::ResponseBody(chunk) => {
            wire::ObservationEvent::ResponseBody(body_chunk_to_wire(chunk))
        }
        backend::ObservationEvent::ResponseEnd(end) => {
            wire::ObservationEvent::ResponseEnd(body_ended_to_wire(end))
        }
        backend::ObservationEvent::Completed(id, unix_ms) => {
            wire::ObservationEvent::Completed((id, unix_ms))
        }
        backend::ObservationEvent::Failed(failed) => {
            wire::ObservationEvent::Failed(wire::ExchangeFailed {
                id: failed.id,
                error: proxy_failure_to_wire(failed.failure),
            })
        }
    }
}

fn observed_exchange_to_wire(exchange: backend::ObservedExchange) -> wire::ObservedExchange {
    wire::ObservedExchange {
        id: exchange.id,
        request: request_head_to_wire(exchange.request),
        request_body: observed_body_to_wire(exchange.request_body),
        response: exchange.response.map(response_head_to_wire),
        response_body: observed_body_to_wire(exchange.response_body),
        started_at_unix_ms: exchange.started_at_unix_ms,
        completed_at_unix_ms: exchange.completed_at_unix_ms,
        failure: exchange.failure.map(proxy_failure_to_wire),
    }
}

fn request_head_to_wire(head: backend::RequestHead) -> wire::RequestHead {
    wire::RequestHead {
        method: head.method,
        uri: head.uri,
        version: head.version,
        headers: head.headers,
    }
}

fn response_head_to_wire(head: backend::ResponseHead) -> wire::ResponseHead {
    wire::ResponseHead {
        status: head.status,
        version: head.version,
        headers: head.headers,
    }
}

fn response_head_from_wire(head: wire::ResponseHead) -> backend::ResponseHead {
    backend::ResponseHead {
        status: head.status,
        version: head.version,
        headers: head.headers,
    }
}

fn observed_body_to_wire(body: backend::ObservedBody) -> wire::ObservedBody {
    wire::ObservedBody {
        bytes: body.bytes,
        total_bytes: body.total_bytes,
        truncated: body.truncated,
        complete: body.complete,
    }
}

fn body_chunk_to_wire(chunk: backend::BodyChunk) -> wire::BodyChunk {
    wire::BodyChunk {
        id: chunk.id,
        bytes: chunk.bytes,
    }
}

fn body_ended_to_wire(end: backend::BodyEnded) -> wire::BodyEnded {
    wire::BodyEnded {
        id: end.id,
        total_bytes: end.total_bytes,
        truncated: end.truncated,
    }
}

fn proxy_failure_to_wire(failure: backend::ProxyFailure) -> WireError {
    WireError {
        code: failure.code,
        message: failure.message,
    }
}

fn wire_error(code: impl Into<String>, message: impl Into<String>) -> WireError {
    WireError {
        code: code.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::net::SocketAddr;
    use std::time::Duration;

    use bytes::Bytes;
    use tokio::io::AsyncWriteExt as _;

    fn match_all_config() -> backend::ProxySessionConfig {
        backend::ProxySessionConfig {
            routes: vec![backend::Route {
                methods: vec![],
                host: None,
                path_prefix: None,
            }],
            ..backend::ProxySessionConfig::default()
        }
    }

    /// Connect through the proxy, send a matched request, and hold the socket
    /// open without ever reading the response.
    async fn stalled_matched_request(addr: SocketAddr) -> tokio::net::TcpStream {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET http://match.example/x HTTP/1.1\r\nHost: match.example\r\n\r\n")
            .await
            .unwrap();
        stream
    }

    fn first_request_id(batch: &[wire::InterceptorEvent]) -> Option<u64> {
        batch.iter().find_map(|event| match event {
            wire::InterceptorEvent::Request(request) => Some(request.id),
            _ => None,
        })
    }

    async fn wait_for_request(
        events: &mut BoxStream<'static, Vec<wire::InterceptorEvent>>,
        exclude: u64,
    ) -> Option<u64> {
        // Bound the whole wait, not just each poll: a wedged loop (the bug) or a
        // flood of error events must resolve to `None` rather than hang.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match tokio::time::timeout(remaining, events.next()).await {
                Ok(Some(batch)) => {
                    if let Some(id) = first_request_id(&batch)
                        && id != exclude
                    {
                        return Some(id);
                    }
                }
                Ok(None) | Err(_) => return None,
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 3)]
    async fn a_stalled_synthetic_consumer_does_not_block_other_exchanges() {
        let temp = tempfile::tempdir().unwrap();
        let authority = backend::ProxyAuthority::create(temp.path()).unwrap();
        let proxy = Arc::new(
            backend::ProxySession::start(&authority, match_all_config())
                .await
                .unwrap(),
        );
        let addr = proxy.addr();
        let attachment = proxy.attach_interceptor().unwrap();

        // A client-driven action stream we can feed on demand.
        let (actions_tx, mut actions_rx) =
            tokio::sync::mpsc::channel::<Vec<wire::InterceptorAction>>(64);
        let actions: BoxStream<'static, Vec<wire::InterceptorAction>> =
            Box::pin(async_stream::stream! {
                while let Some(batch) = actions_rx.recv().await {
                    yield batch;
                }
            });
        let mut events = run_intercept(Arc::clone(&proxy), attachment, actions);

        // Exchange A: a client that never reads its synthetic response.
        let _client_a = stalled_matched_request(addr).await;
        let id_a = wait_for_request(&mut events, u64::MAX)
            .await
            .expect("Request(A) should be delivered");

        // Turn A into a synthetic response, then flood it with body chunks. With
        // A's consumer stalled, the per-exchange pending channel fills and
        // apply_action wedges on backpressure.
        let feeder = tokio::spawn({
            let actions_tx = actions_tx.clone();
            async move {
                let start = wire::InterceptorAction::ResponseStart(wire::ResponseStart {
                    id: id_a,
                    head: wire::ResponseHead {
                        status: 200,
                        version: "HTTP/1.1".into(),
                        headers: vec![],
                    },
                });
                if actions_tx.send(vec![start]).await.is_err() {
                    return;
                }
                let chunk = Bytes::from(vec![0_u8; 256 * 1024]);
                loop {
                    let body = wire::InterceptorAction::ResponseBody(wire::BodyChunk {
                        id: id_a,
                        bytes: chunk.clone(),
                    });
                    if actions_tx.send(vec![body]).await.is_err() {
                        break;
                    }
                }
            }
        });

        // Drive the interception loop until the action pump is wedged. No other
        // interceptor events exist yet, so this only advances action handling.
        let _ = tokio::time::timeout(Duration::from_secs(1), events.next()).await;

        // Exchange B: a brand-new request that must still be delivered even while
        // A's pump is wedged on backpressure.
        let _client_b = stalled_matched_request(addr).await;
        let id_b = wait_for_request(&mut events, id_a).await;

        // Clean up before asserting so a failure does not also wedge teardown:
        // stop feeding, close the stalled sockets, and bound the drain.
        feeder.abort();
        drop(_client_a);
        drop(_client_b);
        let _ = tokio::time::timeout(Duration::from_secs(3), proxy.shutdown()).await;

        assert!(
            matches!(id_b, Some(id) if id != id_a),
            "Request(B) was head-of-line blocked by a stalled exchange: {id_b:?}"
        );
    }
}
