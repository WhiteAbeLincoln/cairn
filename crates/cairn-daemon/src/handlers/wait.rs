use std::future::Future;
use std::pin::Pin;

use cairn_protocol::cairn::daemon::types::ExitStatus as WireExit;

use crate::daemon::Daemon;
use crate::handlers::wire_exit;

/// `sessions.wait`: resolve the session, return a future that yields its exit
/// status. No in-band error channel (a bare `future<exit-status>`), so a
/// resolve miss is an outer transport error.
///
/// Plain `fn`, not `async fn`: resolving the session and building the boxed
/// future are both synchronous; the `.await` lives inside the returned
/// future, not on this function's own execution path.
pub fn wait(
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
