//! wRPC adapters for the reusable HTTP proxy backend.

use cairn_http_proxy as backend;
use cairn_protocol::cairn::daemon::types::Error as WireError;
use cairn_protocol::exports::cairn::daemon::http_proxy as wire;
use futures::StreamExt as _;
use futures::stream::{self, BoxStream};

use crate::daemon::Daemon;

pub fn intercept(
    daemon: &Daemon,
    id: String,
    mut actions: BoxStream<'static, Vec<wire::InterceptorAction>>,
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
    let mut attachment = match proxy.attach_interceptor() {
        Ok(attachment) => attachment,
        Err(error) => {
            return one_interceptor_error("proxy.interceptor_attached", &error.to_string());
        }
    };

    Box::pin(async_stream::stream! {
        loop {
            enum Next {
                Event(Option<backend::InterceptorEvent>),
                Actions(Option<Vec<wire::InterceptorAction>>),
            }
            let next = tokio::select! {
                event = attachment.recv() => Next::Event(event),
                item = actions.next() => Next::Actions(item),
            };
            match next {
                Next::Event(Some(event)) => yield vec![interceptor_event_to_wire(event)],
                Next::Event(None) | Next::Actions(None) => break,
                Next::Actions(Some(batch)) => {
                    for action in batch {
                        let action = match interceptor_action_from_wire(action) {
                            Ok(action) => action,
                            Err(error) => {
                                yield vec![wire::InterceptorEvent::Error(error)];
                                continue;
                            }
                        };
                        if let Err(error) = proxy.apply_action(action).await {
                            yield vec![wire::InterceptorEvent::Error(wire_error(
                                "proxy.invalid_action",
                                error.to_string(),
                            ))];
                        }
                    }
                }
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
