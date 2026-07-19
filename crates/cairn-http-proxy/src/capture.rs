use std::collections::VecDeque;

use bytes::Bytes;

use crate::{ExchangeId, RequestHead, ResponseHead};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedBody {
    pub bytes: Bytes,
    pub total_bytes: u64,
    pub truncated: bool,
    pub complete: bool,
}

impl Default for ObservedBody {
    fn default() -> Self {
        Self {
            bytes: Bytes::new(),
            total_bytes: 0,
            truncated: false,
            complete: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedExchange {
    pub id: ExchangeId,
    pub request: RequestHead,
    pub request_body: ObservedBody,
    pub response: Option<ResponseHead>,
    pub response_body: ObservedBody,
    pub started_at_unix_ms: u64,
    pub completed_at_unix_ms: Option<u64>,
    pub failure: Option<ProxyFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyFailure {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestStarted {
    pub id: ExchangeId,
    pub head: RequestHead,
    pub unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseStarted {
    pub id: ExchangeId,
    pub head: ResponseHead,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyChunk {
    pub id: ExchangeId,
    pub bytes: Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyEnded {
    pub id: ExchangeId,
    pub total_bytes: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExchangeFailed {
    pub id: ExchangeId,
    pub failure: ProxyFailure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObservationEvent {
    Snapshot(Vec<ObservedExchange>),
    RequestStart(RequestStarted),
    RequestBody(BodyChunk),
    RequestEnd(BodyEnded),
    ResponseStart(ResponseStarted),
    ResponseBody(BodyChunk),
    ResponseEnd(BodyEnded),
    Completed(ExchangeId, u64),
    Failed(ExchangeFailed),
}

pub(crate) struct CaptureStore {
    exchanges: VecDeque<ObservedExchange>,
    max_exchanges: usize,
    max_bytes: usize,
    body_limit: usize,
}

impl CaptureStore {
    pub(crate) fn new(max_exchanges: usize, max_bytes: usize, body_limit: usize) -> Self {
        Self {
            exchanges: VecDeque::new(),
            max_exchanges,
            max_bytes,
            body_limit,
        }
    }

    pub(crate) fn snapshot(&self) -> Vec<ObservedExchange> {
        self.exchanges.iter().cloned().collect()
    }

    pub(crate) fn start(&mut self, id: ExchangeId, head: RequestHead, unix_ms: u64) {
        self.exchanges.push_back(ObservedExchange {
            id,
            request: head,
            request_body: ObservedBody::default(),
            response: None,
            response_body: ObservedBody::default(),
            started_at_unix_ms: unix_ms,
            completed_at_unix_ms: None,
            failure: None,
        });
        self.evict();
    }

    pub(crate) fn request_chunk(&mut self, id: ExchangeId, bytes: &Bytes) -> Option<Bytes> {
        let body_limit = self.body_limit;
        let exchange = self.find_mut(id)?;
        capture_chunk(&mut exchange.request_body, bytes, body_limit)
    }

    pub(crate) fn end_request(&mut self, id: ExchangeId) -> Option<BodyEnded> {
        let exchange = self.find_mut(id)?;
        exchange.request_body.complete = true;
        Some(BodyEnded {
            id,
            total_bytes: exchange.request_body.total_bytes,
            truncated: exchange.request_body.truncated,
        })
    }

    pub(crate) fn start_response(&mut self, id: ExchangeId, head: ResponseHead) {
        if let Some(exchange) = self.find_mut(id) {
            exchange.response = Some(head);
        }
    }

    pub(crate) fn response_chunk(&mut self, id: ExchangeId, bytes: &Bytes) -> Option<Bytes> {
        let body_limit = self.body_limit;
        let exchange = self.find_mut(id)?;
        capture_chunk(&mut exchange.response_body, bytes, body_limit)
    }

    pub(crate) fn end_response(&mut self, id: ExchangeId) -> Option<BodyEnded> {
        let exchange = self.find_mut(id)?;
        exchange.response_body.complete = true;
        Some(BodyEnded {
            id,
            total_bytes: exchange.response_body.total_bytes,
            truncated: exchange.response_body.truncated,
        })
    }

    pub(crate) fn complete(&mut self, id: ExchangeId, unix_ms: u64) {
        if let Some(exchange) = self.find_mut(id) {
            exchange.completed_at_unix_ms = Some(unix_ms);
        }
        self.evict();
    }

    pub(crate) fn fail(&mut self, id: ExchangeId, failure: ProxyFailure) {
        if let Some(exchange) = self.find_mut(id) {
            exchange.failure = Some(failure);
            exchange.completed_at_unix_ms = Some(crate::now_unix_ms());
        }
        self.evict();
    }

    fn find_mut(&mut self, id: ExchangeId) -> Option<&mut ObservedExchange> {
        self.exchanges.iter_mut().find(|exchange| exchange.id == id)
    }

    fn evict(&mut self) {
        while self.exchanges.len() > self.max_exchanges || self.captured_bytes() > self.max_bytes {
            let Some(index) = self
                .exchanges
                .iter()
                .position(|exchange| exchange.completed_at_unix_ms.is_some())
            else {
                break;
            };
            self.exchanges.remove(index);
        }
    }

    fn captured_bytes(&self) -> usize {
        self.exchanges
            .iter()
            .map(|exchange| exchange.request_body.bytes.len() + exchange.response_body.bytes.len())
            .sum()
    }
}

fn capture_chunk(body: &mut ObservedBody, bytes: &Bytes, limit: usize) -> Option<Bytes> {
    body.total_bytes = body.total_bytes.saturating_add(bytes.len() as u64);
    let remaining = limit.saturating_sub(body.bytes.len());
    if remaining == 0 {
        body.truncated |= !bytes.is_empty();
        return None;
    }
    let captured = bytes.slice(..bytes.len().min(remaining));
    let mut combined = Vec::with_capacity(body.bytes.len() + captured.len());
    combined.extend_from_slice(&body.bytes);
    combined.extend_from_slice(&captured);
    body.bytes = Bytes::from(combined);
    body.truncated |= captured.len() < bytes.len();
    Some(captured)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(path: &str) -> RequestHead {
        RequestHead {
            method: "GET".into(),
            uri: format!("http://example.com{path}"),
            version: "HTTP/1.1".into(),
            headers: vec![],
        }
    }

    #[test]
    fn capture_reports_total_bytes_and_truncation() {
        let mut store = CaptureStore::new(8, 1024, 4);
        store.start(1, head("/"), 10);
        assert_eq!(
            store.request_chunk(1, &Bytes::from_static(b"abcdef")),
            Some(Bytes::from_static(b"abcd"))
        );
        let end = store.end_request(1).unwrap();
        assert_eq!(end.total_bytes, 6);
        assert!(end.truncated);
        let snapshot = store.snapshot();
        assert_eq!(snapshot[0].request_body.bytes, Bytes::from_static(b"abcd"));
    }

    #[test]
    fn replay_evicts_oldest_completed_exchange_first() {
        let mut store = CaptureStore::new(2, 1024, 32);
        store.start(1, head("/one"), 1);
        store.complete(1, 2);
        store.start(2, head("/two"), 3);
        store.start(3, head("/three"), 4);
        let ids: Vec<_> = store
            .snapshot()
            .into_iter()
            .map(|exchange| exchange.id)
            .collect();
        assert_eq!(ids, vec![2, 3]);
    }
}
