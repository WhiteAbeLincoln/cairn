//! In-process session registry. Holds `Arc<SessionEntry>` keyed by UUIDv7 id;
//! resolution is by exact live name then exact id. Locking discipline: never
//! hold an entry lock across `.await`, never hold two entry locks at once.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use cairn_protocol::cairn::daemon::types::{ExitStatus as WireExit, SessionInfo, SessionSpec};
use cairn_pty::{ClientId, GhosttyPty, PtySession};
use tokio::sync::{broadcast, oneshot};

use crate::error::DaemonError;
use crate::spawn::options_from;

/// Session-lifecycle notifications. Carries ids, not snapshots: two emission
/// points (`AttachGuard::drop`, `rename`) are sync, and `session_info()` is
/// async — the watch handler resolves ids to fresh snapshots itself.
#[derive(Debug, Clone)]
pub enum RegistryEvent {
    /// The session's `session-info` changed structurally
    /// (created / renamed / restarted / exited / attach / detach).
    Changed { id: String },
    /// Reserved: no emitter yet (no session-removal op exists).
    Removed { id: String },
}

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
    /// Clone of the registry's bus sender — lets attach/detach (which happen
    /// on the entry, not the registry) emit `Changed` without a back-pointer.
    events: broadcast::Sender<RegistryEvent>,
}

/// RAII guard: removes the client from the entry's attached-set on drop.
pub struct AttachGuard {
    entry: Arc<SessionEntry>,
    client_id: ClientId,
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        self.entry
            .attached
            .lock()
            .expect("attached lock")
            .remove(&self.client_id);
        // Sync send after the lock is released; no subscribers is fine.
        let _ = self.entry.events.send(RegistryEvent::Changed {
            id: self.entry.id.clone(),
        });
    }
}

impl SessionEntry {
    /// Register an attached client. Returns the kick receiver (fired by the
    /// `kick` op) and an RAII guard that deregisters on drop.
    pub fn attach(self: &Arc<Self>, client_id: ClientId) -> (oneshot::Receiver<()>, AttachGuard) {
        let (kick_tx, kick_rx) = oneshot::channel();
        self.attached
            .lock()
            .expect("attached lock")
            .insert(client_id, AttachHandle { kick: kick_tx });
        // Sync send after the lock is released; no subscribers is fine.
        let _ = self.events.send(RegistryEvent::Changed {
            id: self.id.clone(),
        });
        (
            kick_rx,
            AttachGuard {
                entry: Arc::clone(self),
                client_id,
            },
        )
    }

    /// Rendered ids of currently-attached clients (for list/inspect).
    pub fn attached_ids(&self) -> Vec<String> {
        self.attached
            .lock()
            .expect("attached lock")
            .keys()
            .map(|c| c.to_string())
            .collect()
    }

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
    /// Session-lifecycle event bus (see `RegistryEvent`). Small capacity —
    /// it carries only ids, and overflow is recoverable via subscribers'
    /// `Lagged` → resync path.
    events: broadcast::Sender<RegistryEvent>,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRegistry {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(64);
        Self {
            sessions: RwLock::new(HashMap::new()),
            next_client_id: AtomicU64::new(0),
            events,
        }
    }

    /// Subscribe to session-lifecycle events. Capacity is small (64) —
    /// overflow is recoverable: receivers treat `Lagged` as "resync".
    pub fn subscribe_events(&self) -> broadcast::Receiver<RegistryEvent> {
        self.events.subscribe()
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
        self.sessions
            .read()
            .expect("sessions lock")
            .values()
            .cloned()
            .collect()
    }

