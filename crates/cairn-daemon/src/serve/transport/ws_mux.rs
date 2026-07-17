//! Muxed WebSocket transport (`cairn-mux-v0`): one persistent WebSocket
//! carrying many concurrent wRPC invocations, each on its own logical channel.
//!
//! wRPC's wire format deliberately has no invocation ID — the byte-stream
//! boundary *is* the invocation identity (see the design spec,
//! `docs/superpowers/specs/2026-07-17-ws-mux-design.md`). This module supplies
//! the missing layer for WebSockets: every WS binary message is one mux frame,
//! `[channel_id: u32 BE][flags: u8][payload]`, and each client-opened channel
//! is a logical byte stream carrying exactly one invocation — the same shape
//! QUIC gives WebTransport via `open_bi()`.
//!
//! ## Structure
//!
//! - One **reader task** parses frames, routes payloads into per-channel
//!   bounded buffers, opens channels (id greater than any seen), and enforces
//!   protocol limits. A full channel buffer suspends the whole read loop:
//!   flow control is socket-level by design (the mux carries small control
//!   traffic; bulk streams use dedicated one-shot sockets).
//! - One **writer task** is the sole sink consumer: data/FIN frames arrive in
//!   order on a bounded queue, RST and connection-close jump it via an
//!   unbounded control queue (aborts may overtake data; FIN must not, so FIN
//!   rides the data queue). It flushes after every message (same rationale as
//!   [`crate::ws::FlushOnWrite`]) and owns the keepalive ping interval.
//! - [`MuxReader`]/[`MuxWriter`] are the per-channel logical stream halves
//!   handed to the wRPC frame server through [`MuxAcceptor`] — the repeating
//!   sibling of the one-shot path's `OneShot` acceptor.
//!
//! ## Channel lifecycle
//!
//! A channel closes cleanly when both sides have sent FIN, and abruptly when
//! either sends RST. Frames for a non-live channel id at or below the highest
//! seen are silently ignored — RST and in-flight data legitimately cross on
//! the wire. Only malformed traffic (reserved flag bits, text frames,
//! oversized frames, channel id 0) is a protocol violation, which tears down
//! the connection.

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll, ready};
use std::time::Duration;

use bytes::Bytes;
use futures::{Sink, SinkExt as _, Stream, StreamExt as _};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio_util::sync::{CancellationToken, PollSender};
use wrpc_websockets::tokio_websockets::{CloseCode, Message};

use crate::serve::ConnCtx;

/// Inbound frames buffered per channel before the read loop suspends
/// (whole-socket pause = TCP backpressure, per the design spec).
const INBOUND_BUFFER_FRAMES: usize = 16;
/// Outbound data frames buffered across all channels before writers block.
const OUTBOUND_QUEUE_FRAMES: usize = 64;
/// Keepalive ping cadence: keeps NAT/proxy paths warm and detects dead
/// clients (browsers auto-pong; `tokio-websockets` auto-replies likewise).
const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Wire framing for `cairn-mux-v0`. Shared with the integration-test client
/// so the codec is not duplicated (`pub(crate)` for that reason).
pub(crate) mod frame {
    use bytes::{Buf as _, BufMut as _, Bytes, BytesMut};

    /// `[channel_id: u32 BE][flags: u8]`.
    pub(crate) const HEADER_LEN: usize = 5;
    /// Sender's write side of this channel is done after this frame's payload.
    pub(crate) const FLAG_FIN: u8 = 1;
    /// Channel aborted; both directions dead. Payload ignored.
    pub(crate) const FLAG_RST: u8 = 1 << 1;
    const KNOWN_FLAGS: u8 = FLAG_FIN | FLAG_RST;
    /// Maximum frame payload; larger is a protocol violation.
    pub(crate) const MAX_PAYLOAD: usize = 1 << 20;
    /// Maximum concurrent channels; the daemon RSTs channels beyond this
    /// rather than killing the socket.
    pub(crate) const MAX_CHANNELS: usize = 256;

    pub(crate) fn encode(channel: u32, flags: u8, payload: &[u8]) -> Bytes {
        debug_assert!(payload.len() <= MAX_PAYLOAD);
        let mut buf = BytesMut::with_capacity(HEADER_LEN + payload.len());
        buf.put_u32(channel);
        buf.put_u8(flags);
        buf.put_slice(payload);
        buf.freeze()
    }

    /// Malformed traffic that tears down the whole connection (everything
    /// else is contained to its channel).
    #[derive(Debug, PartialEq, Eq)]
    pub(crate) enum Violation {
        TooShort(usize),
        ReservedFlags(u8),
        ChannelZero,
        Oversized(usize),
    }

