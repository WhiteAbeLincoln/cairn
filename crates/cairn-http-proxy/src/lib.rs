//! HTTP/HTTPS interception backend shared by Cairn's wire and future plugin adapters.

mod authority;
mod capture;
mod proxy;
mod route;

pub use authority::ProxyAuthority;
pub use capture::{
    BodyChunk, BodyEnded, ExchangeFailed, ObservationEvent, ObservedBody, ObservedExchange,
    ProxyFailure, RequestStarted, ResponseStarted,
};
pub use proxy::{
    InterceptorAction, InterceptorAttachment, InterceptorEvent, ProxyError, ProxySession,
    ProxySessionConfig, ResponseStart,
};
pub use route::Route;

use bytes::Bytes;

pub type ExchangeId = u64;
pub type Header = (String, Bytes);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestHead {
    pub method: String,
    pub uri: String,
    pub version: String,
    pub headers: Vec<Header>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseHead {
    pub status: u16,
    pub version: String,
    pub headers: Vec<Header>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterceptedRequest {
    pub id: ExchangeId,
    pub head: RequestHead,
    pub body: Bytes,
}

pub(crate) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
