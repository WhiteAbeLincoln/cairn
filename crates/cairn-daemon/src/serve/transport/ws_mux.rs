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
//! - One **writer task** is the sole sink consumer: data/FIN frames (and
//!   over-limit RSTs, which need the queue's backpressure) arrive in order on
//!   a bounded queue; local-abort RSTs and connection-close jump it via a
//!   control queue that is unbounded but bounded in practice by the live
//!   channel count (aborts may overtake data; FIN must not, so FIN rides the
//!   data queue). It flushes after every message (same rationale as
//!   [`crate::ws::FlushOnWrite`]), owns the keepalive ping interval, and
//!   closes connections whose peer goes silent past [`LIVENESS_TIMEOUT`].
//! - [`MuxReader`]/[`MuxWriter`] are the per-channel logical stream halves
//!   handed to the wRPC frame server through [`MuxAcceptor`]; each channel's
//!   header read runs in its own spawned accept so no channel can stall the
//!   others.
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

/// The `Sec-WebSocket-Protocol` name selecting this muxed protocol. Public
/// (re-exported via [`crate::ws`]) so tests and client implementations offer
/// the exact string the daemon's negotiation table matches.
pub const MUX_SUBPROTOCOL: &str = "cairn-mux-v0";
/// The explicit name for the one-shot protocol (the default when no
/// subprotocol is offered) — pins the wire format against future default
/// changes.
pub const ONESHOT_SUBPROTOCOL: &str = "cairn-oneshot-v0";

/// Inbound frames buffered per channel before the read loop suspends
/// (whole-socket pause = TCP backpressure, per the design spec).
const INBOUND_BUFFER_FRAMES: usize = 16;
/// Outbound data frames buffered across all channels before writers block.
const OUTBOUND_QUEUE_FRAMES: usize = 64;
/// Keepalive ping cadence: keeps NAT/proxy paths warm and detects dead
/// clients (browsers auto-pong; `tokio-websockets` auto-replies likewise).
const PING_INTERVAL: Duration = Duration::from_secs(30);
/// A connection with no inbound traffic (pongs included) for this long is
/// declared dead and closed — the enforcement half of the keepalive pings,
/// without which a silently-vanished peer holds its channels until the OS
/// TCP timeout (~15 minutes). 2.5 ping intervals tolerates one lost pong.
const LIVENESS_TIMEOUT: Duration = Duration::from_millis(75_000);
/// Bounds for draining unread frames after the connection ends, so closing
/// the socket with unread bytes doesn't turn into a TCP RST that destroys
/// the in-flight close frame (same rationale as [`crate::ws::DrainOnDrop`]).
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const DRAIN_LIMIT: usize = 256 * 1024;

/// Wire framing for `cairn-mux-v0`. Public (re-exported as
/// [`crate::ws::mux`], the same pattern as [`crate::ws::split`]) so
/// integration tests and client implementations can share the codec instead
/// of duplicating it.
pub mod frame {
    use bytes::{Buf as _, BufMut as _, Bytes, BytesMut};

    /// `[channel_id: u32 BE][flags: u8]`.
    pub const HEADER_LEN: usize = 5;
    /// Sender's write side of this channel is done after this frame's payload.
    pub const FLAG_FIN: u8 = 1;
    /// Channel aborted; both directions dead. Payload ignored.
    pub const FLAG_RST: u8 = 1 << 1;
    const KNOWN_FLAGS: u8 = FLAG_FIN | FLAG_RST;
    /// Maximum frame payload; larger is a protocol violation.
    pub const MAX_PAYLOAD: usize = 1 << 20;
    /// Maximum concurrent channels; the daemon RSTs channels beyond this
    /// rather than killing the socket.
    pub const MAX_CHANNELS: usize = 256;