    impl std::fmt::Display for Violation {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::TooShort(len) => write!(f, "frame shorter than header ({len} bytes)"),
                Self::ReservedFlags(flags) => write!(f, "reserved flag bits set ({flags:#04x})"),
                Self::ChannelZero => write!(f, "channel id 0 is reserved"),
                Self::Oversized(len) => write!(f, "payload exceeds maximum ({len} bytes)"),
            }
        }
    }

    pub(crate) fn decode(mut msg: Bytes) -> Result<(u32, u8, Bytes), Violation> {
        if msg.len() < HEADER_LEN {
            return Err(Violation::TooShort(msg.len()));
        }
        if msg.len() - HEADER_LEN > MAX_PAYLOAD {
            return Err(Violation::Oversized(msg.len() - HEADER_LEN));
        }
        let channel = msg.get_u32();
        let flags = msg.get_u8();
        if flags & !KNOWN_FLAGS != 0 {
            return Err(Violation::ReservedFlags(flags));
        }
        if channel == 0 {
            return Err(Violation::ChannelZero);
        }
        Ok((channel, flags, msg))
    }
}

/// Control frames that jump the ordered data queue (see module docs for why
/// RST may overtake data but FIN must not).
enum Ctl {
    Rst(u32),
    Close { code: CloseCode, reason: String },
}

#[derive(Default)]
struct ChannelMap {
    /// Highest channel id ever seen; ids at or below this that are not live
    /// are stale (ignored), ids above it open a new channel.
    highest: u32,
    live: HashMap<u32, Entry>,
}

struct Entry {
    /// Feed to the channel's [`MuxReader`]; `None` once the remote FIN'd or
    /// the local reader was dropped (late data is then discarded).
    inbound: Option<mpsc::Sender<Bytes>>,
    /// Error the [`MuxReader`] reports when its feed ends without a clean FIN.
    err: Arc<Mutex<Option<io::Error>>>,
    remote_done: bool,
    local_done: bool,
}

struct Shared {
    channels: Mutex<ChannelMap>,
    ctl: mpsc::UnboundedSender<Ctl>,
}

/// All `channels` locks recover from poisoning: the map is only mutated
/// under short, panic-free critical sections, so the inner value is always
/// well-formed (same pattern as the one-shot path's `OneShot`).
fn lock_channels(shared: &Shared) -> std::sync::MutexGuard<'_, ChannelMap> {
    shared.channels.lock().unwrap_or_else(|p| p.into_inner())
}

fn set_err(entry_err: &Mutex<Option<io::Error>>, error: io::Error) {
    let mut slot = entry_err.lock().unwrap_or_else(|p| p.into_inner());
    // First error wins; a later connection-level error must not mask the
    // channel-level one the reader is about to observe.
    slot.get_or_insert(error);
}

impl Shared {
    fn is_live(&self, id: u32) -> bool {
        lock_channels(self).live.contains_key(&id)
    }

    /// Local write side finished cleanly (FIN sent).
    fn local_done(&self, id: u32) {
        let mut map = lock_channels(self);
        if let Some(entry) = map.live.get_mut(&id) {
            entry.local_done = true;
            if entry.remote_done {
                map.live.remove(&id);
            }
        }
    }

    /// Remote write side finished cleanly (FIN received).
    fn remote_done(&self, id: u32) {
        let mut map = lock_channels(self);
        if let Some(entry) = map.live.get_mut(&id) {
            entry.remote_done = true;
            entry.inbound = None; // sender drop = EOF after buffered payloads drain
            if entry.local_done {
                map.live.remove(&id);
            }
        }
    }

    /// The channel's local reader was dropped; discard late inbound data but
    /// keep the channel alive for the writer half.
    fn reader_gone(&self, id: u32) {
        let mut map = lock_channels(self);
        if let Some(entry) = map.live.get_mut(&id) {
            entry.inbound = None;
        }
    }

    /// Remote aborted the channel.
    fn remote_rst(&self, id: u32) {
        let mut map = lock_channels(self);
        if let Some(entry) = map.live.remove(&id) {
            set_err(
                &entry.err,
                io::Error::new(io::ErrorKind::ConnectionReset, "channel reset by peer"),
            );
        }
    }

    /// Local writer dropped without FIN: abort the channel and tell the peer.
    fn local_rst(&self, id: u32) {
        let removed = {
            let mut map = lock_channels(self);
            map.live.remove(&id)
        };
        if let Some(entry) = removed {
            set_err(
                &entry.err,
                io::Error::new(io::ErrorKind::ConnectionReset, "channel reset locally"),
            );
            let _ = self.ctl.send(Ctl::Rst(id));
        }
    }