    /// Spawn a new session. Rejects an explicit name already used by a live
    /// session; infers `{basename}-{hex-tail}` when no name is given.
    pub async fn create(
        &self,
        spec: SessionSpec,
        default_shell: &str,
    ) -> Result<SessionInfo, DaemonError> {
        let id = uuid::Uuid::now_v7().to_string();
        let name = match &spec.name {
            Some(n) => {
                if self.resolve(n).is_some() {
                    return Err(DaemonError::NameInUse);
                }
                Some(n.clone())
            }
            None => Some(self.inferred_unique_name(&spec, default_shell, &id)),
        };
        let opts = options_from(spec.clone(), default_shell, id.clone());
        let handle = GhosttyPty::spawn(opts).map_err(|e| {
            tracing::warn!(
                error = %e,
                command = ?spec.command,
                workdir = ?spec.workdir,
                "session spawn failed"
            );
            DaemonError::SpawnFailed
        })?;
        let pid = None; // pid surfaced via inspect later if cairn-pty exposes it; None for v0
        let handle: Arc<dyn PtySession> = Arc::new(handle);
        let entry = Arc::new(SessionEntry {
            id: id.clone(),
            created_at_unix_ms: now_unix_ms(),
            spec: spec.clone(),
            name: Mutex::new(name),
            running: RwLock::new(Running {
                handle: handle.clone(),
                pid,
            }),
            attached: Mutex::new(HashMap::new()),
            events: self.events.clone(),
        });
        // Build SessionInfo before inserting — lock is dropped before .await.
        let info = session_info(&entry).await;
        self.sessions
            .write()
            .expect("sessions lock")
            .insert(id.clone(), entry);
        // Send after the entry is inserted, so a subscriber resolving the id finds it.
        let _ = self.events.send(RegistryEvent::Changed { id: id.clone() });
        spawn_exit_watcher(handle, self.events.clone(), id);

        tracing::info!(
            session_id = %info.id,
            name = ?info.name,
            command = ?info.spec.command,
            "session created"
        );

        Ok(info)
    }

    /// `{basename}-{hex-tail}`. `basename` is the command's file stem (or the
    /// default shell's); the suffix is the last 6 hex digits of `id` (UUIDv7's
    /// random tail — the leading digits are a shared millisecond timestamp).
    /// Always appended; extends the tail on the rare collision with a live name.
    fn inferred_unique_name(&self, spec: &SessionSpec, default_shell: &str, id: &str) -> String {
        let prog = spec
            .command
            .first()
            .map(String::as_str)
            .unwrap_or(default_shell);
        let base = std::path::Path::new(prog)
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("session");
        let hex: String = id.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        for len in 6..=hex.len() {
            let candidate = format!("{base}-{}", &hex[hex.len() - len..]);
            if self.resolve(&candidate).is_none() {
                return candidate;
            }
        }
        // Exhausted the whole hex tail (impossible in practice): fall back to the id.
        format!("{base}-{id}")
    }

    pub fn rename(&self, key: &str, new_name: String) -> Result<(), DaemonError> {
        let entry = self.resolve(key).ok_or(DaemonError::NotFound)?;
        if let Some(existing) = self.resolve(&new_name)
            && existing.id != entry.id
        {
            return Err(DaemonError::NameInUse);
        }
        entry.set_name(new_name);
        let _ = self.events.send(RegistryEvent::Changed {
            id: entry.id.clone(),
        });
        Ok(())
    }

    /// Re-spawn under the same id/name; rejects a still-running session unless `force`.
    pub fn restart(&self, key: &str, force: bool, default_shell: &str) -> Result<(), DaemonError> {
        let entry = self.resolve(key).ok_or(DaemonError::NotFound)?;
        if entry.handle().try_exit_status().is_none() && !force {
            return Err(DaemonError::Running);
        }
        let opts = options_from(entry.spec.clone(), default_shell, entry.id.clone());
        let handle = GhosttyPty::spawn(opts).map_err(|e| {
            tracing::warn!(
                error = %e,
                session_id = %entry.id,
                command = ?entry.spec.command,
                workdir = ?entry.spec.workdir,
                "session restart spawn failed"
            );
            DaemonError::SpawnFailed
        })?;
        let handle: Arc<dyn PtySession> = Arc::new(handle);
        entry.swap_running(handle.clone(), None); // old handle dropped -> Drop kills old child
        let _ = self.events.send(RegistryEvent::Changed {
            id: entry.id.clone(),
        });
        spawn_exit_watcher(handle, self.events.clone(), entry.id.clone());
        Ok(())
    }
}

