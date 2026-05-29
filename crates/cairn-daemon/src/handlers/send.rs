use std::pin::Pin;

use bytes::Bytes;
use futures::Stream;
use futures::StreamExt as _;

use cairn_protocol::cairn::daemon::types::Error as WireError;

use crate::daemon::Daemon;
use crate::error::{DaemonError, to_wire};

/// `sessions.send`: inject each streamed chunk into the PTY without claiming
/// leadership. Empty stream is a no-op. Returns once the input stream ends.
pub async fn send(
    d: &Daemon,
    id: String,
    mut chunks: Pin<Box<dyn Stream<Item = Vec<Bytes>> + Send + 'static>>,
) -> Result<(), WireError> {
    let entry = d
        .registry
        .resolve(&id)
        .ok_or_else(|| DaemonError::NotFound.to_wire())?;
    let handle = entry.handle();
    while let Some(batch) = chunks.next().await {
        for chunk in batch {
            handle.inject(chunk).await.map_err(to_wire)?;
        }
    }
    Ok(())
}