    /// Connection died: every live channel errors out together.
    fn fail_all(&self, kind: io::ErrorKind, reason: &str) {
        let mut map = lock_channels(self);
        for (_, entry) in map.live.drain() {
            set_err(&entry.err, io::Error::new(kind, reason.to_string()));
        }
    }
}

/// Repeating [`Accept`] source for the wRPC frame server: yields one
/// `(ConnCtx, MuxWriter, MuxReader)` triple per client-opened channel.
///
/// [`Accept`]: wrpc_transport::frame::Accept
pub(crate) struct MuxAcceptor {
    rx: flume::Receiver<(ConnCtx, MuxWriter, MuxReader)>,
}

impl wrpc_transport::frame::Accept for &MuxAcceptor {
    type Context = ConnCtx;
    type Outgoing = MuxWriter;
    type Incoming = MuxReader;

    async fn accept(&self) -> io::Result<(Self::Context, Self::Outgoing, Self::Incoming)> {
        match self.rx.recv_async().await {
            Ok(chan) => Ok(chan),
            // Mux ended: park forever (like the one-shot `OneShot` after its
            // single yield) — the caller's cancellation token, cancelled by
            // the reader task on connection death, ends the serve loop.
            Err(_) => std::future::pending().await,
        }
    }
}

/// Spawn the reader/writer tasks for one muxed connection and return the
/// acceptor feeding its channels to a wRPC frame server.
///
/// Generic over the WebSocket stream so tests can drive it with an in-memory
/// `Sink`/`Stream` of [`Message`]s instead of a real socket. `cancel` stops
/// both tasks; the reader also cancels it when the connection dies, so the
/// caller's serve loop observes a single stop signal either way.
pub(crate) fn start_mux<S, E>(ctx: ConnCtx, ws: S, cancel: CancellationToken) -> MuxAcceptor
where
    S: Stream<Item = Result<Message, E>> + Sink<Message, Error = E> + Send + Unpin + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let (sink, stream) = ws.split();
    let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
    let (data_tx, data_rx) = mpsc::channel::<Bytes>(OUTBOUND_QUEUE_FRAMES);
    let (accept_tx, accept_rx) = flume::bounded(frame::MAX_CHANNELS);
    let shared = Arc::new(Shared {
        channels: Mutex::new(ChannelMap::default()),
        ctl: ctl_tx,
    });

    tokio::spawn(write_loop(sink, data_rx, ctl_rx, cancel.clone()));
    tokio::spawn(async move {
        read_loop(ctx, stream, &shared, data_tx, accept_tx, &cancel).await;
        // Connection over (EOF, error, violation, or shutdown): stop the
        // writer and the serve loop parked on the (now closed) acceptor.
        cancel.cancel();
    });

    MuxAcceptor { rx: accept_rx }
}

/// Serve wRPC invocations over one muxed WebSocket connection until it closes
/// or `shutdown` fires. The muxed sibling of the one-shot path's `serve_one`.
#[expect(
    dead_code,
    reason = "wired into the /ws upgrade path by the negotiation change"
)]
pub(crate) async fn serve_mux<S, E>(
    ctx: ConnCtx,
    ws: S,
    daemon: crate::daemon::Daemon,
    shutdown: CancellationToken,
) -> anyhow::Result<()>
where
    S: Stream<Item = Result<Message, E>> + Sink<Message, Error = E> + Send + Unpin + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let cancel = shutdown.child_token();
    let acceptor = start_mux(ctx, ws, cancel.clone());
    crate::serve::wrpc::run_wrpc_server::<_, MuxReader, MuxWriter, ()>(acceptor, daemon, cancel)
        .await
}

/// Sole consumer of the WebSocket sink: serializes data frames, control
/// frames (which jump the data queue), and keepalive pings, flushing after
/// every message so streamed responses reach the peer promptly.
async fn write_loop<Snk, E>(
    mut sink: Snk,
    mut data: mpsc::Receiver<Bytes>,
    mut ctl: mpsc::UnboundedReceiver<Ctl>,
    cancel: CancellationToken,
) where
    Snk: Sink<Message, Error = E> + Unpin,
    E: std::fmt::Display,
{
    let mut ping =
        tokio::time::interval_at(tokio::time::Instant::now() + PING_INTERVAL, PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        // Compute the message in the select arms, send once below (single
        // await point). `biased` gives aborts/closes priority over data.
        let msg = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            Some(c) = ctl.recv() => match c {
                Ctl::Rst(id) => Message::binary(frame::encode(id, frame::FLAG_RST, &[])),
                Ctl::Close { code, reason } => {
                    if let Err(error) = sink.send(Message::close(Some(code), &reason)).await {
                        tracing::debug!(%error, "mux close-frame write failed");
                    }
                    break;
                }
            },
            Some(bytes) = data.recv() => Message::binary(bytes),
            _ = ping.tick() => Message::ping(Bytes::new()),
        };
        if let Err(error) = sink.send(msg).await {
            tracing::debug!(%error, "mux write failed; closing connection");
            break;
        }
    }
    // Best-effort clean close on the way out (no-op if transport is gone).
    let _ = sink.send(Message::close(None, "")).await;
}

