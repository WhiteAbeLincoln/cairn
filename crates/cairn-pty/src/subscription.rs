use bytes::Bytes;
use tokio::sync::broadcast;

/// Result of a successful [`crate::pty::PtySession::subscribe`] call.
///
/// `snapshot` is an opaque VT escape sequence representing the terminal
/// state at the moment of subscription. Feed it to a VT100/xterm-compatible
/// emulator (xterm.js, ghostty-web, etc.) before processing `stream` bytes.
///
/// `stream` yields bytes that arrived strictly *after* the snapshot was
/// captured — no gap, no overlap. `broadcast::error::RecvError::Lagged(_)`
/// means the subscriber fell behind the broadcast capacity; recover by
/// dropping this `Subscription` and calling `subscribe()` again — the new
/// snapshot reflects current state and the new receiver starts clean.
/// `RecvError::Closed` means the session has exited.
pub struct Subscription {
    pub snapshot: Bytes,
    pub stream: broadcast::Receiver<Bytes>,
}
