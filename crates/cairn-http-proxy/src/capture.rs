use std::collections::{HashMap, VecDeque};

use bytes::{Bytes, BytesMut};

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

/// Mutable, internal representation of a body's captured bytes while its
/// exchange is tracked by the store. Bytes accumulate in a `BytesMut`, which
/// grows/reuses its backing allocation on `extend_from_slice` (amortized
/// O(1) per append), instead of the old approach of reallocating and
/// `memcpy`-ing the *entire* captured-so-far buffer on every chunk (O(n)
/// per chunk, O(n^2) over a whole body). It is only frozen into the public,
/// immutable `ObservedBody` (backed by `Bytes`) when a snapshot is taken.
#[derive(Debug, Default)]
struct CapturedBody {
    buf: BytesMut,
    total_bytes: u64,
    truncated: bool,
    complete: bool,
}

impl CapturedBody {
    fn to_observed(&self) -> ObservedBody {
        ObservedBody {
            bytes: Bytes::copy_from_slice(&self.buf),
            total_bytes: self.total_bytes,
            truncated: self.truncated,
            complete: self.complete,
        }
    }
}

/// Mutable, internal representation of a tracked exchange. Mirrors
/// `ObservedExchange` field-for-field except that bodies are the
/// `BytesMut`-backed `CapturedBody` rather than the public, `Bytes`-backed
/// `ObservedBody`; `to_observed` performs the (only-here) conversion.
struct CapturedExchange {
    id: ExchangeId,
    request: RequestHead,
    request_body: CapturedBody,
    response: Option<ResponseHead>,
    response_body: CapturedBody,
    started_at_unix_ms: u64,
    completed_at_unix_ms: Option<u64>,
    failure: Option<ProxyFailure>,
}

impl CapturedExchange {
    fn to_observed(&self) -> ObservedExchange {
        ObservedExchange {
            id: self.id,
            request: self.request.clone(),
            request_body: self.request_body.to_observed(),
            response: self.response.clone(),
            response_body: self.response_body.to_observed(),
            started_at_unix_ms: self.started_at_unix_ms,
            completed_at_unix_ms: self.completed_at_unix_ms,
            failure: self.failure.clone(),
        }
    }

    /// Bytes currently buffered across both bodies of this exchange, used to
    /// keep the store's running `captured_total` in sync when the whole
    /// record is evicted.
    fn buffered_len(&self) -> usize {
        self.request_body.buf.len() + self.response_body.buf.len()
    }
}

pub(crate) struct CaptureStore {
    /// Exchange ids in insertion order, oldest first. Drives eviction order
    /// and `snapshot()`'s output order; the records themselves live in
    /// `exchanges`, keyed by id.
    order: VecDeque<ExchangeId>,
    /// Exchange records keyed by id. Using a map (rather than the previous
    /// linear `VecDeque<ObservedExchange>`) makes `find_mut`'s lookup --
    /// taken on every byte written by every concurrent request -- O(1)
    /// instead of an O(n) scan.
    exchanges: HashMap<ExchangeId, CapturedExchange>,
    max_exchanges: usize,
    max_bytes: usize,
    body_limit: usize,
    /// Running total of bytes currently captured (request + response body
    /// bytes) across every exchange in `exchanges`. Maintained incrementally
    /// on every mutation so checking it against `max_bytes` is O(1) instead
    /// of re-summing every exchange's body length on every `evict` loop
    /// iteration (which made eviction of several exchanges O(n^2)).
    captured_total: usize,
}

impl CaptureStore {
    pub(crate) fn new(max_exchanges: usize, max_bytes: usize, body_limit: usize) -> Self {
        Self {
            order: VecDeque::new(),
            exchanges: HashMap::new(),
            max_exchanges,
            max_bytes,
            body_limit,
            captured_total: 0,
        }
    }

    pub(crate) fn snapshot(&self) -> Vec<ObservedExchange> {
        self.order
            .iter()
            .filter_map(|id| self.exchanges.get(id))
            .map(CapturedExchange::to_observed)
            .collect()
    }

    pub(crate) fn start(&mut self, id: ExchangeId, head: RequestHead, unix_ms: u64) {
        self.order.push_back(id);
        self.exchanges.insert(
            id,
            CapturedExchange {
                id,
                request: head,
                request_body: CapturedBody::default(),
                response: None,
                response_body: CapturedBody::default(),
                started_at_unix_ms: unix_ms,
                completed_at_unix_ms: None,
                failure: None,
            },
        );
        self.evict();
    }

