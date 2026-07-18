//! Integration tests for the `cairn-mux-v0` WebSocket subprotocol: one
//! persistent connection carrying many concurrent wRPC invocations, plus the
//! `Sec-WebSocket-Protocol` negotiation table from the design spec
//! (`docs/superpowers/specs/2026-07-17-ws-mux-design.md`).
//!
//! The test-side [`MuxClient`] is a real client implementation of the mux
//! protocol (mirroring what the web UI's `wsMuxDialer` does): one WebSocket,
//! client-allocated increasing channel ids, one wRPC invocation per channel,
//! FIN on write-side completion, RST on abort. It shares the frame codec with
//! the daemon via [`cairn_daemon::ws::mux`].

mod common;

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use cairn_daemon::ws::mux as frame;
use cairn_protocol::client::cairn::daemon as api;
use common::DaemonHarness;
use futures::{SinkExt as _, StreamExt as _};
use http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio::io::AsyncWrite;
use tokio::sync::mpsc;
use wrpc_websockets::tokio_websockets::{ClientBuilder, Message};

// The daemon's own negotiation names: a typo'd offer here would be a compile
// error instead of a confusing no-echo failure.
use cairn_daemon::ws::{MUX_SUBPROTOCOL as MUX, ONESHOT_SUBPROTOCOL as ONESHOT};

// ── negotiation table ──────────────────────────────────────────────────────

/// Open `/ws` offering the given subprotocols (if any) and return the
/// `Sec-WebSocket-Protocol` value the daemon echoed (if any).
async fn negotiate(addr: SocketAddr, offer: Option<&str>) -> Option<String> {
    let mut builder = ClientBuilder::new()
        .uri(&format!("ws://{addr}/ws"))
        .expect("valid ws uri");
    if let Some(offer) = offer {
        builder = builder
            .add_header(
                SEC_WEBSOCKET_PROTOCOL,
                http::HeaderValue::from_str(offer).expect("valid header value"),
            )
            .expect("addable header");
    }
    let (_ws, resp) = builder.connect().await.expect("upgrade must succeed");
    resp.headers()
        .get(SEC_WEBSOCKET_PROTOCOL)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
}

#[tokio::test]
async fn no_offer_selects_one_shot_with_no_echo() {
    let harness = DaemonHarness::start_with_ws().await;
    let addr = harness.ws_addr.unwrap();

    assert_eq!(negotiate(addr, None).await, None);

    // And the connection actually speaks one-shot: the existing per-dial
    // client works against the same daemon.
    let info = api::meta::version(&harness.ws_client(), (), None)
        .await
        .expect("one-shot version");
    assert!(info.daemon.starts_with("cairn-daemon/"));
}

#[tokio::test]
async fn explicit_one_shot_offer_is_echoed_and_behaves_identically() {
    let harness = DaemonHarness::start_with_ws().await;
    let addr = harness.ws_addr.unwrap();

    assert_eq!(negotiate(addr, Some(ONESHOT)).await, Some(ONESHOT.into()));

    // Same connection-per-invocation behavior on a socket that offered the
    // explicit name: drive one invocation over it end to end.
    let client = OneShotWithSubprotocol { addr };
    let info = api::meta::version(&client, (), None)
        .await
        .expect("explicit one-shot version");
    assert!(info.daemon.starts_with("cairn-daemon/"));
}

#[tokio::test]
async fn mux_offer_is_echoed() {
    let harness = DaemonHarness::start_with_ws().await;
    let addr = harness.ws_addr.unwrap();
    assert_eq!(negotiate(addr, Some(MUX)).await, Some(MUX.into()));
}

#[tokio::test]
async fn unknown_offers_get_no_echo() {
    let harness = DaemonHarness::start_with_ws().await;
    let addr = harness.ws_addr.unwrap();
    // The daemon serves one-shot and echoes nothing; a real browser would
    // fail the connection client-side, which is the spec'd outcome.
    assert_eq!(negotiate(addr, Some("bogus-v9")).await, None);
}

#[tokio::test]
async fn first_supported_offer_wins() {
    let harness = DaemonHarness::start_with_ws().await;
    let addr = harness.ws_addr.unwrap();
    let echoed = negotiate(addr, Some("bogus-v9, cairn-mux-v0, cairn-oneshot-v0")).await;
    assert_eq!(echoed, Some(MUX.into()));
}

// ── muxed invocations ──────────────────────────────────────────────────────