/// Reads frames off the socket and routes them to channels until the
/// connection ends. Returning (for any reason) means the connection is dead;
/// the caller cancels the shared token.
async fn read_loop<St, E>(
    ctx: ConnCtx,
    mut stream: St,
    shared: &Arc<Shared>,
    data_tx: mpsc::Sender<Bytes>,
    accept_tx: flume::Sender<(ConnCtx, MuxWriter, MuxReader)>,
    cancel: &CancellationToken,
) where
    St: Stream<Item = Result<Message, E>> + Unpin,
    E: std::fmt::Display,
{
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                shared.fail_all(io::ErrorKind::Interrupted, "daemon shutting down");
                return;
            }
            next = stream.next() => next,
        };
        let msg = match next {
            Some(Ok(msg)) => msg,
            Some(Err(error)) => {
                shared.fail_all(io::ErrorKind::ConnectionReset, &error.to_string());
                return;
            }
            None => break, // peer went away without a close frame
        };

        if msg.is_binary() {
            match frame::decode(Bytes::from(msg.into_payload())) {
                Ok((id, flags, payload)) => {
                    handle_frame(&ctx, shared, &data_tx, &accept_tx, id, flags, payload).await;
                }
                Err(violation) => {
                    protocol_error(shared, &violation.to_string());
                    return;
                }
            }
        } else if msg.is_text() {
            protocol_error(shared, "text frame in muxed mode");
            return;
        } else if msg.is_close() {
            break;
        }
        // Pings/pongs are handled by tokio-websockets itself; skip.
    }
    shared.fail_all(
        io::ErrorKind::ConnectionReset,
        "WebSocket connection closed",
    );
}

fn protocol_error(shared: &Shared, reason: &str) {
    tracing::debug!(%reason, "mux protocol violation; closing connection");
    let _ = shared.ctl.send(Ctl::Close {
        code: CloseCode::PROTOCOL_ERROR,
        reason: reason.to_string(),
    });
    shared.fail_all(io::ErrorKind::InvalidData, reason);
}

/// What to do with a decoded frame — decided under the channel-map lock,
/// acted on after it is released (never await while holding it).
enum Action {
    Open {
        in_tx: mpsc::Sender<Bytes>,
        in_rx: mpsc::Receiver<Bytes>,
        err: Arc<Mutex<Option<io::Error>>>,
        remote_done: bool,
    },
    Deliver {
        tx: Option<mpsc::Sender<Bytes>>,
        fin: bool,
    },
    RstOverLimit,
    Ignore,
}

