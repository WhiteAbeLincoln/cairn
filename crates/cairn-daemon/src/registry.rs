//! In-process session registry. Holds `Arc<SessionEntry>` keyed by UUIDv7 id;
//! resolution is by exact live name then exact id. Locking discipline: never
//! hold an entry lock across `.await`, never hold two entry locks at once.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use cairn_pty::{ClientId, GhosttyPty, PtySession};
use cairn_protocol::cairn::daemon::types::{ExitStatus as WireExit, SessionInfo, SessionSpec};
use tokio::sync::oneshot;

use crate::error::DaemonError;
use crate::spawn::options_from;

/// The swappable runtime state of a session (replaced on restart).
struct Running {
    handle: Arc<dyn PtySession>,
    pid: Option<u32>,
}

/// One client's attach registration; `kick` fires `kick` to evict it.
pub struct AttachHandle {
    pub kick: oneshot::Sender<()>,
}

/// A registry entry: stable identity + swappable handle + daemon-side metadata.
pub struct SessionEntry {
    pub id: String,
    pub created_at_unix_ms: u64,
    pub spec: SessionSpec,
    name: Mutex<Option<String>>,
    running: RwLock<Running>,
    pub attached: Mutex<HashMap<ClientId, AttachHandle>>,
}

impl SessionEntry {
    /// Clone the current session handle out of the lock (held only for the clone).
    pub fn handle(&self) -> Arc<dyn PtySession> {
        self.running.read().expect("running lock").handle.clone()
    }

    pub fn pid(&self) -> Option<u32> {
        self.running.read().expect("running lock").pid
    }

    pub fn name(&self) -> Option<String> {
        self.name.lock().expect("name lock").clone()
    }

    fn set_name(&self, new: String) {
        *self.name.lock().expect("name lock") = Some(new);
    }

    fn swap_running(&self, handle: Arc<dyn PtySession>, pid: Option<u32>) {
        *self.running.write().expect("running lock") = Running { handle, pid };
    }
}