    pub fn encode(channel: u32, flags: u8, payload: &[u8]) -> Bytes {
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
    pub enum Violation {
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

    pub fn decode(mut msg: Bytes) -> Result<(u32, u8, Bytes), Violation> {
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

/// Source of the connection's client-opened channels: yields one
/// `(ConnCtx, MuxWriter, MuxReader)` triple per channel.
pub(crate) struct MuxAcceptor {
    rx: flume::Receiver<(ConnCtx, MuxWriter, MuxReader)>,
}

impl MuxAcceptor {
    /// Next opened channel, or `None` once the connection is over.
    pub(crate) async fn next(&self) -> Option<(ConnCtx, MuxWriter, MuxReader)> {
        self.rx.recv_async().await.ok()
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
    let (sink, mut stream) = ws.split();
    let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
    let (data_tx, data_rx) = mpsc::channel::<Bytes>(OUTBOUND_QUEUE_FRAMES);
    let (accept_tx, accept_rx) = flume::bounded(frame::MAX_CHANNELS);
    let shared = Arc::new(Shared {
        channels: Mutex::new(ChannelMap::default()),
        ctl: ctl_tx,
    });
    // Any inbound traffic (data, pongs, close) proves the peer is alive; the
    // writer declares the connection dead when this goes stale.
    let last_seen = Arc::new(Mutex::new(tokio::time::Instant::now()));

    tokio::spawn(write_loop(
        sink,
        data_rx,
        ctl_rx,
        last_seen.clone(),
        cancel.clone(),
    ));
    tokio::spawn(async move {
        let end = read_loop(
            ctx,
            &mut stream,
            &shared,
            data_tx,
            accept_tx,
            &last_seen,
            &cancel,
        )
        .await;
        // Connection over (EOF, error, violation, or shutdown): stop the
        // writer and the serve loop waiting on the (now closed) acceptor.
        cancel.cancel();
        if matches!(end, ReadEnd::Peer) {
            // Consume whatever the peer pipelined behind the last frame we
            // acted on, so closing the socket with unread bytes in the kernel
            // buffer doesn't answer with TCP RST and destroy the writer's
            // close frame (see `crate::ws::DrainOnDrop` for the full story).
            let _ = tokio::time::timeout(DRAIN_TIMEOUT, async {
                let mut budget = DRAIN_LIMIT;
                while let Some(Ok(msg)) = stream.next().await {
                    if msg.is_close() {
                        break;
                    }
                    budget = budget.saturating_sub(msg.as_payload().len().max(1));
                    if budget == 0 {
                        break;
                    }
                }
            })
            .await;
        }
    });

    MuxAcceptor { rx: accept_rx }
}

/// Serve wRPC invocations over one muxed WebSocket connection until it closes
/// or `shutdown` fires. The muxed sibling of the one-shot path's `serve_one`.
///
/// Unlike `run_wrpc_server`'s single sequential accept, each channel's wRPC
/// header is read in its own spawned task: `Server::accept` awaits the header
/// inline on the accepted stream, so a channel opened without a (complete)
/// header would otherwise stall every other invocation on the connection. A
/// stalled task ends when its channel errors out on connection death.
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

    let server: Arc<wrpc_transport::Server<ConnCtx, MuxReader, MuxWriter, ()>> =
        Arc::new(wrpc_transport::Server::new());
    let invocations = cairn_protocol::serve(server.as_ref(), daemon).await?;
    let mut invocations = futures::stream::select_all(
        invocations
            .into_iter()
            .map(|(instance, name, stream)| stream.map(move |res| (instance, name, res))),
    );

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,

            chan = acceptor.next() => {
                let Some((cx, tx, rx)) = chan else { break }; // mux died
                let server = server.clone();
                tokio::spawn(async move {
                    let one = super::websocket::OneShot::new(cx, tx, rx);
                    if let Err(error) = server.accept(&one).await {
                        tracing::debug!(%error, "mux channel rejected");
                    }
                });
            }

            item = invocations.next() => {
                match item {
                    Some((_instance, _name, Ok(fut))) => {
                        tokio::spawn(fut);
                    }
                    Some((instance, name, Err(error))) => {
                        tracing::debug!(%error, %instance, %name, "mux invocation failed");
                    }
                    None => break,
                }
            }
        }
    }

    Ok(())
}

/// One outbound step: an ordinary message, or the connection's final message
/// (a close frame) after which the writer stops.
enum Out {
    Msg(Message),
    Last(Message),
}

/// Sole consumer of the WebSocket sink: serializes data frames, control
/// frames (which jump the data queue), and keepalive pings, flushing after
/// every message so streamed responses reach the peer promptly. Also owns
/// the liveness verdict: no inbound traffic for [`LIVENESS_TIMEOUT`] closes
/// the connection. Cancels `cancel` on exit so the read side (which may be
/// parked on a dead socket) tears down with it.
async fn write_loop<Snk, E>(
    mut sink: Snk,
    mut data: mpsc::Receiver<Bytes>,
    mut ctl: mpsc::UnboundedReceiver<Ctl>,
    last_seen: Arc<Mutex<tokio::time::Instant>>,
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
        let out = tokio::select! {
            biased;
            // The read task queues Ctl::Close *then* cancels, so on
            // cancellation a queued close (with its protocol-error code and
            // reason) must still win over the generic goodbye.
            _ = cancel.cancelled() => Out::Last(queued_close(&mut ctl)),
            Some(c) = ctl.recv() => match c {
                Ctl::Rst(id) => Out::Msg(Message::binary(frame::encode(id, frame::FLAG_RST, &[]))),
                Ctl::Close { code, reason } => Out::Last(Message::close(Some(code), &reason)),
            },
            Some(bytes) = data.recv() => Out::Msg(Message::binary(bytes)),
            _ = ping.tick() => {
                let stale = {
                    let seen = last_seen.lock().unwrap_or_else(|p| p.into_inner());
                    seen.elapsed() > LIVENESS_TIMEOUT
                };
                if stale {
                    tracing::debug!("mux peer unresponsive; closing connection");
                    Out::Last(Message::close(None, "keepalive timeout"))
                } else {
                    Out::Msg(Message::ping(Bytes::new()))
                }
            }
        };
        let (msg, last) = match out {
            Out::Msg(msg) => (msg, false),
            Out::Last(msg) => (msg, true),
        };
        if let Err(error) = sink.send(msg).await {
            tracing::debug!(%error, "mux write failed; closing connection");
            // Best-effort goodbye on a transport that may already be gone.
            let _ = sink.send(Message::close(None, "")).await;
            break;
        }
        if last {
            break;
        }
    }
    // Writer-initiated deaths (keepalive timeout, sink error) must tear down
    // the read side too; on other paths this is an idempotent no-op.
    cancel.cancel();
}

/// Drain already-queued control frames for a close, so a protocol-error
/// close (code + reason) is not lost to the cancellation race. Falls back
/// to the generic goodbye.
fn queued_close(ctl: &mut mpsc::UnboundedReceiver<Ctl>) -> Message {
    while let Ok(c) = ctl.try_recv() {
        if let Ctl::Close { code, reason } = c {
            return Message::close(Some(code), &reason);
        }
    }
    Message::close(None, "")
}

/// How the read side ended — decides whether the post-loop drain runs.
enum ReadEnd {
    /// Cancelled (shutdown, or writer-initiated teardown): the socket is
    /// being abandoned deliberately; nothing to drain.
    Cancelled,
    /// The peer's traffic ended it (EOF, close, error, violation): unread
    /// pipelined frames may remain and should be drained before close.
    Peer,
}

/// Reads frames off the socket and routes them to channels until the
/// connection ends. Returning (for any reason) means the connection is dead;
/// the caller cancels the shared token.
async fn read_loop<St, E>(
    ctx: ConnCtx,
    stream: &mut St,
    shared: &Arc<Shared>,
    data_tx: mpsc::Sender<Bytes>,
    accept_tx: flume::Sender<(ConnCtx, MuxWriter, MuxReader)>,
    last_seen: &Mutex<tokio::time::Instant>,
    cancel: &CancellationToken,
) -> ReadEnd
where
    St: Stream<Item = Result<Message, E>> + Unpin,
    E: std::fmt::Display,
{
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => {
                shared.fail_all(io::ErrorKind::Interrupted, "mux connection closed");
                return ReadEnd::Cancelled;
            }
            next = stream.next() => next,
        };
        let msg = match next {
            Some(Ok(msg)) => msg,
            Some(Err(error)) => {
                shared.fail_all(io::ErrorKind::ConnectionReset, &error.to_string());
                return ReadEnd::Peer;
            }
            None => break, // peer went away without a close frame
        };
        // Any inbound message — pongs included — proves the peer is alive.
        {
            let mut seen = last_seen.lock().unwrap_or_else(|p| p.into_inner());
            *seen = tokio::time::Instant::now();
        }