#[tokio::test]
async fn concurrent_invocations_share_one_socket() {
    let harness = DaemonHarness::start_with_ws().await;
    let client = MuxClient::connect(harness.ws_addr.unwrap()).await;

    // Two invocations in flight at once on the same WebSocket.
    let (version, sessions) = tokio::join!(
        api::meta::version(&client, (), None),
        api::sessions::list_all(&client, (), None),
    );
    let version = version.expect("version over mux");
    assert!(
        version.daemon.starts_with("cairn-daemon/"),
        "unexpected daemon version: {}",
        version.daemon
    );
    assert_eq!(version.protocol, "cairn:daemon@0.1.0");
    assert!(
        sessions.expect("list-all over mux").is_empty(),
        "fresh daemon should have no sessions"
    );

    // The connection is reusable: a later invocation opens a fresh channel.
    let again = api::meta::version(&client, (), None)
        .await
        .expect("second version over the same socket");
    assert_eq!(again.daemon, version.daemon);
}

/// One stalled channel must not block other invocations: the daemon reads
/// each channel's wRPC header in its own task, so a channel that is opened
/// but never sends its header (or anything at all) cannot park the accept
/// path that every other channel goes through.
#[tokio::test]
async fn stalled_channel_does_not_block_other_invocations() {
    let harness = DaemonHarness::start_with_ws().await;
    let client = MuxClient::connect(harness.ws_addr.unwrap()).await;

    // Open channels that never send a wRPC header — one completely empty,
    // one with a partial header (just the version byte).
    client.open_raw_channel(&[]);
    client.open_raw_channel(&[0x00]);

    // Invocations on other channels must still complete promptly.
    let version = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        api::meta::version(&client, (), None),
    )
    .await
    .expect("version must not be blocked by a stalled channel")
    .expect("version over mux");
    assert!(version.daemon.starts_with("cairn-daemon/"));
}

/// A burst of stalled channels (under the concurrency limit) still leaves
/// the connection fully serviceable.
#[tokio::test]
async fn many_stalled_channels_do_not_wedge_the_connection() {
    let harness = DaemonHarness::start_with_ws().await;
    let client = MuxClient::connect(harness.ws_addr.unwrap()).await;

    for _ in 0..100 {
        client.open_raw_channel(&[]);
    }

    let version = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        api::meta::version(&client, (), None),
    )
    .await
    .expect("version must not be blocked by stalled channels")
    .expect("version over mux");
    assert!(version.daemon.starts_with("cairn-daemon/"));
}

#[tokio::test]
async fn mux_survives_a_failing_invocation() {
    let harness = DaemonHarness::start_with_ws().await;
    let client = MuxClient::connect(harness.ws_addr.unwrap()).await;

    // A call that returns a domain error (unknown session) must not disturb
    // the connection or a concurrent healthy call.
    let (inspect, version) = tokio::join!(
        api::sessions::inspect(&client, (), None, "no-such-session"),
        api::meta::version(&client, (), None),
    );
    let inspect = inspect.expect("inspect invocation completes");
    assert!(inspect.is_err(), "unknown session must be a domain error");
    version.expect("concurrent version unaffected");

    let after = api::meta::version(&client, (), None)
        .await
        .expect("connection still healthy after the error");
    assert!(after.daemon.starts_with("cairn-daemon/"));
}

// ── test-side mux client ───────────────────────────────────────────────────

/// A client-side implementation of `cairn-mux-v0`: one WebSocket, one wRPC
/// invocation per client-opened channel.
struct MuxClient {
    next_id: AtomicU32,
    /// Encoded frames destined for the socket (unbounded so `Drop` can queue
    /// an RST without blocking; test traffic is tiny).
    out: mpsc::UnboundedSender<Bytes>,
    /// Demux table: channel id -> feed for that invocation's reader.
    channels: Arc<Mutex<HashMap<u32, mpsc::UnboundedSender<Bytes>>>>,
}

impl MuxClient {
    async fn connect(addr: SocketAddr) -> Self {
        let (ws, resp) = ClientBuilder::new()
            .uri(&format!("ws://{addr}/ws"))
            .expect("valid ws uri")
            .add_header(SEC_WEBSOCKET_PROTOCOL, http::HeaderValue::from_static(MUX))
            .expect("addable header")
            .connect()
            .await
            .expect("mux upgrade");
        assert_eq!(
            resp.headers()
                .get(SEC_WEBSOCKET_PROTOCOL)
                .map(|v| v.as_bytes()),
            Some(MUX.as_bytes()),
            "daemon must echo the mux subprotocol"
        );

        let (mut sink, mut stream) = ws.split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<Bytes>();
        let channels: Arc<Mutex<HashMap<u32, mpsc::UnboundedSender<Bytes>>>> = Arc::default();

        tokio::spawn(async move {
            while let Some(bytes) = out_rx.recv().await {
                if sink.send(Message::binary(bytes)).await.is_err() {
                    break;
                }
            }
        });

        let demux = channels.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                if !msg.is_binary() {
                    continue; // pings/pongs; close ends the stream next poll
                }
                let Ok((id, flags, payload)) = frame::decode(Bytes::from(msg.into_payload()))
                else {
                    break; // daemon sent a malformed frame; give up loudly via EOFs
                };
                let entry = {
                    let map = demux.lock().unwrap();
                    map.get(&id).cloned()
                };
                if flags & frame::FLAG_RST != 0 {
                    demux.lock().unwrap().remove(&id);
                    continue;
                }
                if let Some(tx) = entry
                    && !payload.is_empty()
                {
                    let _ = tx.send(payload);
                }
                if flags & frame::FLAG_FIN != 0 {
                    demux.lock().unwrap().remove(&id);
                }
            }
            // Connection over: dropping the feeds EOFs every open reader.
            demux.lock().unwrap().clear();
        });

        Self {
            next_id: AtomicU32::new(1),
            out,
            channels,
        }
    }
}