    pub(crate) fn request_chunk(&mut self, id: ExchangeId, bytes: &Bytes) -> Option<Bytes> {
        let local_limit = self.body_limit;
        let global_budget = self.max_bytes.saturating_sub(self.captured_total);
        let exchange = self.find_mut(id)?;
        let (captured, added) = capture_chunk(
            &mut exchange.request_body,
            bytes,
            local_limit,
            global_budget,
        );
        self.captured_total += added;
        captured
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
        let local_limit = self.body_limit;
        let global_budget = self.max_bytes.saturating_sub(self.captured_total);
        let exchange = self.find_mut(id)?;
        let (captured, added) = capture_chunk(
            &mut exchange.response_body,
            bytes,
            local_limit,
            global_budget,
        );
        self.captured_total += added;
        captured
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

    fn find_mut(&mut self, id: ExchangeId) -> Option<&mut CapturedExchange> {
        self.exchanges.get_mut(&id)
    }

    /// Evicts completed exchanges, oldest first, while the store is over
    /// either cap: `max_exchanges` records, or `max_bytes` of captured body
    /// content.
    ///
    /// Policy for the byte cap (this is the fix for the memory-bound bug: an
    /// all-active workload -- nothing ever completes -- used to retain
    /// unbounded captured bytes because this loop could only reclaim
    /// *completed* exchanges): this loop still never removes an in-flight
    /// exchange's record outright, so a slow/hung request doesn't vanish
    /// from the observable history. Instead, `max_bytes` is enforced where
    /// the bytes are actually produced -- see `request_chunk`/
    /// `response_chunk`'s `global_budget` computation, passed into
    /// `capture_chunk` -- which stops capturing further body content (and
    /// marks the body `truncated`) the instant the store-wide cap would be
    /// exceeded, regardless of how many exchanges are active concurrently.
    /// That write-time gate is what makes `captured_total <= max_bytes` an
    /// invariant; the check in this loop is a defensive backstop, and what
    /// promptly reclaims memory from *historical* completed exchanges once
    /// the exchange-count cap alone wouldn't have.
    fn evict(&mut self) {
        while self.exchanges.len() > self.max_exchanges || self.captured_total > self.max_bytes {
            let Some(index) = self.order.iter().position(|id| {
                self.exchanges
                    .get(id)
                    .is_some_and(|exchange| exchange.completed_at_unix_ms.is_some())
            }) else {
                break;
            };
            let Some(id) = self.order.remove(index) else {
                break;
            };
            if let Some(exchange) = self.exchanges.remove(&id) {
                self.captured_total -= exchange.buffered_len();
            }
        }
    }
}

/// Appends `bytes` to `body`'s captured buffer, subject to two independent
/// caps:
/// - `local_limit` (`capture_body_bytes`): the max bytes captured for this
///   one body (request or response).
/// - `global_budget`: bytes remaining under the store-wide `max_bytes`
///   (`replay_max_bytes`) cap, shared across every body of every exchange
///   currently held. This is what keeps memory bounded even when every
///   exchange is still in flight and `evict` cannot reclaim anything by
///   dropping records.
///
/// Returns the slice actually captured (forwarded on to the peer) and the
/// number of bytes added to `body`'s buffer, so the caller can keep its
/// running store-wide total (`captured_total`) in sync in O(1) instead of
/// re-summing every body's length.
fn capture_chunk(
    body: &mut CapturedBody,
    bytes: &Bytes,
    local_limit: usize,
    global_budget: usize,
) -> (Option<Bytes>, usize) {
    body.total_bytes = body.total_bytes.saturating_add(bytes.len() as u64);
    let local_remaining = local_limit.saturating_sub(body.buf.len());
    let allowed = local_remaining.min(global_budget);
    if allowed == 0 {
        body.truncated |= !bytes.is_empty();
        return (None, 0);
    }
    let captured = bytes.slice(..bytes.len().min(allowed));
    body.buf.extend_from_slice(&captured);
    body.truncated |= captured.len() < bytes.len();
    let added = captured.len();
    (Some(captured), added)
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
    fn capture_bounds_total_bytes_across_many_active_exchanges() {
        // Regression test for the memory-bound bug: an all-active workload
        // (nothing ever completes, so `evict` has nothing it can reclaim)
        // must still be capped by `max_bytes` across every exchange's
        // captured body bytes combined, not just per-exchange/per-body.
        let max_bytes = 1024;
        // body_limit is intentionally far larger than max_bytes so the
        // per-body cap never binds first -- only the store-wide budget can
        // be the thing that stops capture here.
        let mut store = CaptureStore::new(1000, max_bytes, 4096);
        let chunk = Bytes::from(vec![b'x'; 100]);
        for id in 0..50u64 {
            store.start(id, head("/"), id);
            // Never call complete()/fail() -- every exchange stays active.
            for _ in 0..3 {
                store.request_chunk(id, &chunk);
            }
        }

        let total: usize = store
            .snapshot()
            .iter()
            .map(|exchange| exchange.request_body.bytes.len() + exchange.response_body.bytes.len())
            .sum();

        assert!(
            total <= max_bytes,
            "captured {total} bytes but max_bytes is {max_bytes}"
        );
        assert_eq!(total, max_bytes, "budget should be filled exactly");
    }

    #[test]
    fn capture_chunk_accumulates_exact_prefix_across_many_chunks() {
        // Correctness guard for the BytesMut-accumulator refactor: writing
        // many small chunks must produce exactly the same captured prefix
        // (and truncation behavior) as writing one big chunk would, proving
        // the amortized-append path doesn't drop, reorder, or duplicate
        // bytes at chunk boundaries.
        let mut store = CaptureStore::new(8, 1024, 10);
        store.start(1, head("/"), 0);

        let chunks: [&[u8]; 6] = [b"ab", b"cd", b"ef", b"gh", b"ij", b"kl"];
        let mut returned = Vec::new();
        for chunk in chunks {
            if let Some(captured) = store.request_chunk(1, &Bytes::copy_from_slice(chunk)) {
                returned.extend_from_slice(&captured);
            }
        }

        // body_limit is 10: the first five 2-byte chunks (10 bytes) are
        // captured in full; the sixth is entirely past the limit.
        assert_eq!(returned, b"abcdefghij");

        let end = store.end_request(1).unwrap();
        assert_eq!(end.total_bytes, 12);
        assert!(end.truncated);

        let snapshot = store.snapshot();
        assert_eq!(
            snapshot[0].request_body.bytes,
            Bytes::from_static(b"abcdefghij")
        );
        assert_eq!(snapshot[0].request_body.total_bytes, 12);
        assert!(snapshot[0].request_body.truncated);
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
