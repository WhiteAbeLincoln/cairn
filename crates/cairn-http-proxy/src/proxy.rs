use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use http::{HeaderMap, Request, Response, StatusCode};
use http_body_util::{BodyExt as _, StreamBody};
use hudsucker::hyper::body::{Body as _, Frame};
use hudsucker::rustls::crypto::aws_lc_rs;
use hudsucker::{Body, HttpContext, HttpHandler, Proxy, RequestOrResponse};
use tokio::sync::{Notify, broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::capture::{CaptureStore, ProxyFailure};
use crate::{
    BodyChunk, ExchangeFailed, ExchangeId, InterceptedRequest, ObservationEvent, RequestHead,
    RequestStarted, ResponseHead, ResponseStarted, Route, now_unix_ms,
};

const BAD_GATEWAY: &str = "the session interceptor is unavailable";

#[derive(Clone, Debug)]
pub struct ProxySessionConfig {
    pub routes: Vec<Route>,
    pub max_intercepted_request_bytes: usize,
    pub capture_body_bytes: usize,
    pub replay_max_exchanges: usize,
    pub replay_max_bytes: usize,
    pub max_active_exchanges: usize,
    pub interceptor_wait: Duration,
    pub decision_timeout: Duration,
}

impl Default for ProxySessionConfig {
    fn default() -> Self {
        Self {
            routes: Vec::new(),
            max_intercepted_request_bytes: 8 * 1024 * 1024,
            capture_body_bytes: 1024 * 1024,
            replay_max_exchanges: 256,
            replay_max_bytes: 32 * 1024 * 1024,
            max_active_exchanges: 256,
            interceptor_wait: Duration::from_secs(5),
            decision_timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseStart {
    pub id: ExchangeId,
    pub head: ResponseHead,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterceptorAction {
    Forward(ExchangeId),
    ResponseStart(ResponseStart),
    ResponseBody(BodyChunk),
    ResponseEnd(ExchangeId),
    Fail(ExchangeId, ProxyFailure),
}

impl InterceptorAction {
    fn exchange_id(&self) -> ExchangeId {
        match self {
            Self::Forward(id) | Self::ResponseEnd(id) => *id,
            Self::ResponseStart(start) => start.id,
            Self::ResponseBody(chunk) => chunk.id,
            Self::Fail(id, _) => *id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterceptorEvent {
    Request(InterceptedRequest),
    Cancelled(ExchangeId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyError {
    InterceptorAlreadyAttached,
    InterceptorUnavailable,
    UnknownExchange(ExchangeId),
}

impl fmt::Display for ProxyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InterceptorAlreadyAttached => {
                write!(formatter, "an interceptor is already attached")
            }
            Self::InterceptorUnavailable => write!(formatter, "the interceptor is unavailable"),
            Self::UnknownExchange(id) => write!(formatter, "unknown proxy exchange {id}"),
        }
    }
}

impl std::error::Error for ProxyError {}

pub struct InterceptorAttachment {
    token: u64,
    core: Arc<Core>,
    events: mpsc::Receiver<InterceptorEvent>,
}

impl InterceptorAttachment {
    /// Receive the next interceptor event, or `None` once the proxy detaches.
    ///
    /// # Cancel Safety
    ///
    /// Cancel-safe. This forwards to `tokio::sync::mpsc::Receiver::recv`, which
    /// does not lose a message if the returned future is dropped before it
    /// completes: the event stays queued for the next `recv`.
    pub async fn recv(&mut self) -> Option<InterceptorEvent> {
        self.events.recv().await
    }
}

impl Drop for InterceptorAttachment {
    fn drop(&mut self) {
        self.core.detach_interceptor(self.token);
    }
}

pub struct ProxySession {
    core: Arc<Core>,
    addr: SocketAddr,
    shutdown: CancellationToken,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ProxySession {
    pub async fn start(
        authority: &crate::ProxyAuthority,
        config: ProxySessionConfig,
    ) -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = listener.local_addr()?;
        let core = Arc::new(Core::new(config));
        let handler = CairnHandler {
            core: Arc::clone(&core),
            exchange_id: None,
        };
        let shutdown = CancellationToken::new();
        let proxy = Proxy::builder()
            .with_listener(listener)
            .with_ca(authority.authority()?)
            .with_rustls_connector(aws_lc_rs::default_provider())
            .with_http_handler(handler)
            .with_graceful_shutdown(shutdown.clone().cancelled_owned())
            .build()?;
        let task = tokio::spawn(async move {
            if let Err(error) = proxy.start().await {
                tracing::warn!(%error, "session HTTP proxy stopped with an error");
            }
        });
        Ok(Self {
            core,
            addr,
            shutdown,
            task: Mutex::new(Some(task)),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn proxy_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn attach_interceptor(&self) -> Result<InterceptorAttachment, ProxyError> {
        self.core.attach_interceptor()
    }

    /// Deliver an interceptor action to the exchange it names.
    ///
    /// # Cancel Safety
    ///
    /// Not cancel-safe with respect to the action: internally this awaits a send
    /// on the per-exchange channel, so if the returned future is dropped while
    /// that send is pending (e.g. the exchange's consumer is backpressured), the
    /// action is lost. Callers that must not lose actions should drive this to
    /// completion. Losing the action on teardown is intentional: it only happens
    /// when the whole interception is being cancelled.
    pub async fn apply_action(&self, action: InterceptorAction) -> Result<(), ProxyError> {
        self.core.apply_action(action).await
    }

    pub fn subscribe_observations(
        &self,
    ) -> (
        Vec<crate::ObservedExchange>,
        broadcast::Receiver<ObservationEvent>,
    ) {
        self.core.subscribe_observations()
    }

    /// Signal the proxy to stop and wait for its accept loop to drain.
    ///
    /// # Cancel Safety
    ///
    /// Cancel-safe for the shutdown signal: the cancellation is delivered
    /// synchronously before the first await, so dropping the returned future
    /// still stops the proxy. Only the join is skipped — the task continues its
    /// own graceful drain in the background.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        let task = lock_recover(&self.task).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

impl Drop for ProxySession {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

struct AttachedInterceptor {
    token: u64,
    events: mpsc::Sender<InterceptorEvent>,
}

struct Core {
    config: ProxySessionConfig,
    next_exchange_id: AtomicU64,
    next_interceptor_token: AtomicU64,
    active_exchanges: Mutex<HashSet<ExchangeId>>,
    interceptor: Mutex<Option<AttachedInterceptor>>,
    interceptor_changed: Notify,
    pending: Mutex<HashMap<ExchangeId, mpsc::Sender<InterceptorAction>>>,
    capture: Mutex<CaptureStore>,
    observations: broadcast::Sender<ObservationEvent>,
}

impl Core {
    fn new(config: ProxySessionConfig) -> Self {
        let (observations, _) = broadcast::channel(256);
        Self {
            capture: Mutex::new(CaptureStore::new(
                config.replay_max_exchanges,
                config.replay_max_bytes,
                config.capture_body_bytes,
            )),
            config,
            next_exchange_id: AtomicU64::new(1),
            next_interceptor_token: AtomicU64::new(1),
            active_exchanges: Mutex::new(HashSet::new()),
            interceptor: Mutex::new(None),
            interceptor_changed: Notify::new(),
            pending: Mutex::new(HashMap::new()),
            observations,
        }
    }

    fn attach_interceptor(self: &Arc<Self>) -> Result<InterceptorAttachment, ProxyError> {
        let mut slot = lock_recover(&self.interceptor);
        if slot.is_some() {
            return Err(ProxyError::InterceptorAlreadyAttached);
        }
        let token = self.next_interceptor_token.fetch_add(1, Ordering::Relaxed);
        let (events, receiver) = mpsc::channel(128);
        *slot = Some(AttachedInterceptor { token, events });
        drop(slot);
        self.interceptor_changed.notify_waiters();
        Ok(InterceptorAttachment {
            token,
            core: Arc::clone(self),
            events: receiver,
        })
    }

    fn detach_interceptor(&self, token: u64) {
        let mut slot = lock_recover(&self.interceptor);
        if slot
            .as_ref()
            .is_some_and(|attached| attached.token == token)
        {
            *slot = None;
            lock_recover(&self.pending).clear();
        }
    }

    async fn send_interceptor_event(&self, event: InterceptorEvent) -> Result<(), ProxyError> {
        let deadline = tokio::time::Instant::now() + self.config.interceptor_wait;
        loop {
            let notified = self.interceptor_changed.notified();
            let sender = lock_recover(&self.interceptor)
                .as_ref()
                .map(|attached| attached.events.clone());
            if let Some(sender) = sender {
                // The deadline bounds delivery as well as attach-waiting: an
                // interceptor that is attached but not draining must not be able
                // to wedge the request forever. `reserve` is cancel-safe (it only
                // loses queue position on drop) and the synchronous `send`
                // afterwards cannot lose the event.
                return match tokio::time::timeout_at(deadline, sender.reserve()).await {
                    Ok(Ok(permit)) => {
                        permit.send(event);
                        Ok(())
                    }
                    Ok(Err(_)) | Err(_) => Err(ProxyError::InterceptorUnavailable),
                };
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Err(ProxyError::InterceptorUnavailable);
            }
        }
    }

    async fn apply_action(&self, action: InterceptorAction) -> Result<(), ProxyError> {
        let id = action.exchange_id();
        let sender = lock_recover(&self.pending).get(&id).cloned();
        let sender = sender.ok_or(ProxyError::UnknownExchange(id))?;
        sender
            .send(action)
            .await
            .map_err(|_| ProxyError::UnknownExchange(id))
    }

    fn subscribe_observations(
        &self,
    ) -> (
        Vec<crate::ObservedExchange>,
        broadcast::Receiver<ObservationEvent>,
    ) {
        let receiver = self.observations.subscribe();
        let snapshot = lock_recover(&self.capture).snapshot();
        (snapshot, receiver)
    }

    fn begin_exchange(&self, head: RequestHead) -> Result<ExchangeId, ()> {
        let mut active = lock_recover(&self.active_exchanges);
        if active.len() >= self.config.max_active_exchanges {
            return Err(());
        }
        let id = self.next_exchange_id.fetch_add(1, Ordering::Relaxed);
        active.insert(id);
        drop(active);
        let unix_ms = now_unix_ms();
        lock_recover(&self.capture).start(id, head.clone(), unix_ms);
        self.emit(ObservationEvent::RequestStart(RequestStarted {
            id,
            head,
            unix_ms,
        }));
        Ok(id)
    }

    fn request_chunk(&self, id: ExchangeId, bytes: &Bytes) {
        if let Some(bytes) = lock_recover(&self.capture).request_chunk(id, bytes) {
            self.emit(ObservationEvent::RequestBody(BodyChunk { id, bytes }));
        }
    }

    fn end_request(&self, id: ExchangeId) {
        if let Some(end) = lock_recover(&self.capture).end_request(id) {
            self.emit(ObservationEvent::RequestEnd(end));
        }
    }

    fn start_response(&self, id: ExchangeId, head: ResponseHead) {
        lock_recover(&self.capture).start_response(id, head.clone());
        self.emit(ObservationEvent::ResponseStart(ResponseStarted {
            id,
            head,
        }));
    }

    fn response_chunk(&self, id: ExchangeId, bytes: &Bytes) {
        if let Some(bytes) = lock_recover(&self.capture).response_chunk(id, bytes) {
            self.emit(ObservationEvent::ResponseBody(BodyChunk { id, bytes }));
        }
    }

    fn complete(&self, id: ExchangeId) {
        if !self.finish_slot(id) {
            return;
        }
        if let Some(end) = lock_recover(&self.capture).end_response(id) {
            self.emit(ObservationEvent::ResponseEnd(end));
        }
        let unix_ms = now_unix_ms();
        lock_recover(&self.capture).complete(id, unix_ms);
        lock_recover(&self.pending).remove(&id);
        self.emit(ObservationEvent::Completed(id, unix_ms));
    }

    fn fail(&self, id: ExchangeId, code: &str, message: impl Into<String>) {
        if !self.finish_slot(id) {
            return;
        }
        let failure = ProxyFailure {
            code: code.into(),
            message: message.into(),
        };
        lock_recover(&self.capture).fail(id, failure.clone());
        lock_recover(&self.pending).remove(&id);
        self.emit(ObservationEvent::Failed(ExchangeFailed { id, failure }));
    }

    fn cancel(&self, id: ExchangeId) {
        if !self.finish_slot(id) {
            return;
        }
        let failure = ProxyFailure {
            code: "proxy.downstream_cancelled".into(),
            message: "the downstream process cancelled the exchange".into(),
        };
        lock_recover(&self.capture).fail(id, failure.clone());
        lock_recover(&self.pending).remove(&id);
        let interceptor = lock_recover(&self.interceptor)
            .as_ref()
            .map(|attached| attached.events.clone());
        if let Some(interceptor) = interceptor {
            // `cancel` runs from a synchronous `Drop`, so it cannot await the
            // send directly. A bare `try_send` silently drops the event when the
            // 128-deep channel is momentarily full (e.g. a restart SIGKILL
            // burst), leaving the interceptor with a dead exchange forever. When
            // a runtime is available, deliver it reliably with a bounded
            // background send; only fall back to a best-effort `try_send` when
            // there is no reactor to spawn onto.
            let timeout = self.config.interceptor_wait;
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        let _ = tokio::time::timeout(
                            timeout,
                            interceptor.send(InterceptorEvent::Cancelled(id)),
                        )
                        .await;
                    });
                }
                Err(_) => {
                    let _ = interceptor.try_send(InterceptorEvent::Cancelled(id));
                }
            }
        }
        self.emit(ObservationEvent::Failed(ExchangeFailed { id, failure }));
    }

    /// Finalize an exchange that is being forwarded as a protocol upgrade
    /// (WebSocket or otherwise). hudsucker routes upgrade requests straight to
    /// its websocket path without ever invoking `handle_response`, so no
    /// response body guard is created to release the active-exchange slot. We
    /// release it here and emit a terminal observation; the upgrade itself may
    /// still forward.
    fn complete_forwarded(&self, id: ExchangeId) {
        if !self.finish_slot(id) {
            return;
        }
        let unix_ms = now_unix_ms();
        lock_recover(&self.capture).complete(id, unix_ms);
        lock_recover(&self.pending).remove(&id);
        self.emit(ObservationEvent::Completed(id, unix_ms));
    }

    fn finish_slot(&self, id: ExchangeId) -> bool {
        lock_recover(&self.active_exchanges).remove(&id)
    }

    fn emit(&self, event: ObservationEvent) {
        let _ = self.observations.send(event);
    }

    #[cfg(test)]
    fn active_len(&self) -> usize {
        lock_recover(&self.active_exchanges).len()
    }
}

#[derive(Clone)]
struct CairnHandler {
    core: Arc<Core>,
    exchange_id: Option<ExchangeId>,
}

struct ExchangeGuard {
    core: Arc<Core>,
    id: ExchangeId,
    armed: bool,
}

impl ExchangeGuard {
    fn new(core: Arc<Core>, id: ExchangeId) -> Self {
        Self {
            core,
            id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ExchangeGuard {
    fn drop(&mut self) {
        if self.armed {
            self.core.cancel(self.id);
        }
    }
}

#[derive(Clone, Copy)]
enum ObservedDirection {
    Request,
    Response,
}

struct ObservedBodyGuard {
    core: Arc<Core>,
    id: ExchangeId,
    direction: ObservedDirection,
    expected: Option<u64>,
    delivered: u64,
    armed: bool,
}

impl ObservedBodyGuard {
    fn new(
        core: Arc<Core>,
        id: ExchangeId,
        direction: ObservedDirection,
        expected: Option<u64>,
    ) -> Self {
        Self {
            core,
            id,
            direction,
            expected,
            delivered: 0,
            armed: true,
        }
    }

    fn record(&mut self, bytes: usize) {
        self.delivered = self.delivered.saturating_add(bytes as u64);
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ObservedBodyGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if self.expected == Some(self.delivered) {
            match self.direction {
                ObservedDirection::Request => self.core.end_request(self.id),
                ObservedDirection::Response => self.core.complete(self.id),
            }
        } else {
            self.core.cancel(self.id);
        }
    }
}

impl HttpHandler for CairnHandler {
    async fn handle_request(
        &mut self,
        _context: &HttpContext,
        request: Request<Body>,
    ) -> RequestOrResponse {
        self.on_request(request).await
    }

    async fn handle_response(
        &mut self,
        _context: &HttpContext,
        response: Response<Body>,
    ) -> Response<Body> {
        self.on_response(response)
    }

    async fn handle_error(
        &mut self,
        _context: &HttpContext,
        error: hudsucker::hyper_util::client::legacy::Error,
    ) -> Response<Body> {
        if let Some(id) = self.exchange_id.take() {
            self.core.fail(id, "proxy.upstream", error.to_string());
        }
        failure_response(StatusCode::BAD_GATEWAY, "upstream request failed")
    }
}

impl CairnHandler {
    async fn on_request(&mut self, request: Request<Body>) -> RequestOrResponse {
        // CONNECT establishes the tunnel that carries the decrypted HTTP
        // exchange. Routing and observation apply to the inner request.
        if request.method() == http::Method::CONNECT {
            return request.into();
        }
        let head = request_head(&request);
        let id = match self.core.begin_exchange(head.clone()) {
            Ok(id) => id,
            Err(()) => {
                return failure_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "proxy capacity exhausted",
                )
                .into();
            }
        };
        self.exchange_id = Some(id);
        let mut guard = ExchangeGuard::new(Arc::clone(&self.core), id);
        let matched = self
            .core
            .config
            .routes
            .iter()
            .any(|route| route.matches(&request));
        let upgrade = is_upgrade(&request);
        if matched && upgrade {
            self.core.fail(
                id,
                "proxy.unsupported_upgrade",
                "matched protocol upgrades are unsupported",
            );
            guard.disarm();
            return failure_response(
                StatusCode::NOT_IMPLEMENTED,
                "matched protocol upgrades are unsupported",
            )
            .into();
        }

        let (parts, body) = request.into_parts();
        if !matched {
            if upgrade {
                // hudsucker hands upgrade requests to its websocket path after
                // this returns and never calls handle_response, so the exchange
                // must be finalized here to release its slot. The upgrade still
                // forwards with its original (unobserved) body.
                self.core.complete_forwarded(id);
                guard.disarm();
                return Request::from_parts(parts, body).into();
            }
            let body = observed_request_body(Arc::clone(&self.core), id, body);
            guard.disarm();
            return Request::from_parts(parts, body).into();
        }

        let collected = match collect_bounded(
            Arc::clone(&self.core),
            id,
            body,
            self.core.config.max_intercepted_request_bytes,
            self.core.config.decision_timeout,
        )
        .await
        {
            Ok(collected) => collected,
            Err(CollectError::TooLarge) => {
                self.core.fail(
                    id,
                    "proxy.request_too_large",
                    "intercepted request body exceeds the configured limit",
                );
                guard.disarm();
                return failure_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "intercepted request body is too large",
                )
                .into();
            }
            Err(CollectError::Timeout) => {
                self.core.fail(
                    id,
                    "proxy.request_timeout",
                    "intercepted request upload timed out",
                );
                guard.disarm();
                return failure_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    "intercepted request upload timed out",
                )
                .into();
            }
            Err(CollectError::Body(error)) => {
                self.core.fail(id, "proxy.request_body", error.to_string());
                guard.disarm();
                return failure_response(
                    StatusCode::BAD_GATEWAY,
                    "failed to read intercepted request body",
                )
                .into();
            }
        };
        let CollectedBody {
            bytes: body,
            trailers,
        } = collected;

        let (actions_tx, mut actions_rx) = mpsc::channel(32);
        lock_recover(&self.core.pending).insert(id, actions_tx);
        let event = InterceptorEvent::Request(InterceptedRequest {
            id,
            head,
            body: body.clone(),
        });
        if let Err(error) = self.core.send_interceptor_event(event).await {
            self.core
                .fail(id, "proxy.interceptor_unavailable", error.to_string());
            guard.disarm();
            return failure_response(StatusCode::BAD_GATEWAY, BAD_GATEWAY).into();
        }

        let decision =
            tokio::time::timeout(self.core.config.decision_timeout, actions_rx.recv()).await;
        match decision {
            Ok(Some(InterceptorAction::Forward(action_id))) if action_id == id => {
                guard.disarm();
                Request::from_parts(parts, forward_body(body, trailers)).into()
            }
            Ok(Some(InterceptorAction::ResponseStart(start))) if start.id == id => {
                match synthetic_response(Arc::clone(&self.core), id, start.head, actions_rx) {
                    Ok(response) => {
                        guard.disarm();
                        response.into()
                    }
                    Err(message) => {
                        self.core
                            .fail(id, "proxy.invalid_response", message.clone());
                        guard.disarm();
                        failure_response(StatusCode::BAD_GATEWAY, &message).into()
                    }
                }
            }
            Ok(Some(InterceptorAction::Fail(action_id, failure))) if action_id == id => {
                self.core.fail(id, &failure.code, failure.message);
                guard.disarm();
                failure_response(
                    StatusCode::BAD_GATEWAY,
                    "the interceptor rejected the request",
                )
                .into()
            }
            Ok(Some(_)) => {
                self.core
                    .fail(id, "proxy.protocol", "invalid first interceptor action");
                guard.disarm();
                failure_response(StatusCode::BAD_GATEWAY, "invalid interceptor response").into()
            }
            Ok(None) => {
                self.core
                    .fail(id, "proxy.interceptor_disconnected", BAD_GATEWAY);
                guard.disarm();
                failure_response(StatusCode::BAD_GATEWAY, BAD_GATEWAY).into()
            }
            Err(_) => {
                self.core.fail(
                    id,
                    "proxy.interceptor_timeout",
                    "interceptor decision timed out",
                );
                guard.disarm();
                failure_response(
                    StatusCode::GATEWAY_TIMEOUT,
                    "interceptor decision timed out",
                )
                .into()
            }
        }
    }

    fn on_response(&mut self, response: Response<Body>) -> Response<Body> {
        let Some(id) = self.exchange_id else {
            return response;
        };
        let head = response_head(&response);
        self.core.start_response(id, head);
        let (parts, body) = response.into_parts();
        Response::from_parts(
            parts,
            observed_response_body(Arc::clone(&self.core), id, body),
        )
    }
}

fn observed_request_body(core: Arc<Core>, id: ExchangeId, body: Body) -> Body {
    observed_body(core, id, ObservedDirection::Request, body)
}

fn observed_response_body(core: Arc<Core>, id: ExchangeId, body: Body) -> Body {
    observed_body(core, id, ObservedDirection::Response, body)
}

/// Wrap a body so its data is captured/observed as it streams, while releasing
/// the active-exchange slot no matter how the body ends — including when the
/// downstream drops it before ever polling it.
///
/// The `ObservedBodyGuard` is constructed here and moved into the generator, so
/// it lives in the returned body's state from creation. Dropping the body
/// before its first poll therefore still runs the guard's `Drop`, cancelling
/// the exchange rather than leaking its slot.
fn observed_body(
    core: Arc<Core>,
    id: ExchangeId,
    direction: ObservedDirection,
    mut body: Body,
) -> Body {
    let expected = body.size_hint().exact();
    let mut guard = ObservedBodyGuard::new(Arc::clone(&core), id, direction, expected);
    // Yield whole frames (data *and* trailers) so trailer frames — e.g. gRPC's
    // `grpc-status` — reach the peer intact. Only DATA frames are captured and
    // counted toward the observation.
    Body::from(StreamBody::new(async_stream::stream! {
        // `guard` is captured by the surrounding `async move`, so it is owned by
        // this future the moment `observed_body` returns; the uses below force
        // the move-capture.
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => match frame.into_data() {
                    Ok(bytes) => {
                        match direction {
                            ObservedDirection::Request => core.request_chunk(id, &bytes),
                            ObservedDirection::Response => core.response_chunk(id, &bytes),
                        }
                        guard.record(bytes.len());
                        yield Ok::<Frame<Bytes>, hudsucker::Error>(Frame::data(bytes));
                    }
                    // Non-data frame (trailers): forward as-is, not captured.
                    Err(frame) => yield Ok(frame),
                },
                Err(error) => {
                    let code = match direction {
                        ObservedDirection::Request => "proxy.request_body",
                        ObservedDirection::Response => "proxy.response_body",
                    };
                    core.fail(id, code, error.to_string());
                    guard.disarm();
                    yield Err(error);
                    return;
                }
            }
        }
        match direction {
            ObservedDirection::Request => core.end_request(id),
            ObservedDirection::Response => core.complete(id),
        }
        guard.disarm();
    }))
}

fn synthetic_response(
    core: Arc<Core>,
    id: ExchangeId,
    head: ResponseHead,
    mut actions: mpsc::Receiver<InterceptorAction>,
) -> Result<Response<Body>, String> {
    let mut builder = Response::builder().status(head.status);
    for (name, value) in &head.headers {
        builder = builder.header(name, value.as_ref());
    }
    core.start_response(id, head);
    // Construct the guard here and move it into the generator so a body dropped
    // before its first poll (client disconnects before reading) still runs Drop
    // and cancels the exchange rather than leaking its slot.
    let mut guard = ExchangeGuard::new(Arc::clone(&core), id);
    let stream = async_stream::stream! {
        while let Some(action) = actions.recv().await {
            match action {
                InterceptorAction::ResponseBody(chunk) if chunk.id == id => {
                    core.response_chunk(id, &chunk.bytes);
                    yield Ok::<Bytes, std::io::Error>(chunk.bytes);
                }
                InterceptorAction::ResponseEnd(action_id) if action_id == id => {
                    core.complete(id);
                    guard.disarm();
                    return;
                }
                InterceptorAction::Fail(action_id, failure) if action_id == id => {
                    core.fail(id, &failure.code, failure.message);
                    guard.disarm();
                    return;
                }
                _ => {
                    core.fail(id, "proxy.protocol", "invalid synthetic response action");
                    guard.disarm();
                    return;
                }
            }
        }
        core.fail(id, "proxy.interceptor_disconnected", BAD_GATEWAY);
        guard.disarm();
    };
    builder
        .body(Body::from_stream(stream))
        .map_err(|error| error.to_string())
}

enum CollectError {
    TooLarge,
    Timeout,
    Body(hudsucker::Error),
}

/// A fully buffered intercepted request body plus any trailer frames observed
/// while reading it, so trailers can be re-attached when the request is
/// forwarded.
struct CollectedBody {
    bytes: Bytes,
    trailers: Option<HeaderMap>,
}

/// Read a matched request body into memory, bounded by both `limit` (size) and
/// `timeout` (the whole upload phase). Bounding the upload phase stops a
/// slow-loris body under the size limit from pinning an active-exchange slot
/// while it dribbles in before the interceptor decision even begins.
async fn collect_bounded(
    core: Arc<Core>,
    id: ExchangeId,
    mut body: Body,
    limit: usize,
    timeout: Duration,
) -> Result<CollectedBody, CollectError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut collected = Vec::new();
    let mut trailers: Option<HeaderMap> = None;
    loop {
        let frame = match tokio::time::timeout_at(deadline, body.frame()).await {
            Ok(Some(frame)) => frame.map_err(CollectError::Body)?,
            Ok(None) => break,
            Err(_) => return Err(CollectError::Timeout),
        };
        match frame.into_data() {
            Ok(bytes) => {
                if collected.len().saturating_add(bytes.len()) > limit {
                    return Err(CollectError::TooLarge);
                }
                core.request_chunk(id, &bytes);
                collected.extend_from_slice(&bytes);
            }
            Err(frame) => {
                if let Ok(map) = frame.into_trailers() {
                    match trailers.as_mut() {
                        Some(existing) => existing.extend(map),
                        None => trailers = Some(map),
                    }
                }
            }
        }
    }
    core.end_request(id);
    Ok(CollectedBody {
        bytes: Bytes::from(collected),
        trailers,
    })
}

/// Rebuild a forwarded request body from its buffered bytes, re-attaching any
/// trailer frames captured during collection so gRPC request trailers survive.
fn forward_body(bytes: Bytes, trailers: Option<HeaderMap>) -> Body {
    match trailers {
        None => Body::from(bytes),
        Some(trailers) => Body::from(StreamBody::new(async_stream::stream! {
            if !bytes.is_empty() {
                yield Ok::<Frame<Bytes>, hudsucker::Error>(Frame::data(bytes));
            }
            yield Ok(Frame::trailers(trailers));
        })),
    }
}

fn request_head(request: &Request<Body>) -> RequestHead {
    RequestHead {
        method: request.method().to_string(),
        uri: request.uri().to_string(),
        version: version_string(request.version()),
        headers: request
            .headers()
            .iter()
            .map(|(name, value)| (name.to_string(), Bytes::copy_from_slice(value.as_bytes())))
            .collect(),
    }
}

fn response_head(response: &Response<Body>) -> ResponseHead {
    ResponseHead {
        status: response.status().as_u16(),
        version: version_string(response.version()),
        headers: response
            .headers()
            .iter()
            .map(|(name, value)| (name.to_string(), Bytes::copy_from_slice(value.as_bytes())))
            .collect(),
    }
}

fn version_string(version: http::Version) -> String {
    match version {
        http::Version::HTTP_09 => "HTTP/0.9",
        http::Version::HTTP_10 => "HTTP/1.0",
        http::Version::HTTP_11 => "HTTP/1.1",
        http::Version::HTTP_2 => "HTTP/2",
        http::Version::HTTP_3 => "HTTP/3",
        _ => "HTTP/unknown",
    }
    .into()
}

fn is_upgrade(request: &Request<Body>) -> bool {
    request.headers().contains_key(http::header::UPGRADE)
}

fn failure_response(status: StatusCode, message: &str) -> Response<Body> {
    match Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(message.to_owned()))
    {
        Ok(response) => response,
        Err(_) => {
            let mut response = Response::new(Body::from(message.to_owned()));
            *response.status_mut() = status;
            response
        }
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ProxySessionConfig {
        ProxySessionConfig {
            routes: vec![Route {
                methods: vec![],
                host: None,
                path_prefix: None,
            }],
            interceptor_wait: Duration::from_millis(20),
            decision_timeout: Duration::from_millis(20),
            ..ProxySessionConfig::default()
        }
    }

    #[tokio::test]
    async fn only_one_interceptor_can_attach() {
        let core = Arc::new(Core::new(config()));
        let first = core.attach_interceptor().unwrap();
        assert_eq!(
            core.attach_interceptor().err(),
            Some(ProxyError::InterceptorAlreadyAttached)
        );
        drop(first);
        assert!(core.attach_interceptor().is_ok());
    }

    #[tokio::test]
    async fn interceptor_send_times_out_when_channel_is_not_drained() {
        let core = Arc::new(Core::new(config()));
        // Keep the attachment alive but never drain it, so the 128-deep channel
        // stays full.
        let _attachment = core.attach_interceptor().unwrap();
        for _ in 0..128 {
            core.send_interceptor_event(InterceptorEvent::Cancelled(0))
                .await
                .expect("the channel has room for the first 128 events");
        }
        // The next delivery must fail closed within interceptor_wait rather than
        // hang forever waiting for capacity that will never come. The outer
        // timeout is a backstop: if delivery hangs (the bug) it elapses and the
        // assertion fails; the fix returns InterceptorUnavailable well before it.
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            core.send_interceptor_event(InterceptorEvent::Cancelled(1)),
        )
        .await;
        match result {
            Ok(Err(ProxyError::InterceptorUnavailable)) => {}
            other => panic!("expected a bounded InterceptorUnavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancel_delivers_even_when_the_interceptor_channel_is_full() {
        let config = ProxySessionConfig {
            interceptor_wait: Duration::from_secs(5),
            ..config()
        };
        let core = Arc::new(Core::new(config));
        let mut attachment = core.attach_interceptor().unwrap();
        let id = core
            .begin_exchange(RequestHead {
                method: "GET".into(),
                uri: "http://example/".into(),
                version: "HTTP/1.1".into(),
                headers: vec![],
            })
            .expect("capacity available");
        // Saturate the 128-deep interceptor channel with sentinel events.
        for _ in 0..128 {
            core.send_interceptor_event(InterceptorEvent::Cancelled(0))
                .await
                .expect("channel has room");
        }
        // A downstream cancellation raised while the channel is full (e.g. a
        // restart SIGKILL burst) must still reach the interceptor, otherwise it
        // keeps a dead exchange live forever.
        core.cancel(id);
        let mut saw_cancel = false;
        loop {
            match tokio::time::timeout(Duration::from_millis(500), attachment.recv()).await {
                Ok(Some(InterceptorEvent::Cancelled(cancelled))) if cancelled == id => {
                    saw_cancel = true;
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => break,
            }
        }
        assert!(
            saw_cancel,
            "the Cancelled event was dropped on a full channel"
        );
    }

    fn request_head() -> RequestHead {
        RequestHead {
            method: "GET".into(),
            uri: "http://example/".into(),
            version: "HTTP/1.1".into(),
            headers: vec![],
        }
    }

    #[tokio::test]
    async fn dropping_an_unpolled_observed_response_body_releases_the_slot() {
        let core = Arc::new(Core::new(config()));
        let id = core.begin_exchange(request_head()).expect("capacity");
        assert_eq!(core.active_len(), 1);
        // A downstream that drops the body before polling it (never reads the
        // response) must not strand the active-exchange slot.
        let observed = observed_response_body(
            Arc::clone(&core),
            id,
            Body::from(Bytes::from_static(b"data")),
        );
        drop(observed);
        assert_eq!(
            core.active_len(),
            0,
            "response slot leaked on unpolled drop"
        );
    }

    #[tokio::test]
    async fn dropping_an_unpolled_observed_request_body_releases_the_slot() {
        let core = Arc::new(Core::new(config()));
        let id = core.begin_exchange(request_head()).expect("capacity");
        assert_eq!(core.active_len(), 1);
        let observed = observed_request_body(
            Arc::clone(&core),
            id,
            Body::from(Bytes::from_static(b"data")),
        );
        drop(observed);
        assert_eq!(core.active_len(), 0, "request slot leaked on unpolled drop");
    }

    #[tokio::test]
    async fn a_slow_matched_upload_times_out_and_frees_the_slot() {
        // config() matches everything; give the upload a short budget.
        let config = ProxySessionConfig {
            decision_timeout: Duration::from_millis(50),
            ..config()
        };
        let core = Arc::new(Core::new(config));
        let mut handler = CairnHandler {
            core: Arc::clone(&core),
            exchange_id: None,
        };
        // A matched request whose body dribbles one chunk then stalls forever —
        // a slow-loris upload. It must not hold an active slot indefinitely.
        let body = Body::from_stream(async_stream::stream! {
            yield Ok::<Bytes, hudsucker::Error>(Bytes::from_static(b"partial"));
            std::future::pending::<()>().await;
            yield Ok(Bytes::from_static(b"never"));
        });
        let request = Request::builder()
            .method("POST")
            .uri("http://bridge.example/upload")
            .body(body)
            .expect("valid request");
        let outcome = tokio::time::timeout(Duration::from_secs(2), handler.on_request(request))
            .await
            .expect("on_request must not hang on a slow upload");
        let RequestOrResponse::Response(response) = outcome else {
            panic!("expected a failure response for a timed-out upload");
        };
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(core.active_len(), 0, "timed-out upload leaked its slot");
    }

    #[tokio::test]
    async fn dropping_an_unpolled_synthetic_response_releases_the_slot() {
        let core = Arc::new(Core::new(config()));
        let id = core.begin_exchange(request_head()).expect("capacity");
        let (_actions_tx, actions_rx) = mpsc::channel(1);
        let head = ResponseHead {
            status: 200,
            version: "HTTP/1.1".into(),
            headers: vec![],
        };
        let response = synthetic_response(Arc::clone(&core), id, head, actions_rx)
            .expect("valid synthetic response");
        // A client that disconnects before reading the synthetic response drops
        // the body unpolled; the slot must still be released.
        drop(response);
        assert_eq!(
            core.active_len(),
            0,
            "synthetic response slot leaked on unpolled drop"
        );
    }

    #[tokio::test]
    async fn observed_body_forwards_trailer_frames() {
        let core = Arc::new(Core::new(config()));
        let id = core.begin_exchange(request_head()).expect("capacity");
        // A body that ends with a trailer frame (e.g. gRPC's grpc-status) must
        // survive the observed/forwarded path with its trailers intact.
        let mut trailers = HeaderMap::new();
        trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
        let trailers_for_body = trailers.clone();
        let inner = Body::from(StreamBody::new(async_stream::stream! {
            yield Ok::<Frame<Bytes>, hudsucker::Error>(Frame::data(Bytes::from_static(b"payload")));
            yield Ok(Frame::trailers(trailers_for_body));
        }));
        let observed = observed_response_body(Arc::clone(&core), id, inner);
        let collected = observed.collect().await.expect("body collects");
        assert_eq!(collected.trailers().cloned(), Some(trailers));
    }

    #[tokio::test]
    async fn unmatched_upgrade_request_does_not_leak_a_slot() {
        // No routes → the request is unmatched and forwarded. hudsucker routes
        // upgrade requests straight to its websocket path without ever calling
        // handle_response, and drops the request body unpolled, so the exchange
        // must be finalized during handle_request or its slot leaks forever.
        let config = ProxySessionConfig {
            routes: vec![],
            ..config()
        };
        let core = Arc::new(Core::new(config));
        let mut handler = CairnHandler {
            core: Arc::clone(&core),
            exchange_id: None,
        };
        let request = Request::builder()
            .method("GET")
            .uri("http://bridge.example/ws")
            .header(http::header::CONNECTION, "Upgrade")
            .header(http::header::UPGRADE, "websocket")
            .body(Body::empty())
            .expect("valid request");
        let outcome = handler.on_request(request).await;
        assert!(
            matches!(outcome, RequestOrResponse::Request(_)),
            "an unmatched upgrade should still be forwarded"
        );
        assert_eq!(core.active_len(), 0, "the upgrade exchange leaked its slot");
    }

    #[tokio::test]
    async fn actions_are_correlated_by_exchange_id() {
        let core = Arc::new(Core::new(config()));
        let (sender, mut receiver) = mpsc::channel(1);
        lock_recover(&core.pending).insert(42, sender);
        core.apply_action(InterceptorAction::Forward(42))
            .await
            .unwrap();
        assert_eq!(receiver.recv().await, Some(InterceptorAction::Forward(42)));
        assert_eq!(
            core.apply_action(InterceptorAction::Forward(7)).await,
            Err(ProxyError::UnknownExchange(7))
        );
    }
}