impl MuxClient {
    /// Open a channel by sending a single raw frame with the given payload
    /// and never touch it again — simulates a stalled or misbehaving client
    /// (e.g. a channel whose wRPC header never arrives).
    fn open_raw_channel(&self, payload: &[u8]) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.out.send(frame::encode(id, 0, payload));
    }
}

impl wrpc_transport::Invoke for MuxClient {
    type Context = ();
    type Outgoing = wrpc_transport::frame::Outgoing;
    type Incoming = wrpc_transport::frame::Incoming;

    async fn invoke<P>(
        &self,
        (): Self::Context,
        instance: &str,
        func: &str,
        params: Bytes,
        paths: impl AsRef<[P]> + Send,
    ) -> anyhow::Result<(Self::Outgoing, Self::Incoming)>
    where
        P: AsRef<[Option<usize>]> + Send + Sync,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        self.channels.lock().unwrap().insert(id, in_tx);
        let tx = ChannelWriter {
            id,
            out: self.out.clone(),
            fin_sent: false,
        };
        // Chunk-buffered AsyncRead over the demux feed: EOF when the daemon
        // FINs (feed dropped) or the connection dies (table cleared).
        let rx = tokio_util::io::StreamReader::new(
            tokio_stream::wrappers::UnboundedReceiverStream::new(in_rx).map(Ok::<_, io::Error>),
        );
        wrpc_transport::frame::invoke(tx, rx, instance, func, params, paths).await
    }
}

/// Write half of one client channel: data frames, FIN on shutdown, RST if
/// dropped without one.
struct ChannelWriter {
    id: u32,
    out: mpsc::UnboundedSender<Bytes>,
    fin_sent: bool,
}

impl AsyncWrite for ChannelWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let n = buf.len().min(frame::MAX_PAYLOAD);
        self.out
            .send(frame::encode(self.id, 0, &buf[..n]))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "mux connection closed"))?;
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if !self.fin_sent {
            let _ = self.out.send(frame::encode(self.id, frame::FLAG_FIN, &[]));
            self.fin_sent = true;
        }
        Poll::Ready(Ok(()))
    }
}

impl Drop for ChannelWriter {
    fn drop(&mut self) {
        if !self.fin_sent {
            let _ = self.out.send(frame::encode(self.id, frame::FLAG_RST, &[]));
        }
    }
}

/// One-shot client that offers `cairn-oneshot-v0` explicitly on every dial —
/// proving the explicit name selects behavior identical to the default.
struct OneShotWithSubprotocol {
    addr: SocketAddr,
}

impl wrpc_transport::Invoke for OneShotWithSubprotocol {
    type Context = ();
    type Outgoing = wrpc_transport::frame::Outgoing;
    type Incoming = wrpc_transport::frame::Incoming;

    async fn invoke<P>(
        &self,
        (): Self::Context,
        instance: &str,
        func: &str,
        params: Bytes,
        paths: impl AsRef<[P]> + Send,
    ) -> anyhow::Result<(Self::Outgoing, Self::Incoming)>
    where
        P: AsRef<[Option<usize>]> + Send + Sync,
    {
        let (ws, resp) = ClientBuilder::new()
            .uri(&format!("ws://{}/ws", self.addr))?
            .add_header(
                SEC_WEBSOCKET_PROTOCOL,
                http::HeaderValue::from_static(ONESHOT),
            )?
            .connect()
            .await?;
        anyhow::ensure!(
            resp.headers()
                .get(SEC_WEBSOCKET_PROTOCOL)
                .map(|v| v.as_bytes())
                == Some(ONESHOT.as_bytes()),
            "daemon must echo the explicit one-shot subprotocol"
        );
        let (tx, rx) = cairn_daemon::ws::split(ws);
        wrpc_transport::frame::invoke(tx, rx, instance, func, params, paths).await
    }
}
