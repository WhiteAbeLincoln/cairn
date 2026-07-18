//! WebSocket byte-stream adapter for the wRPC frame transport.
//!
//! Bridges a `tokio-websockets` [`WebSocketStream`] to the `AsyncRead` /
//! `AsyncWrite` halves the wRPC frame layer drives, wrapping
//! [`wrpc_websockets::split`] with two behaviors the frame layer needs on a
//! WebSocket that it gets for free on sockets and QUIC streams:
//!
//! ## Eager flushing (write half)
//!
//! wRPC's frame `egress` loop writes each frame with `write_all_buf` and only
//! flushes the underlying writer when the stream is fully shut down. On an
//! unbuffered transport (a raw socket, a QUIC stream) each write reaches the
//! peer regardless, so streaming works without an explicit flush. But
//! `tokio-websockets`' sink *buffers* written messages and only transmits them
//! on `poll_flush` (or once ~8 KiB have accumulated). A streaming response —
//! an `attach` snapshot, then quiet — would therefore sit in the buffer
//! indefinitely, deadlocking the bidirectional stream. [`FlushOnWrite`] flushes
//! after every completed write so each frame reaches the peer immediately.
//!
//! ## Drain on drop (read half)
//!
//! wRPC's frame layer aborts its ingress task as soon as an invocation
//! completes, which can drop the read half *before* the peer's EOF sentinel
//! (an empty WebSocket text frame) has been consumed. If the connection is
//! then closed with those bytes still unread in the kernel receive buffer, the
//! OS answers with TCP RST instead of FIN — and the RST destroys the peer's
//! receive queue, losing an in-flight response ("incomplete results" at the
//! client). [`DrainOnDrop`] catches that case: when the read half is dropped
//! before reaching EOF, it spawns a short, bounded task that reads the stream
//! to EOF so the socket closes cleanly.
//!
//! Exposed at crate root (like [`crate::tls`]) so integration tests can build a
//! matching client-side dialer.

/// The `cairn-mux-v0` frame codec, re-exported for integration tests and
/// client implementations (see the `ws_mux` module docs).
pub use crate::serve::transport::ws_mux::frame as mux;
/// The `/ws` subprotocol names, re-exported so clients offer the exact
/// strings the daemon's negotiation table matches.
pub use crate::serve::transport::ws_mux::{MUX_SUBPROTOCOL, ONESHOT_SUBPROTOCOL};

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite};
use wrpc_websockets::tokio_websockets::WebSocketStream;

/// Upper bound on how long [`DrainOnDrop`]'s cleanup task will keep reading.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
/// Upper bound on how many bytes the drain task will consume before giving up
/// (guards against a peer that keeps streaming after we stopped caring).
const DRAIN_LIMIT: usize = 256 * 1024;

/// An [`AsyncWrite`] wrapper that flushes the inner writer after every completed
/// `poll_write`, so buffered transports transmit each frame promptly.
pub struct FlushOnWrite<W> {
    inner: W,
    /// Byte count from the most recent inner write that still needs flushing
    /// before we report it complete. Tracked so a `Pending` flush is not
    /// mistaken for an incomplete write (which would re-send the same bytes).
    pending_flush: Option<usize>,
}

impl<W> FlushOnWrite<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            pending_flush: None,
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for FlushOnWrite<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            match self.pending_flush {
                Some(n) => {
                    ready!(Pin::new(&mut self.inner).poll_flush(cx))?;
                    self.pending_flush = None;
                    return Poll::Ready(Ok(n));
                }
                None => {
                    let n = ready!(Pin::new(&mut self.inner).poll_write(cx, buf))?;
                    self.pending_flush = Some(n);
                    // Loop: flush before reporting the write complete.
                }
            }
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// An [`AsyncRead`] wrapper that, if dropped before observing EOF or an error,
/// hands the inner reader to a bounded background task that drains it to EOF.
/// See the module docs for why leaving the peer's half-close unread causes a
/// connection reset that can destroy an in-flight response.
pub struct DrainOnDrop<R: AsyncRead + Send + Unpin + 'static> {
    inner: Option<R>,
    /// The stream reached EOF or errored; nothing left worth draining.
    finished: bool,
}

impl<R: AsyncRead + Send + Unpin + 'static> DrainOnDrop<R> {
    fn new(inner: R) -> Self {
        Self {
            inner: Some(inner),
            finished: false,
        }
    }
}

impl<R: AsyncRead + Send + Unpin + 'static> AsyncRead for DrainOnDrop<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(Ok(()));
        };
        let before = buf.filled().len();
        let result = ready!(Pin::new(inner).poll_read(cx, buf));
        match &result {
            // Zero new bytes into spare capacity == EOF.
            Ok(()) if buf.filled().len() == before && buf.remaining() > 0 => {
                this.finished = true;
            }
            Ok(()) => {}
            Err(_) => this.finished = true,
        }
        Poll::Ready(result)
    }
}

impl<R: AsyncRead + Send + Unpin + 'static> Drop for DrainOnDrop<R> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let Some(mut inner) = self.inner.take() else {
            return;
        };
        // No runtime (e.g. dropped after runtime shutdown): nothing we can do,
        // and nothing to lose — the process is tearing the sockets down anyway.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            let _ = tokio::time::timeout(DRAIN_TIMEOUT, async {
                let mut buf = [0u8; 8 * 1024];
                let mut total = 0usize;
                loop {
                    match inner.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            total += n;
                            if total >= DRAIN_LIMIT {
                                break;
                            }
                        }
                    }
                }
            })
            .await;
        });
    }
}

/// Split a [`WebSocketStream`] into wRPC frame byte-stream halves: the write
/// half flushes eagerly, the read half drains itself on early drop. See the
/// module docs for why both behaviors are required.
///
/// The returned halves are `Send + Sync` even when `T` is only `Send`: the
/// `futures` `SplitSink`/`SplitStream` guard the shared stream with a `BiLock`,
/// which is `Sync` given `T: Send`. That is what lets a hyper `Upgraded`
/// (`Send`-only) satisfy the frame `Server`'s `Send + Sync` bounds.
pub fn split<T>(
    ws: WebSocketStream<T>,
) -> (
    impl AsyncWrite + Send + Sync + Unpin + 'static,
    impl AsyncRead + Send + Sync + Unpin + 'static,
)
where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (tx, rx) = wrpc_websockets::split(ws);
    (FlushOnWrite::new(tx), DrainOnDrop::new(rx))
}