/// Spawn a task that resolves when `handle` exits and emits a `Changed`
/// event for `id`. One watcher per spawned handle: `create()` and `restart()`
/// each spawn one for the handle they just created. On restart, the old
/// handle's watcher (spawned earlier) fires once as the old child dies —
/// harmless, the watch handler coalesces repeated ids for the same session.
/// Requires an ambient tokio runtime (`tokio::spawn`); all production callers
/// are RPC handlers, and registry tests are `#[tokio::test]`.
fn spawn_exit_watcher(
    handle: Arc<dyn PtySession>,
    events: broadcast::Sender<RegistryEvent>,
    id: String,
) {
    tokio::spawn(async move {
        handle.wait().await;
        let _ = events.send(RegistryEvent::Changed { id });
    });
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
    let (cols, rows) = handle
        .size()
        .await
        .map(|s| (s.cols, s.rows))
        .unwrap_or((0, 0));
    let exit = handle.try_exit_status().map(|st| WireExit {
        code: st.code(),
        signal: st.signal().map(|s| s as u8),
        unix_ms: st.unix_ms(),
        reason: st.reason().map(String::from),
    });
    let attached_clients = entry.attached_ids();
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
        let info = reg
            .create(spec(Some("dev")), "/bin/sh")
            .await
            .expect("create");
        let by_name = reg.resolve("dev").expect("by name");
        let by_id = reg.resolve(&info.id).expect("by id");
        assert_eq!(by_name.id, info.id);
        assert_eq!(by_id.id, info.id);
        assert!(reg.resolve("nope").is_none());
    }

    #[tokio::test]
    async fn duplicate_live_name_is_rejected() {
        let reg = SessionRegistry::new();
        reg.create(spec(Some("dev")), "/bin/sh")
            .await
            .expect("first");
        let err = reg
            .create(spec(Some("dev")), "/bin/sh")
            .await
            .expect_err("dup");
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
        let info = reg
            .create(spec(Some("old")), "/bin/sh")
            .await
            .expect("create");
        reg.rename(&info.id, "new".to_string()).expect("rename");
        assert!(reg.resolve("old").is_none());
        let e = reg.resolve("new").expect("by new name");
        assert_eq!(e.id, info.id);
    }

    #[tokio::test]
    async fn rename_to_same_name_is_ok() {
        let reg = SessionRegistry::new();
        let info = reg
            .create(spec(Some("same")), "/bin/sh")
            .await
            .expect("create");
        // Renaming a session to its own current name should be a no-op success.
        reg.rename(&info.id, "same".to_string())
            .expect("rename to same name");
        let e = reg.resolve("same").expect("still resolvable");
        assert_eq!(e.id, info.id);
    }

    #[tokio::test]
    async fn restart_force_replaces_running() {
        let reg = SessionRegistry::new();
        let info = reg
            .create(spec(Some("worker")), "/bin/sh")
            .await
            .expect("create");
        // Session is running — force restart should succeed.
        reg.restart(&info.id, true, "/bin/sh")
            .expect("force restart");
        // Session still resolves under the same id.
        let e = reg.resolve(&info.id).expect("still in registry");
        assert_eq!(e.id, info.id);
    }

    #[tokio::test]
    async fn restart_without_force_while_running_is_rejected() {
        let reg = SessionRegistry::new();
        let info = reg
            .create(spec(Some("worker")), "/bin/sh")
            .await
            .expect("create");
        let err = reg
            .restart(&info.id, false, "/bin/sh")
            .expect_err("should reject");
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

    #[tokio::test]
    async fn create_without_name_infers_basename_and_hex_suffix() {
        let reg = SessionRegistry::new();
        // spec(None) uses command ["sleep", "100"], so the basename is "sleep".
        let info = reg.create(spec(None), "/bin/sh").await.expect("create");
        let name = info.name.expect("a name should be inferred");
        let suffix = name
            .strip_prefix("sleep-")
            .unwrap_or_else(|| panic!("expected 'sleep-' prefix, got {name}"));
        assert_eq!(suffix.len(), 6, "suffix should be 6 hex chars: {name}");
        assert!(
            suffix.chars().all(|c| c.is_ascii_hexdigit()),
            "suffix not hex: {name}"
        );
        // The suffix is the tail of the session id's hex digits.
        let hex: String = info.id.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        assert_eq!(
            suffix,
            &hex[hex.len() - 6..],
            "suffix must be the id's hex tail"
        );
    }

    #[tokio::test]
    async fn attach_registers_then_guard_drop_removes() {
        let reg = SessionRegistry::new();
        let info = reg
            .create(spec(Some("dev")), "/bin/sh")
            .await
            .expect("create");
        let entry = reg.resolve(&info.id).expect("resolve");
        let cid = reg.mint_client_id();

        let (_kick_rx, guard) = entry.attach(cid);
        assert_eq!(entry.attached_ids(), vec![cid.to_string()]);
        drop(guard);
        assert!(entry.attached_ids().is_empty());
    }

    /// Helper: pull the next event off the bus, generously bounded so a
    /// genuine bug (no event sent) fails the test instead of hanging.
    async fn next_event(rx: &mut broadcast::Receiver<RegistryEvent>) -> RegistryEvent {
        tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("event within timeout")
            .expect("bus not closed")
    }

    #[tokio::test]
    async fn subscribe_then_create_emits_changed_with_created_id() {
        let reg = SessionRegistry::new();
        let mut events = reg.subscribe_events();
        let info = reg
            .create(spec(Some("dev")), "/bin/sh")
            .await
            .expect("create");
        let ev = next_event(&mut events).await;
        assert!(matches!(ev, RegistryEvent::Changed { id } if id == info.id));
    }

    #[tokio::test]
    async fn rename_emits_changed() {
        let reg = SessionRegistry::new();
        let info = reg
            .create(spec(Some("old")), "/bin/sh")
            .await
            .expect("create");
        let mut events = reg.subscribe_events();
        reg.rename(&info.id, "new".to_string()).expect("rename");
        let ev = next_event(&mut events).await;
        assert!(matches!(ev, RegistryEvent::Changed { id } if id == info.id));
    }

    #[tokio::test]
    async fn force_restart_emits_changed() {
        let reg = SessionRegistry::new();
        let info = reg
            .create(spec(Some("worker")), "/bin/sh")
            .await
            .expect("create");
        let mut events = reg.subscribe_events();
        reg.restart(&info.id, true, "/bin/sh")
            .expect("force restart");
        let ev = next_event(&mut events).await;
        assert!(matches!(ev, RegistryEvent::Changed { id } if id == info.id));
    }

    #[tokio::test]
    async fn attach_then_guard_drop_each_emit_changed() {
        let reg = SessionRegistry::new();
        let info = reg
            .create(spec(Some("dev")), "/bin/sh")
            .await
            .expect("create");
        let entry = reg.resolve(&info.id).expect("resolve");
        let cid = reg.mint_client_id();
        let mut events = reg.subscribe_events();

        let (_kick_rx, guard) = entry.attach(cid);
        let attach_ev = next_event(&mut events).await;
        assert!(matches!(attach_ev, RegistryEvent::Changed { id } if id == info.id));

        drop(guard);
        let detach_ev = next_event(&mut events).await;
        assert!(matches!(detach_ev, RegistryEvent::Changed { id } if id == info.id));
    }

    #[tokio::test]
    async fn exit_watcher_emits_changed_after_short_lived_session_exits() {
        let reg = SessionRegistry::new();
        let mut events = reg.subscribe_events();
        let short_lived = SessionSpec {
            name: Some("short".to_string()),
            command: vec!["sh".into(), "-c".into(), "exit 0".into()],
            env: vec![],
            env_inherit: true,
            workdir: None,
            tty: true,
            stdin: true,
            idle_timeout_secs: None,
            scrollback_lines: 100,
        };
        let info = reg.create(short_lived, "/bin/sh").await.expect("create");
        // Drain the create() event so we're waiting specifically for the
        // exit-watcher's event, with no further registry call in between.
        next_event(&mut events).await;

        let ev = next_event(&mut events).await;
        assert!(matches!(ev, RegistryEvent::Changed { id } if id == info.id));
    }
}
