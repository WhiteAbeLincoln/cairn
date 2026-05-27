use std::future::Future;
use std::pin::Pin;

use cairn_protocol::cairn::daemon::types::ExitStatus as WireExit;

use crate::daemon::Daemon;
use crate::handlers::wire_exit;

/// `sessions.wait`: resolve the session, return a future that yields its exit
/// status. No in-band error channel (a bare `future<exit-status>`), so a
/// resolve miss is an outer transport error.
pub async fn wait(
    d: &Daemon,
    id: String,
) -> anyhow::Result<Pin<Box<dyn Future<Output = WireExit> + Send + 'static>>> {
    let entry = d
        .registry
        .resolve(&id)
        .ok_or_else(|| anyhow::anyhow!("session.not_found: {id}"))?;
    let handle = entry.handle();
    Ok(Box::pin(async move { wire_exit(handle.wait().await) }))
}
