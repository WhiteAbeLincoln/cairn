use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::BodyExt as _;
use hudsucker::hyper::body::Body as _;
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
                return sender
                    .send(event)
                    .await
                    .map_err(|_| ProxyError::InterceptorUnavailable);
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
            let _ = interceptor.try_send(InterceptorEvent::Cancelled(id));
        }
        self.emit(ObservationEvent::Failed(ExchangeFailed { id, failure }));
    }

    fn finish_slot(&self, id: ExchangeId) -> bool {
        lock_recover(&self.active_exchanges).remove(&id)
    }

    fn emit(&self, event: ObservationEvent) {
        let _ = self.observations.send(event);
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
        if matched && is_upgrade(&request) {
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
            let body = observed_request_body(Arc::clone(&self.core), id, body);
            guard.disarm();
            return Request::from_parts(parts, body).into();
        }

        let body = match collect_bounded(
            Arc::clone(&self.core),
            id,
            body,
            self.core.config.max_intercepted_request_bytes,
        )
        .await
        {
            Ok(body) => body,
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
                Request::from_parts(parts, Body::from(body)).into()
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

    async fn handle_response(
        &mut self,
        _context: &HttpContext,
        response: Response<Body>,
    ) -> Response<Body> {
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

fn observed_request_body(core: Arc<Core>, id: ExchangeId, mut body: Body) -> Body {
    let expected = body.size_hint().exact();
    Body::from_stream(async_stream::stream! {
        let mut guard = ObservedBodyGuard::new(
            Arc::clone(&core),
            id,
            ObservedDirection::Request,
            expected,
        );
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => {
                    if let Ok(bytes) = frame.into_data() {
                        core.request_chunk(id, &bytes);
                        guard.record(bytes.len());
                        yield Ok::<Bytes, hudsucker::Error>(bytes);
                    }
                }
                Err(error) => {
                    core.fail(id, "proxy.request_body", error.to_string());
                    guard.disarm();
                    yield Err(error);
                    return;
                }
            }
        }
        core.end_request(id);
        guard.disarm();
    })
}

fn observed_response_body(core: Arc<Core>, id: ExchangeId, mut body: Body) -> Body {
    let expected = body.size_hint().exact();
    Body::from_stream(async_stream::stream! {
        let mut guard = ObservedBodyGuard::new(
            Arc::clone(&core),
            id,
            ObservedDirection::Response,
            expected,
        );
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => {
                    if let Ok(bytes) = frame.into_data() {
                        core.response_chunk(id, &bytes);
                        guard.record(bytes.len());
                        yield Ok::<Bytes, hudsucker::Error>(bytes);
                    }
                }
                Err(error) => {
                    core.fail(id, "proxy.response_body", error.to_string());
                    guard.disarm();
                    yield Err(error);
                    return;
                }
            }
        }
        core.complete(id);
        guard.disarm();
    })
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
    let stream = async_stream::stream! {
        let mut guard = ExchangeGuard::new(Arc::clone(&core), id);
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
    Body(hudsucker::Error),
}

async fn collect_bounded(
    core: Arc<Core>,
    id: ExchangeId,
    mut body: Body,
    limit: usize,
) -> Result<Bytes, CollectError> {
    let mut collected = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(CollectError::Body)?;
        if let Ok(bytes) = frame.into_data() {
            if collected.len().saturating_add(bytes.len()) > limit {
                return Err(CollectError::TooLarge);
            }
            core.request_chunk(id, &bytes);
            collected.extend_from_slice(&bytes);
        }
    }
    core.end_request(id);
    Ok(Bytes::from(collected))
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
