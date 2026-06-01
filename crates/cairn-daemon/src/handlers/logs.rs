use std::pin::Pin;

use bytes::Bytes;
use futures::Stream;
use tokio::sync::broadcast::error::RecvError;
use tokio_stream::wrappers::ReceiverStream;

use cairn_protocol::cairn::daemon::types::LogWindow;

use crate::daemon::Daemon;

/// `sessions.logs`: emit the buffered snapshot (windowed best-effort), then, if
/// `follow`, the live output until the session closes. Output-only: no input,
/// no leader, raw `Bytes` chunks (no server-event tagging).
pub async fn logs(
    d: &Daemon,
    id: String,
    window: LogWindow,
    follow: bool,
) -> anyhow::Result<Pin<Box<dyn Stream<Item = Vec<Bytes>> + Send + 'static>>> {
    let entry = d
        .registry
        .resolve(&id)
        .ok_or_else(|| anyhow::anyhow!("session.not_found: {id}"))?;
    let handle = entry.handle();
    let client_id = d.registry.mint_client_id();
    let sub = handle
        .subscribe(client_id)
        .await
        .map_err(|e| anyhow::anyhow!("subscribe failed: {e}"))?;

    let snapshot = apply_window(sub.snapshot.clone(), &window);
    let (tx, out) = tokio::sync::mpsc::channel::<Vec<Bytes>>(2);

    tokio::spawn(async move {
        // Move the whole Subscription into the task so its guard (leadership/
        // primary-count) stays alive for the task's full lifetime — not just
        // until the stream field is moved out.
        let mut sub = sub;
        if tx.send(vec![snapshot]).await.is_err() {
            return;
        }
        if !follow {
            return;
        }
        loop {
            match sub.recv().await {
                Ok(b) => {
                    if tx.send(vec![b]).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Lagged(_)) => continue, // logs tolerates gaps; keep following
                Err(RecvError::Closed) => return,      // session ended
            }
        }
    });

    Ok(Box::pin(ReceiverStream::new(out)))
}

/// Apply a `log-window` to the snapshot bytes (best-effort, line-based).
fn apply_window(snapshot: Bytes, window: &LogWindow) -> Bytes {
    match window {
        LogWindow::All => snapshot,
        LogWindow::Tail(n) => tail_lines(&snapshot, *n as usize),
    }
}

/// Last `n` lines of `bytes`, preserving line terminators. Correct whether or
/// not the final line ends in `\n` (`split_inclusive` keeps each `\n` with its
/// line and does not emit a trailing empty segment).
fn tail_lines(bytes: &[u8], n: usize) -> Bytes {
    if n == 0 {
        return Bytes::new();
    }
    let lines: Vec<&[u8]> = bytes.split_inclusive(|&b| b == b'\n').collect();
    let start = lines.len().saturating_sub(n);
    Bytes::copy_from_slice(&lines[start..].concat())
}

#[cfg(test)]
mod tests {
    use super::tail_lines;

    #[test]
    fn tail_handles_unterminated_final_line() {
        assert_eq!(&tail_lines(b"a\nb\nc", 1)[..], b"c");
        assert_eq!(&tail_lines(b"a\nb\nc", 2)[..], b"b\nc");
        assert_eq!(&tail_lines(b"a\nb\nc", 9)[..], b"a\nb\nc");
    }

    #[test]
    fn tail_handles_terminated_final_line() {
        assert_eq!(&tail_lines(b"a\nb\nc\n", 1)[..], b"c\n");
        assert_eq!(&tail_lines(b"a\nb\nc\n", 2)[..], b"b\nc\n");
    }

    #[test]
    fn tail_edge_cases() {
        assert_eq!(&tail_lines(b"a\nb\nc", 0)[..], b"");
        assert_eq!(&tail_lines(b"", 5)[..], b"");
        assert_eq!(&tail_lines(b"single", 1)[..], b"single");
    }
}
