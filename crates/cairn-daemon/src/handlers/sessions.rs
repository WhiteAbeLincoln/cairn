//! Unary session-interface handlers.
//!
//! Each function is thin: it operates on the registry, maps errors to wire
//! envelopes, and returns. No lock is held across an `.await` — `handle()`
//! always clones the `Arc<dyn PtySession>` out before any async call.

use std::time::Duration;

use cairn_protocol::cairn::daemon::types::{Error as WireError, SessionInfo, SessionSpec, Signal};

use crate::daemon::Daemon;
use crate::error::{to_wire, DaemonError};
use crate::registry::session_info;
use crate::signal::to_libc;

pub async fn list_all(d: &Daemon) -> Vec<SessionInfo> {
    let entries = d.registry.list();
    // Fan out size() concurrently — one round-trip latency regardless of count.
    futures::future::join_all(entries.iter().map(|e| session_info(e))).await
}

pub async fn inspect(d: &Daemon, id: String) -> Result<SessionInfo, WireError> {
    let entry = d.registry.resolve(&id).ok_or_else(|| DaemonError::NotFound.to_wire())?;
    Ok(session_info(&entry).await)
}

pub async fn create(d: &Daemon, spec: SessionSpec) -> Result<SessionInfo, WireError> {
    d.registry.create(spec, &d.cfg.default_shell).await.map_err(DaemonError::to_wire)
}

pub async fn rename(d: &Daemon, id: String, new_name: String) -> Result<(), WireError> {
    d.registry.rename(&id, new_name).map_err(DaemonError::to_wire)
}

pub async fn restart(d: &Daemon, id: String, force: bool) -> Result<(), WireError> {
    d.registry.restart(&id, force, &d.cfg.default_shell).map_err(DaemonError::to_wire)
}

pub async fn kick(d: &Daemon, id: String, client: Option<String>) -> Result<(), WireError> {
    let entry = d.registry.resolve(&id).ok_or_else(|| DaemonError::NotFound.to_wire())?;
    // Note: `attached` is only populated by the attach bridge in Plan 3, so this
    // is a no-op success for the core slice — which is the correct behavior.
    let mut attached = entry.attached.lock().expect("attached lock");
    match client {
        Some(cid) => {
            // Find the matching ClientId by its string representation.
            let key = attached.keys().find(|c| c.to_string() == cid).copied();
            if let Some(k) = key
                && let Some(h) = attached.remove(&k)
            {
                let _ = h.kick.send(());
            }
        }
        None => {
            for (_id, h) in attached.drain() {
                let _ = h.kick.send(());
            }
        }
    }
    Ok(())
}

pub async fn kill(
    d: &Daemon,
    id: String,
    sig: Signal,
    grace_ms: Option<u32>,
) -> Result<(), WireError> {
    let entry = d.registry.resolve(&id).ok_or_else(|| DaemonError::NotFound.to_wire())?;
    let signum = to_libc(&sig).map_err(DaemonError::to_wire)?;
    // Clone handle out before any .await — lock is released immediately.
    let handle = entry.handle();
    handle.signal(signum).await.map_err(to_wire)?;

    if let Some(g) = grace_ms {
        // Arm a daemon-owned escalation task: fires SIGKILL after the grace period
        // if the session hasn't exited. Independent of client liveness.
        let handle = entry.handle();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(g as u64)).await;
            if handle.try_exit_status().is_none() {
                let _ = handle.signal(libc::SIGKILL).await;
            }
        });
    }
    Ok(())
}