pub struct SessionRegistry {
    sessions: RwLock<HashMap<String, Arc<SessionEntry>>>,
    next_client_id: AtomicU64,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            next_client_id: AtomicU64::new(0),
        }
    }

    pub fn mint_client_id(&self) -> ClientId {
        ClientId::from_u64(self.next_client_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Resolve by exact live name first, then by exact id.
    pub fn resolve(&self, key: &str) -> Option<Arc<SessionEntry>> {
        let map = self.sessions.read().expect("sessions lock");
        if let Some(entry) = map.values().find(|e| e.name().as_deref() == Some(key)) {
            return Some(entry.clone());
        }
        map.get(key).cloned()
    }

    pub fn list(&self) -> Vec<Arc<SessionEntry>> {
        self.sessions.read().expect("sessions lock").values().cloned().collect()
    }

    /// Spawn a new session. Rejects a name already used by a live session.
    pub async fn create(
        &self,
        spec: SessionSpec,
        default_shell: &str,
    ) -> Result<SessionInfo, DaemonError> {
        if let Some(name) = &spec.name
            && self.resolve(name).is_some()
        {
            return Err(DaemonError::NameInUse);
        }
        let opts = options_from(spec.clone(), default_shell);
        let handle = GhosttyPty::spawn(opts).map_err(|_| DaemonError::SpawnFailed)?;
        let pid = None; // pid surfaced via inspect later if cairn-pty exposes it; None for v0
        let id = uuid::Uuid::now_v7().to_string();
        let entry = Arc::new(SessionEntry {
            id: id.clone(),
            created_at_unix_ms: now_unix_ms(),
            spec: spec.clone(),
            name: Mutex::new(spec.name.clone()),
            running: RwLock::new(Running { handle: Arc::new(handle), pid }),
            attached: Mutex::new(HashMap::new()),
        });
        // Build SessionInfo before inserting — lock is dropped before .await.
        let info = session_info(&entry).await;
        self.sessions.write().expect("sessions lock").insert(id, entry);
        Ok(info)
    }

    pub fn rename(&self, key: &str, new_name: String) -> Result<(), DaemonError> {
        let entry = self.resolve(key).ok_or(DaemonError::NotFound)?;
        if let Some(existing) = self.resolve(&new_name)
            && existing.id != entry.id
        {
            return Err(DaemonError::NameInUse);
        }
        entry.set_name(new_name);
        Ok(())
    }

    /// Re-spawn under the same id/name; rejects a still-running session unless `force`.
    pub fn restart(
        &self,
        key: &str,
        force: bool,
        default_shell: &str,
    ) -> Result<(), DaemonError> {
        let entry = self.resolve(key).ok_or(DaemonError::NotFound)?;
        if entry.handle().try_exit_status().is_none() && !force {
            return Err(DaemonError::Running);
        }
        let opts = options_from(entry.spec.clone(), default_shell);
        let handle = GhosttyPty::spawn(opts).map_err(|_| DaemonError::SpawnFailed)?;
        entry.swap_running(Arc::new(handle), None); // old handle dropped -> Drop kills old child
        Ok(())
    }
}

pub(crate) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Build the wire `SessionInfo` for an entry (size via `size()`, exit via the
/// non-blocking `try_exit_status()`).
///
/// Locking discipline: `handle()` clones the `Arc<dyn PtySession>` out under a
/// brief read lock; `size()` is only awaited AFTER that lock is released.
pub async fn session_info(entry: &SessionEntry) -> SessionInfo {
    // Clone handle out — lock released here before any .await.
    let handle = entry.handle();
    let (cols, rows) = handle.size().await.map(|s| (s.cols, s.rows)).unwrap_or((0, 0));
    let exit = handle.try_exit_status().map(|st| WireExit {
        code: st.code(),
        signal: st.signal().map(|s| s as u8),
        unix_ms: st.unix_ms(),
    });
    let attached_clients = entry
        .attached
        .lock()
        .expect("attached lock")
        .keys()
        .map(|c| c.to_string())
        .collect();
    SessionInfo {
        id: entry.id.clone(),
        name: entry.name(),
        pid: entry.pid(),
        cols,
        rows,
        attached_clients,
        created_at_unix_ms: entry.created_at_unix_ms,
        exit,
        spec: entry.spec.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_protocol::cairn::daemon::types::SessionSpec;

    fn spec(name: Option<&str>) -> SessionSpec {
        SessionSpec {
            name: name.map(str::to_string),
            command: vec!["sleep".into(), "100".into()],
            env: vec![],
            env_inherit: true,
            workdir: None,
            tty: true,
            stdin: true,
            idle_timeout_secs: None,
            scrollback_lines: 100,
        }
    }

    #[tokio::test]
    async fn create_then_resolve_by_name_and_id() {
        let reg = SessionRegistry::new();
        let info = reg.create(spec(Some("dev")), "/bin/sh").await.expect("create");
        let by_name = reg.resolve("dev").expect("by name");
        let by_id = reg.resolve(&info.id).expect("by id");
        assert_eq!(by_name.id, info.id);
        assert_eq!(by_id.id, info.id);
        assert!(reg.resolve("nope").is_none());
    }

    #[tokio::test]
    async fn duplicate_live_name_is_rejected() {
        let reg = SessionRegistry::new();
        reg.create(spec(Some("dev")), "/bin/sh").await.expect("first");
        let err = reg.create(spec(Some("dev")), "/bin/sh").await.expect_err("dup");
        assert!(matches!(err, crate::error::DaemonError::NameInUse));
    }

    #[tokio::test]
    async fn mint_client_id_is_monotonic() {
        let reg = SessionRegistry::new();
        let a = reg.mint_client_id();
        let b = reg.mint_client_id();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn rename_updates_resolve() {
        let reg = SessionRegistry::new();
        let info = reg.create(spec(Some("old")), "/bin/sh").await.expect("create");
        reg.rename(&info.id, "new".to_string()).expect("rename");
        assert!(reg.resolve("old").is_none());
        let e = reg.resolve("new").expect("by new name");
        assert_eq!(e.id, info.id);
    }

    #[tokio::test]
    async fn rename_to_same_name_is_ok() {
        let reg = SessionRegistry::new();
        let info = reg.create(spec(Some("same")), "/bin/sh").await.expect("create");
        // Renaming a session to its own current name should be a no-op success.
        reg.rename(&info.id, "same".to_string()).expect("rename to same name");
        let e = reg.resolve("same").expect("still resolvable");
        assert_eq!(e.id, info.id);
    }

    #[tokio::test]
    async fn restart_force_replaces_running() {
        let reg = SessionRegistry::new();
        let info = reg.create(spec(Some("worker")), "/bin/sh").await.expect("create");
        // Session is running — force restart should succeed.
        reg.restart(&info.id, true, "/bin/sh").expect("force restart");
        // Session still resolves under the same id.
        let e = reg.resolve(&info.id).expect("still in registry");
        assert_eq!(e.id, info.id);
    }

    #[tokio::test]
    async fn restart_without_force_while_running_is_rejected() {
        let reg = SessionRegistry::new();
        let info = reg.create(spec(Some("worker")), "/bin/sh").await.expect("create");
        let err = reg.restart(&info.id, false, "/bin/sh").expect_err("should reject");
        assert!(matches!(err, crate::error::DaemonError::Running));
    }

    #[tokio::test]
    async fn list_returns_all_sessions() {
        let reg = SessionRegistry::new();
        reg.create(spec(Some("a")), "/bin/sh").await.expect("a");
        reg.create(spec(Some("b")), "/bin/sh").await.expect("b");
        let entries = reg.list();
        assert_eq!(entries.len(), 2);
    }
}