async fn handle_frame(
    ctx: &ConnCtx,
    shared: &Arc<Shared>,
    data_tx: &mpsc::Sender<Bytes>,
    accept_tx: &flume::Sender<(ConnCtx, MuxWriter, MuxReader)>,
    id: u32,
    flags: u8,
    payload: Bytes,
) {
    let fin = flags & frame::FLAG_FIN != 0;
    let rst = flags & frame::FLAG_RST != 0;

    let action = {
        let mut map = lock_channels(shared);
        if id > map.highest {
            map.highest = id;
            if rst {
                // Opened-and-aborted in one frame: nothing ever existed.
                Action::Ignore
            } else if map.live.len() >= frame::MAX_CHANNELS {
                Action::RstOverLimit
            } else {
                let (in_tx, in_rx) = mpsc::channel(INBOUND_BUFFER_FRAMES);
                let err = Arc::new(Mutex::new(None));
                map.live.insert(
                    id,
                    Entry {
                        inbound: (!fin).then(|| in_tx.clone()),
                        err: err.clone(),
                        remote_done: fin,
                        local_done: false,
                    },
                );
                Action::Open {
                    in_tx,
                    in_rx,
                    err,
                    remote_done: fin,
                }
            }
        } else if let Some(entry) = map.live.get(&id) {
            if rst {
                // Handled outside the lock via remote_rst (needs removal).
                Action::Deliver {
                    tx: None,
                    fin: false,
                }
            } else {
                Action::Deliver {
                    tx: entry.inbound.clone(),
                    fin,
                }
            }
        } else {
            // Stale: RST raced data, or the channel is long gone. Silence.
            Action::Ignore
        }
    };

    if rst && matches!(action, Action::Deliver { .. }) {
        shared.remote_rst(id);
        return;
    }

    match action {
        Action::Open {
            in_tx,
            in_rx,
            err,
            remote_done,
        } => {
            if !payload.is_empty() {
                // Fresh channel with capacity; cannot meaningfully fail.
                let _ = in_tx.send(payload).await;
            }
            drop(in_tx); // entry holds the feed (unless FIN'd: EOF after payload)
            let reader = MuxReader {
                rx: in_rx,
                err,
                chunk: Bytes::new(),
                done: false,
            };
            let writer = MuxWriter {
                id,
                data: PollSender::new(data_tx.clone()),
                shared: shared.clone(),
                fin_sent: false,
            };
            if remote_done {
                shared.remote_done(id);
            }
            // Full only if the serve loop stopped accepting; drop then.
            let _ = accept_tx.send_async((ctx.clone(), writer, reader)).await;
        }
        Action::Deliver { tx, fin } => {
            if let Some(tx) = tx {
                // Awaiting here while a channel's buffer is full suspends the
                // whole read loop: deliberate whole-socket backpressure.
                if !payload.is_empty() && tx.send(payload).await.is_err() {
                    shared.reader_gone(id);
                }
            }
            if fin {
                shared.remote_done(id);
            }
        }
        Action::RstOverLimit => {
            tracing::debug!(channel = id, "mux channel limit exceeded; resetting");
            let _ = shared.ctl.send(Ctl::Rst(id));
        }
        Action::Ignore => {}
    }
}

/// Logical read half of one mux channel: yields the channel's inbound
/// payloads, EOF on the peer's FIN, or an error on RST/connection death.
pub(crate) struct MuxReader {
    rx: mpsc::Receiver<Bytes>,
    err: Arc<Mutex<Option<io::Error>>>,
    /// Remainder of a payload larger than the caller's read buffer.
    chunk: Bytes,
    done: bool,
}

impl AsyncRead for MuxReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            if !self.chunk.is_empty() {
                let n = self.chunk.len().min(buf.remaining());
                buf.put_slice(&self.chunk.split_to(n));
                return Poll::Ready(Ok(()));
            }
            if self.done {
                return Poll::Ready(Ok(())); // sticky EOF
            }
            match ready!(self.rx.poll_recv(cx)) {
                Some(bytes) => self.chunk = bytes,
                None => {
                    self.done = true;
                    let err = {
                        let mut slot = self.err.lock().unwrap_or_else(|p| p.into_inner());
                        slot.take()
                    };
                    return match err {
                        Some(error) => Poll::Ready(Err(error)),
                        None => Poll::Ready(Ok(())), // clean FIN
                    };
                }
            }
        }
    }
}

/// Logical write half of one mux channel: chunks writes into data frames,
/// FIN on shutdown, RST if dropped without one.
pub(crate) struct MuxWriter {
    id: u32,
    data: PollSender<Bytes>,
    shared: Arc<Shared>,
    fin_sent: bool,
}

impl MuxWriter {
    fn conn_gone() -> io::Error {
        io::Error::new(io::ErrorKind::BrokenPipe, "mux connection closed")
    }
}

impl AsyncWrite for MuxWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.fin_sent {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "write after channel shutdown",
            )));
        }
        if !self.shared.is_live(self.id) {
            // RST'd (by peer or locally) — fail fast so the invocation ends
            // instead of streaming frames the peer will ignore as stale.
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "channel closed",
            )));
        }
        ready!(self.data.poll_reserve(cx)).map_err(|_| Self::conn_gone())?;
        let n = buf.len().min(frame::MAX_PAYLOAD);
        let frame = frame::encode(self.id, 0, &buf[..n]);
        self.data.send_item(frame).map_err(|_| Self::conn_gone())?;
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        // The write loop flushes after every message; nothing is buffered here.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        if self.fin_sent {
            return Poll::Ready(Ok(()));
        }
        if !self.shared.is_live(self.id) {
            // Channel already dead (RST): a FIN would be stale noise.
            self.fin_sent = true;
            return Poll::Ready(Ok(()));
        }
        if ready!(self.data.poll_reserve(cx)).is_err() {
            // Connection gone: shutting down a dead channel is a no-op.
            self.fin_sent = true;
            return Poll::Ready(Ok(()));
        }
        let id = self.id;
        let fin = frame::encode(id, frame::FLAG_FIN, &[]);
        if self.data.send_item(fin).is_err() {
            self.fin_sent = true;
            return Poll::Ready(Ok(()));
        }
        self.fin_sent = true;
        self.shared.local_done(id);
        Poll::Ready(Ok(()))
    }
}

