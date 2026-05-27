pub mod attach;
pub mod logs;
pub mod meta;
pub mod send;
pub mod sessions;
pub mod wait;

use cairn_protocol::cairn::daemon::types::ExitStatus as WireExit;

/// Map a `cairn_pty::ExitStatus` to the wire `exit-status` record.
pub fn wire_exit(st: cairn_pty::ExitStatus) -> WireExit {
    WireExit {
        code: st.code(),
        signal: st.signal().map(|s| s as u8),
        unix_ms: st.unix_ms(),
    }
}
