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
    let (tx, out) = tokio::sync::mpsc::channel::<Vec<Bytes>>(32);

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
            match sub.stream.recv().await {
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

fn tail_lines(bytes: &[u8], n: usize) -> Bytes {
    if n == 0 {
        return Bytes::new();
    }
    // Index of the start of the last `n` lines.
    let mut newlines = 0usize;
    let mut start = bytes.len();
    for i in (0..bytes.len()).rev() {
        if bytes[i] == b'\n' {
            newlines += 1;
            if newlines > n {
                start = i + 1;
                break;
            }
            start = i;
        } else {
            start = i;
        }
    }
    Bytes::copy_from_slice(&bytes[start..])
}