impl Drop for MuxWriter {
    fn drop(&mut self) {
        if !self.fin_sent {
            // Invocation aborted mid-write: tell the peer the channel is dead.
            self.shared.local_rst(self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use futures::channel::mpsc as fmpsc;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use wrpc_transport::frame::Accept as _;

    /// In-memory stand-in for a `WebSocketStream`: messages the test pushes
    /// into `client_tx` arrive at the mux; messages the mux writes appear on
    /// `client_rx`.
    struct FakeWs {
        tx: fmpsc::Sender<Message>,
        rx: fmpsc::Receiver<Result<Message, io::Error>>,
    }

    impl Stream for FakeWs {
        type Item = Result<Message, io::Error>;
        fn poll_next(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<Option<Self::Item>> {
            Pin::new(&mut self.rx).poll_next(cx)
        }
    }

    impl Sink<Message> for FakeWs {
        type Error = io::Error;
        fn poll_ready(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Pin::new(&mut self.tx)
                .poll_ready(cx)
                .map_err(io::Error::other)
        }
        fn start_send(mut self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            Pin::new(&mut self.tx)
                .start_send(item)
                .map_err(io::Error::other)
        }
        fn poll_flush(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Pin::new(&mut self.tx)
                .poll_flush(cx)
                .map_err(io::Error::other)
        }
        fn poll_close(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Pin::new(&mut self.tx)
                .poll_close(cx)
                .map_err(io::Error::other)
        }
    }

    struct Peer {
        /// Push client->daemon messages here.
        to_mux: fmpsc::Sender<Result<Message, io::Error>>,
        /// Daemon->client messages appear here.
        from_mux: fmpsc::Receiver<Message>,
        acceptor: MuxAcceptor,
        cancel: CancellationToken,
    }

    fn start() -> Peer {
        let (out_tx, from_mux) = fmpsc::channel::<Message>(1024);
        let (to_mux, in_rx) = fmpsc::channel::<Result<Message, io::Error>>(1024);
        let ws = FakeWs {
            tx: out_tx,
            rx: in_rx,
        };
        let ctx = ConnCtx {
            identity: crate::identity::Identity::Anonymous,
        };
        let cancel = CancellationToken::new();
        let acceptor = start_mux(ctx, ws, cancel.clone());
        Peer {
            to_mux,
            from_mux,
            acceptor,
            cancel,
        }
    }

    async fn send_frame(peer: &mut Peer, id: u32, flags: u8, payload: &[u8]) {
        peer.to_mux
            .send(Ok(Message::binary(frame::encode(id, flags, payload))))
            .await
            .expect("mux dropped its inbound stream");
    }

    async fn accept(peer: &Peer) -> (ConnCtx, MuxWriter, MuxReader) {
        tokio::time::timeout(Duration::from_secs(5), (&peer.acceptor).accept())
            .await
            .expect("timed out waiting for accept")
            .expect("accept failed")
    }

    /// Next daemon->client message, skipping pings.
    async fn next_msg(peer: &mut Peer) -> Message {
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(5), peer.from_mux.next())
                .await
                .expect("timed out waiting for outbound message")
                .expect("mux writer ended");
            if msg.is_ping() || msg.is_pong() {
                continue;
            }
            return msg;
        }
    }

    async fn next_frame(peer: &mut Peer) -> (u32, u8, Bytes) {
        let msg = next_msg(peer).await;
        assert!(msg.is_binary(), "expected binary frame, got {msg:?}");
        frame::decode(Bytes::from(msg.into_payload())).expect("daemon sent malformed frame")
    }

    async fn read_all(reader: &mut MuxReader) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        reader.read_to_end(&mut out).await?;
        Ok(out)
    }

    #[tokio::test]
    async fn two_channels_interleave_independently() {
        let mut peer = start();

        // Open both channels, interleaving their data.
        send_frame(&mut peer, 1, 0, b"alpha-").await;
        send_frame(&mut peer, 2, 0, b"beta-").await;
        send_frame(&mut peer, 1, frame::FLAG_FIN, b"one").await;
        send_frame(&mut peer, 2, frame::FLAG_FIN, b"two").await;

        let (_ctx1, mut w1, mut r1) = accept(&peer).await;
        let (_ctx2, mut w2, mut r2) = accept(&peer).await;

        assert_eq!(read_all(&mut r1).await.unwrap(), b"alpha-one");
        assert_eq!(read_all(&mut r2).await.unwrap(), b"beta-two");

        // Respond in the opposite order; each channel's bytes stay its own.
        w2.write_all(b"resp-two").await.unwrap();
        w2.shutdown().await.unwrap();
        w1.write_all(b"resp-one").await.unwrap();
        w1.shutdown().await.unwrap();

        assert_eq!(
            next_frame(&mut peer).await,
            (2, 0, Bytes::from_static(b"resp-two"))
        );
        assert_eq!(
            next_frame(&mut peer).await,
            (2, frame::FLAG_FIN, Bytes::new())
        );
        assert_eq!(
            next_frame(&mut peer).await,
            (1, 0, Bytes::from_static(b"resp-one"))
        );
        assert_eq!(
            next_frame(&mut peer).await,
            (1, frame::FLAG_FIN, Bytes::new())
        );

        peer.cancel.cancel();
    }

    #[tokio::test]
    async fn fin_piggybacked_on_data_and_standalone_both_eof() {
        let mut peer = start();

        // Piggybacked: data and FIN in one frame.
        send_frame(&mut peer, 1, frame::FLAG_FIN, b"payload").await;
        let (_ctx, _w1, mut r1) = accept(&peer).await;
        assert_eq!(read_all(&mut r1).await.unwrap(), b"payload");

        // Standalone: data frame, then an empty FIN frame.
        send_frame(&mut peer, 2, 0, b"payload").await;
        send_frame(&mut peer, 2, frame::FLAG_FIN, &[]).await;
        let (_ctx, _w2, mut r2) = accept(&peer).await;
        assert_eq!(read_all(&mut r2).await.unwrap(), b"payload");

        peer.cancel.cancel();
    }

    #[tokio::test]
    async fn rst_kills_only_its_channel() {
        let mut peer = start();

        send_frame(&mut peer, 1, 0, b"one").await;
        send_frame(&mut peer, 2, 0, b"two").await;
        let (_c1, mut w1, mut r1) = accept(&peer).await;
        let (_c2, mut w2, mut r2) = accept(&peer).await;

        send_frame(&mut peer, 1, frame::FLAG_RST, &[]).await;

        // Channel 1's reader errors (after its buffered payload) and its
        // writer fails fast.
        let err = read_all(&mut r1)
            .await
            .expect_err("reader must error on RST");
        assert_eq!(err.kind(), io::ErrorKind::ConnectionReset);
        // The RST may still be in flight when the writer polls; retry briefly.
        let mut write_failed = false;
        for _ in 0..50 {
            if w1.write_all(b"x").await.is_err() {
                write_failed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(write_failed, "writer must fail after RST");

        // Channel 2 is unaffected.
        send_frame(&mut peer, 2, frame::FLAG_FIN, &[]).await;
        assert_eq!(read_all(&mut r2).await.unwrap(), b"two");
        w2.write_all(b"ok").await.unwrap();
        w2.shutdown().await.unwrap();
        assert_eq!(
            next_frame(&mut peer).await,
            (2, 0, Bytes::from_static(b"ok"))
        );
        assert_eq!(
            next_frame(&mut peer).await,
            (2, frame::FLAG_FIN, Bytes::new())
        );

        peer.cancel.cancel();
    }

    #[tokio::test]
    async fn dropping_writer_without_fin_sends_rst() {
        let mut peer = start();

        send_frame(&mut peer, 1, frame::FLAG_FIN, b"req").await;
        let (_ctx, w, mut r) = accept(&peer).await;
        assert_eq!(read_all(&mut r).await.unwrap(), b"req");

        drop(w); // aborted invocation: no FIN was sent
        let (id, flags, _) = next_frame(&mut peer).await;
        assert_eq!((id, flags), (1, frame::FLAG_RST));

        peer.cancel.cancel();
    }

    #[tokio::test]
    async fn stale_frames_are_silently_ignored() {
        let mut peer = start();

        // Channel 1 lives and dies by RST.
        send_frame(&mut peer, 1, 0, b"one").await;
        let (_c1, _w1, _r1) = accept(&peer).await;
        send_frame(&mut peer, 1, frame::FLAG_RST, &[]).await;

        // Late data for the dead channel: must be ignored, not a violation.
        send_frame(&mut peer, 1, 0, b"late").await;
        send_frame(&mut peer, 1, frame::FLAG_FIN, &[]).await;

        // The connection is still healthy: a new channel works end to end.
        send_frame(&mut peer, 2, frame::FLAG_FIN, b"still-alive").await;
        let (_c2, mut w2, mut r2) = accept(&peer).await;
        assert_eq!(read_all(&mut r2).await.unwrap(), b"still-alive");
        w2.write_all(b"yes").await.unwrap();
        w2.shutdown().await.unwrap();
        assert_eq!(
            next_frame(&mut peer).await,
            (2, 0, Bytes::from_static(b"yes"))
        );

        peer.cancel.cancel();
    }

    /// Every protocol violation tears down the whole connection: live
    /// readers error and the daemon sends a close frame.
    #[tokio::test]
    async fn protocol_violations_close_the_connection() {
        let violations: Vec<(&str, Message)> = vec![
            (
                "reserved flags",
                Message::binary(frame::encode(2, 0b0000_0100, b"x")),
            ),
            ("text frame", Message::text(String::new())),
            ("oversized payload", Message::binary(oversized_frame())),
            ("channel id 0", Message::binary(channel_zero_frame())),
        ];

        for (name, poison) in violations {
            let mut peer = start();

            // A live channel to observe the teardown through.
            send_frame(&mut peer, 1, 0, b"pending").await;
            let (_ctx, _w, mut r) = accept(&peer).await;

            peer.to_mux.send(Ok(poison)).await.unwrap();

            let err = read_all(&mut r)
                .await
                .expect_err(&format!("{name}: live reader must error"));
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{name}");

            // Daemon announces the protocol error with a close frame.
            loop {
                let msg = next_msg(&mut peer).await;
                if msg.is_close() {
                    break;
                }
            }
            peer.cancel.cancel();
        }
    }

    #[tokio::test]
    async fn channel_limit_rsts_new_channel_but_keeps_connection() {
        let mut peer = start();

        for id in 1..=(frame::MAX_CHANNELS as u32) {
            send_frame(&mut peer, id, 0, b"x").await;
        }
        // One over the limit: RST for it, nothing else disturbed.
        let over = frame::MAX_CHANNELS as u32 + 1;
        send_frame(&mut peer, over, 0, b"x").await;

        let (id, flags, _) = next_frame(&mut peer).await;
        assert_eq!((id, flags), (over, frame::FLAG_RST));

        // Channel 1 still works end to end.
        let (_ctx, mut w1, mut r1) = accept(&peer).await;
        send_frame(&mut peer, 1, frame::FLAG_FIN, &[]).await;
        assert_eq!(read_all(&mut r1).await.unwrap(), b"x");
        w1.write_all(b"alive").await.unwrap();
        w1.shutdown().await.unwrap();
        assert_eq!(
            next_frame(&mut peer).await,
            (1, 0, Bytes::from_static(b"alive"))
        );

        peer.cancel.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn keepalive_ping_sent_within_interval() {
        let mut peer = start();

        let msg = tokio::time::timeout(PING_INTERVAL + Duration::from_secs(1), async {
            loop {
                let msg = peer.from_mux.next().await.expect("mux writer ended");
                if msg.is_ping() {
                    return msg;
                }
            }
        })
        .await
        .expect("no ping within the keepalive interval");
        assert!(msg.is_ping());

        peer.cancel.cancel();
    }

    #[tokio::test]
    async fn shutdown_fails_live_channels_and_stops_tasks() {
        let mut peer = start();

        send_frame(&mut peer, 1, 0, b"pending").await;
        let (_ctx, _w, mut r) = accept(&peer).await;

        peer.cancel.cancel();

        let err = read_all(&mut r)
            .await
            .expect_err("reader must observe shutdown");
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);

        // Writer task ends with a best-effort close frame.
        let deadline = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(msg) = peer.from_mux.next().await {
                if msg.is_close() {
                    return true;
                }
            }
            false
        })
        .await
        .expect("writer did not stop after cancellation");
        assert!(deadline, "expected a close frame on shutdown");
    }

    /// Hand-built frame addressed to the reserved channel 0 (`encode()`
    /// debug-asserts against producing one, so build it manually).
    fn channel_zero_frame() -> Bytes {
        use bytes::BufMut as _;
        let mut buf = bytes::BytesMut::with_capacity(frame::HEADER_LEN + 1);
        buf.put_u32(0);
        buf.put_u8(0);
        buf.put_u8(b'x');
        buf.freeze()
    }

    /// Hand-built frame whose payload exceeds `MAX_PAYLOAD` (`encode()`
    /// debug-asserts against producing one, so build it manually).
    fn oversized_frame() -> Bytes {
        use bytes::BufMut as _;
        let mut buf = bytes::BytesMut::with_capacity(frame::HEADER_LEN + frame::MAX_PAYLOAD + 1);
        buf.put_u32(2);
        buf.put_u8(0);
        buf.put_slice(&vec![0u8; frame::MAX_PAYLOAD + 1]);
        buf.freeze()
    }
}