        if msg.is_binary() {
            match frame::decode(Bytes::from(msg.into_payload())) {
                Ok((id, flags, payload)) => {
                    handle_frame(&ctx, shared, &data_tx, &accept_tx, id, flags, payload).await;
                }
                Err(violation) => {
                    protocol_error(shared, &violation.to_string());
                    return ReadEnd::Peer;
                }
            }
        } else if msg.is_text() {
            protocol_error(shared, "text frame in muxed mode");
            return ReadEnd::Peer;
        } else if msg.is_close() {
            break;
        }
        // Pings/pongs are handled by tokio-websockets itself; skip.
    }
    shared.fail_all(
        io::ErrorKind::ConnectionReset,
        "WebSocket connection closed",
    );
    ReadEnd::Peer
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
    },
    Deliver {
        tx: Option<mpsc::Sender<Bytes>>,
        fin: bool,
    },
    /// A live channel reset by the peer.
    Rst,
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
                // A FIN'd open is inserted already remote-done with no feed:
                // the payload below still reaches the reader through `in_tx`,
                // whose drop then delivers EOF.
                map.live.insert(
                    id,
                    Entry {
                        inbound: (!fin).then(|| in_tx.clone()),
                        err: err.clone(),
                        remote_done: fin,
                        local_done: false,
                    },
                );
                Action::Open { in_tx, in_rx, err }
            }
        } else if let Some(entry) = map.live.get(&id) {
            if rst {
                Action::Rst
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

    match action {
        Action::Open { in_tx, in_rx, err } => {
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
            // A full queue blocks here, pausing the whole socket until the
            // serve loop catches up (it drains promptly — each channel's
            // header read is spawned, never awaited inline). If the serve
            // loop is gone entirely, the send fails and the channel is
            // dropped; connection teardown is already underway.
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
        Action::Rst => shared.remote_rst(id),
        Action::RstOverLimit => {
            tracing::debug!(channel = id, "mux channel limit exceeded; resetting");
            // Through the *bounded* data queue, not ctl: an open flood past
            // the channel limit would grow the unbounded ctl queue without
            // limit (one RST per inbound frame) while the biased select
            // starved data frames. Here a backlog pauses the read loop
            // instead — the same whole-socket backpressure as data. Abort
            // ordering doesn't matter for a channel that never existed.
            let _ = data_tx.send(frame::encode(id, frame::FLAG_RST, &[])).await;
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
        tokio::time::timeout(Duration::from_secs(5), peer.acceptor.next())
            .await
            .expect("timed out waiting for accept")
            .expect("mux ended before yielding a channel")
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

            // Daemon announces the protocol error with a close frame carrying
            // code 1002 and a reason — not the generic goodbye (that close is
            // queued on ctl and races the cancellation; the writer must
            // prefer it).
            loop {
                let msg = next_msg(&mut peer).await;
                if msg.is_close() {
                    // Close payload: 2-byte big-endian code, then UTF-8 reason.
                    let payload = msg.as_payload();
                    assert!(
                        payload.len() > 2,
                        "{name}: close frame must carry code + reason, got {payload:?}"
                    );
                    let code = u16::from_be_bytes([payload[0], payload[1]]);
                    assert_eq!(code, 1002, "{name}: close code must be PROTOCOL_ERROR");
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

    /// A peer that never sends anything (not even pongs — the fake socket
    /// swallows pings) must be declared dead after [`LIVENESS_TIMEOUT`]:
    /// close frame sent, live channels failed, tasks torn down.
    #[tokio::test(start_paused = true)]
    async fn silent_peer_is_closed_after_liveness_timeout() {
        let mut peer = start();

        // A live channel to observe the teardown through.
        send_frame(&mut peer, 1, 0, b"pending").await;
        let (_ctx, _w, mut r) = accept(&peer).await;

        // Consume outbound traffic until the keepalive verdict arrives;
        // paused time auto-advances through the ping ticks.
        let saw_close = tokio::time::timeout(LIVENESS_TIMEOUT * 3, async {
            loop {
                let msg = peer.from_mux.next().await.expect("mux writer ended");
                if msg.is_close() {
                    return true;
                }
            }
        })
        .await
        .expect("connection must be closed after the liveness timeout");
        assert!(saw_close);

        // The teardown propagates to live channels and the read side.
        let err = read_all(&mut r)
            .await
            .expect_err("live reader must fail when the peer is declared dead");
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
    }

    /// Inbound traffic (any message) resets the liveness clock: a peer that
    /// keeps talking is never declared dead, even with time advancing far
    /// past the timeout.
    #[tokio::test(start_paused = true)]
    async fn active_peer_is_not_closed_by_liveness_timeout() {
        let mut peer = start();

        send_frame(&mut peer, 1, 0, b"pending").await;
        let (_ctx, _w, _r) = accept(&peer).await;

        // Talk periodically for several timeout windows; the daemon must
        // only ever send pings, never a close.
        for i in 0..12u32 {
            tokio::time::sleep(LIVENESS_TIMEOUT / 2).await;
            send_frame(&mut peer, 1, 0, &[i as u8]).await;
            while let Ok(Some(msg)) =
                tokio::time::timeout(Duration::from_millis(1), peer.from_mux.next()).await
            {
                assert!(
                    !msg.is_close(),
                    "an active peer must not be closed for liveness"
                );
            }
        }

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
