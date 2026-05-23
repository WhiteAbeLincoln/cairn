//! Session worker thread: bootstraps the current-thread tokio runtime,
//! runs a single LocalSet task that multiplexes PTY I/O, command dispatch,
//! and child exit via tokio::select!.
//!
//! See docs/superpowers/specs/2026-05-22-pty-session-trait-design.md for
//! the architectural rationale (single thread per session, Unix-only,
//! pty-process for AsyncRead/AsyncWrite and tokio::process::Child).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use bytes::Bytes;
use libghostty_vt::{Terminal, TerminalOptions};
use tokio::sync::broadcast;

use super::Command;
use super::input_classifier::is_user_input;
use super::process::{ChildProcess, Pty};
use crate::{ClientId, PtyError, SpawnOptions, Subscription, TermSize};

pub use std::process::ExitStatus;

/// State shared between the worker thread's setup phase and the caller.
pub(super) struct WorkerHandles {
    pub cmd_tx: flume::Sender<Command>,
    pub exit_rx: tokio::sync::watch::Receiver<Option<ExitStatus>>,
}

/// Spawn the dedicated OS thread that owns the PTY and runs the dispatcher.
///
/// Returns the channels external callers use to interact with the session.
pub(super) fn spawn(opts: SpawnOptions) -> Result<WorkerHandles, PtyError> {
    let (cmd_tx, cmd_rx) = flume::unbounded::<Command>();
    let cmd_tx_for_worker = cmd_tx.clone();
    let (exit_tx, exit_rx) = tokio::sync::watch::channel::<Option<ExitStatus>>(None);

    // Clamp to at least 1: broadcast::channel(0) panics, and capacity is just
    // a tuning knob — silently promoting 0 → 1 is more forgiving than erroring.
    let broadcast_capacity = opts.broadcast_capacity.max(1);
    let initial_size = opts.size;
    let scrollback_lines = opts.scrollback_lines;

    // pty_process::Pty::new() wraps the PTY master fd in
    // tokio::io::unix::AsyncFd, which requires an active tokio runtime
    // context. tokio::process::Command::spawn() likewise needs one. We open
    // the PTY and spawn the child on the worker thread (inside block_on) so
    // the runtime context is always present. A oneshot channel carries spawn
    // errors back to the caller synchronously via thread::join.
    //
    // We build the Runtime here so that its construction error surfaces to
    // the caller, but we do NOT call rt.enter() from this (potentially async)
    // thread to avoid the "cannot drop runtime in async context" panic that
    // tokio raises when a current_thread Runtime is dropped from an async task.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    // Oneshot channel: worker thread sends Ok(SessionState) or Err(PtyError)
    // before entering the session loop. std::sync::mpsc because we join() the
    // thread on the blocking parent path and don't need async.
    let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<(), PtyError>>();

    std::thread::Builder::new()
        .name("cairn-pty-session".into())
        .spawn(move || {
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                // Now inside the runtime context: open PTY, spawn child.
                //
                // pty-process 0.4: Pty::new() wraps the master in AsyncFd;
                // pts() returns the slave Pts used for spawn.
                let pty = match pty_process::Pty::new().map_err(|e| PtyError::Backend {
                    source: Box::new(e),
                }) {
                    Ok(p) => p,
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };

                // On macOS, TIOCSWINSZ on the master PTY fd fails with ENOTTY
                // until the slave side has been opened at least once. Open pts
                // first so that resize() succeeds.
                let pts = match pty.pts().map_err(|e| PtyError::Backend {
                    source: Box::new(e),
                }) {
                    Ok(p) => p,
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };

                if let Err(e) = pty
                    .resize(pty_process::Size::new(initial_size.rows, initial_size.cols))
                    .map_err(|e| PtyError::Backend {
                        source: Box::new(e),
                    })
                {
                    let _ = init_tx.send(Err(e));
                    return;
                }

                // Translate tokio::process::Command into pty_process::Command.
                // pty-process wraps tokio::process::Command; we copy program
                // + args + env + cwd by hand via as_std().
                //
                // Note: std::process::Command exposes overrides/removals via
                // get_envs() but does NOT expose whether env_clear() was
                // called. If a caller invokes env_clear() before spawn, the
                // child here will inherit the parent environment rather than
                // starting clean. This is a limitation of the std API and is
                // not specific to pty-process; document accordingly if any
                // adapter needs env_clear semantics.
                let std_cmd = opts.command.as_std();
                let mut builder = pty_process::Command::new(std_cmd.get_program());
                for arg in std_cmd.get_args() {
                    builder.arg(arg);
                }
                for (k, v) in std_cmd.get_envs() {
                    if let Some(v) = v {
                        builder.env(k, v);
                    } else {
                        builder.env_remove(k);
                    }
                }
                if let Some(cwd) = std_cmd.get_current_dir() {
                    builder.current_dir(cwd);
                }

                // spawn() takes &Pts; Pts can be dropped after child starts.
                let child = match builder.spawn(&pts).map_err(|e| PtyError::Backend {
                    source: Box::new(e),
                }) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };

                // CRITICAL: drop our copy of the slave fd. The child holds its own
                // dup'd fd via stdin/stdout/stderr inheritance, so the child can
                // still use the TTY — but the master only sees EOF when ALL slave
                // fds are closed. If we keep our pts alive, pty.read never returns
                // EOF after the child exits, deadlocking the post-exit cleanup path.
                drop(pts);

                // Signal success to the parent; parent unblocks once this is sent.
                let _ = init_tx.send(Ok(()));

                run_session(SessionState {
                    pty,
                    child,
                    cmd_rx,
                    cmd_tx: cmd_tx_for_worker,
                    exit_tx,
                    broadcast_capacity,
                    initial_size,
                    scrollback_lines,
                })
                .await;
            });
        })
        .map_err(|e| PtyError::Io { source: e })?;

    // Block until the worker thread finishes PTY setup. This preserves the
    // original contract: spawn() returns Ok only after the child is running.
    match init_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(_) => {
            // Thread panicked before sending; surface a generic error.
            return Err(PtyError::Backend {
                source: Box::new(std::io::Error::other(
                    "worker thread exited before PTY was ready",
                )),
            });
        }
    }

    Ok(WorkerHandles { cmd_tx, exit_rx })
}

/// Spawn a session worker that drives `run_session` against caller-provided
/// `Pty` and `ChildProcess` implementations. Used by tests to inject mocks
/// without going through pty_process / tokio::process construction.
///
/// Unlike `spawn`, this function performs no in-thread construction (the
/// caller is responsible for handing over already-ready handles), so the
/// init-mpsc back-channel isn't needed.
#[cfg(test)]
pub(super) fn spawn_with<P, C>(
    pty: P,
    child: C,
    opts: SpawnOptions,
) -> Result<WorkerHandles, PtyError>
where
    P: Pty,
    C: ChildProcess,
{
    let (cmd_tx, cmd_rx) = flume::unbounded::<Command>();
    let cmd_tx_for_worker = cmd_tx.clone();
    let (exit_tx, exit_rx) = tokio::sync::watch::channel::<Option<ExitStatus>>(None);

    let broadcast_capacity = opts.broadcast_capacity.max(1);
    let initial_size = opts.size;
    let scrollback_lines = opts.scrollback_lines;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    std::thread::Builder::new()
        .name("cairn-pty-session-mock".into())
        .spawn(move || {
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                run_session(SessionState {
                    pty,
                    child,
                    cmd_rx,
                    cmd_tx: cmd_tx_for_worker,
                    exit_tx,
                    broadcast_capacity,
                    initial_size,
                    scrollback_lines,
                })
                .await;
            });
        })
        .map_err(|e| PtyError::Io { source: e })?;

    Ok(WorkerHandles { cmd_tx, exit_rx })
}

struct SessionState<P: Pty, C: ChildProcess> {
    pty: P,
    child: C,
    cmd_rx: flume::Receiver<Command>,
    cmd_tx: flume::Sender<Command>,
    exit_tx: tokio::sync::watch::Sender<Option<ExitStatus>>,
    broadcast_capacity: usize,
    initial_size: TermSize,
    scrollback_lines: usize,
}

/// Main session loop. Runs inside the LocalSet on the dedicated thread.
///
/// Single tokio::select! across:
///   - pty.read(...)               (PTY readable → vt_write + broadcast)
///   - cmd_rx.recv_async()         (external commands → dispatch)
///   - child.wait()                (child exit → publish status + tear down)
async fn run_session<P: Pty, C: ChildProcess>(mut s: SessionState<P, C>) {
    // Pending writes from the libghostty-vt PtyWriteFn callback. The callback
    // is synchronous (fires inside terminal.vt_write); pty.write_all is async.
    // We queue bytes in the callback and drain them on the same task after
    // each vt_write call. Rc<RefCell<...>> is safe because the LocalSet is
    // single-threaded; borrow_mut is held only across sync code.
    let pending_writes: Rc<RefCell<VecDeque<Bytes>>> = Rc::default();

    // Shared counter of "primary attached" subscribers. Incremented in
    // the Command::Subscribe arm; decremented by the PrimaryGuard inside
    // each Subscription on drop. Read by libghostty callbacks
    // (installed below) to decide whether to emit backend replies.
    // Atomic (not Cell) so it can be cloned into Subscriptions, which
    // are Send.
    let primary_count: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));

    // Construct the VT emulator. The PtyWriteFn closure captures a clone of
    // pending_writes and pushes; the main loop drains and forwards to pty.
    let terminal = match Terminal::new(TerminalOptions {
        cols: s.initial_size.cols,
        rows: s.initial_size.rows,
        max_scrollback: s.scrollback_lines,
    }) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = ?e, "failed to construct libghostty-vt Terminal");
            drain_commands_with_construction_error(&s.cmd_rx);
            return;
        }
    };
    // Wrap in Rc<RefCell<>> NOW, before any callback install. libghostty
    // 0.1.1's `on_*` installers store a raw C pointer to &self.vtable as
    // userdata; if we moved the Terminal after install (e.g., by doing
    // `Rc::new(RefCell::new(terminal))` later), that pointer would dangle.
    // Constructing the Rc first places the Terminal at a stable heap
    // address; `borrow_mut()` returns &mut Terminal pointing to that
    // address. Subsequent `vt_write` and callback dispatches all see the
    // same heap location, so the userdata pointer remains valid for the
    // life of the Rc.
    let terminal = Rc::new(RefCell::new(terminal));

    let pending_for_cb = pending_writes.clone();
    let pc_for_pty_write = primary_count.clone();
    if let Err(e) = terminal.borrow_mut().on_pty_write(move |_term, data| {
        // When a primary client (Subscription holder) is attached, the
        // client emulator is the authoritative answerer for queries
        // libghostty's parser would otherwise auto-reply to (DA1, DA2,
        // DA3, DSR cursor, DECRQM, XTVERSION). Suppressing the backend
        // reply here is the load-bearing half of query delegation; the
        // other half — broadcasting the original query bytes to the
        // client — happens unconditionally in the PTY-read arm.
        if pc_for_pty_write.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            pending_for_cb
                .borrow_mut()
                .push_back(Bytes::copy_from_slice(data));
        }
    }) {
        tracing::error!(error = ?e, "failed to install PtyWriteFn callback");
        drain_commands_with_construction_error(&s.cmd_rx);
        return;
    }

    // Override libghostty's default XTVERSION reply ("libghostty") with
    // "cairn <version>". Gated on primary_count so attached client
    // emulators take over the response when present.
    const XTVERSION_REPLY: &str = concat!("cairn ", env!("CARGO_PKG_VERSION"));
    let pc_for_xtversion = primary_count.clone();
    if let Err(e) = terminal.borrow_mut().on_xtversion(move |_term| {
        if pc_for_xtversion.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            Some(XTVERSION_REPLY)
        } else {
            None
        }
    }) {
        tracing::error!(error = ?e, "failed to install XtversionFn callback");
        drain_commands_with_construction_error(&s.cmd_rx);
        return;
    }

    // Cached size; updated on every successful resize. pty_process::Pty has
    // no get_size shortcut and we always set the size ourselves, so caching
    // is authoritative. Wrapped in Rc<Cell<_>> so the on_size libghostty
    // callback (installed immediately below) can capture a clone and read it
    // synchronously inside vt_write.
    let current_size: Rc<Cell<TermSize>> = Rc::new(Cell::new(s.initial_size));

    // Answer XTWINOPS size queries (CSI 14/16/18 t). libghostty has no
    // default for these — without the callback they're silently
    // dropped. Pixel dimensions are placeholders; the backend has no
    // font. Real pixel sizes come from the client emulator once
    // attached. Non-zero defaults avoid divide-by-zero in image
    // protocol code paths.
    const DEFAULT_CELL_WIDTH_PX: u32 = 10;
    const DEFAULT_CELL_HEIGHT_PX: u32 = 20;
    let pc_for_size = primary_count.clone();
    let current_size_for_cb = current_size.clone();
    if let Err(e) = terminal.borrow_mut().on_size(move |_term| {
        if pc_for_size.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            let size = current_size_for_cb.get();
            Some(libghostty_vt::terminal::SizeReportSize {
                rows: size.rows,
                columns: size.cols,
                cell_width: DEFAULT_CELL_WIDTH_PX,
                cell_height: DEFAULT_CELL_HEIGHT_PX,
            })
        } else {
            None
        }
    }) {
        tracing::error!(error = ?e, "failed to install SizeFn callback");
        drain_commands_with_construction_error(&s.cmd_rx);
        return;
    }

    // Color scheme has no honest backend answer (no theme). Returning
    // None unconditionally is the documented policy. The callback is
    // installed (rather than left unset) so future changes to delegate
    // to attached observers live in one place.
    if let Err(e) = terminal.borrow_mut().on_color_scheme(|_term| None) {
        tracing::error!(error = ?e, "failed to install ColorSchemeFn callback");
        drain_commands_with_construction_error(&s.cmd_rx);
        return;
    }

    let (bcast_tx, _) = broadcast::channel::<Bytes>(s.broadcast_capacity);
    // Option so the EOF/exit path can drop the sender promptly, surfacing
    // RecvError::Closed to existing subscribers even if cmd_rx is still alive.
    let bcast_tx: Rc<RefCell<Option<broadcast::Sender<Bytes>>>> =
        Rc::new(RefCell::new(Some(bcast_tx)));

    let mut leader: Option<ClientId> = None;
    let mut _last_input_at: Option<std::time::Instant> = None;

    let mut buf = vec![0u8; 65536];
    // Track whether we have already published the exit status, to keep
    // behavior identical when EOF on the PTY fires before SIGCHLD propagates.
    // Used as the guard on the `child.wait()` select branch so we never
    // poll wait twice.
    let mut exit_published = false;
    // Track whether the PTY master has hit EOF/error. Used to disable the
    // pty.read branch in select! so we don't spin on a dead fd. The
    // dispatcher loop continues processing commands (returning Closed via
    // post-exit normalisation) until the caller drops GhosttyPty, at which
    // point cmd_rx disconnects and we exit.
    let mut pty_closed = false;

    loop {
        // tokio::select! creates each branch's future fresh per iteration.
        // The `&mut self` borrows that pty.read / child.wait require are
        // local to a single iteration — when one branch wins, select! drops
        // the others before running the matched arm, releasing borrows so
        // the arm can call &mut methods on the same object freely.
        tokio::select! {
            // ── PTY readable (disabled once we've seen EOF)
            res = s.pty.read(&mut buf), if !pty_closed => {
                match res {
                    Ok(0) => {
                        // EOF — child closed slave. Mark pty_closed so we
                        // stop polling read; close the broadcast so existing
                        // subscribers observe Closed; await child exit if
                        // not already published. Loop continues so callers
                        // can still receive Closed replies via cmd_rx.
                        pty_closed = true;
                        *bcast_tx.borrow_mut() = None;
                        if !exit_published
                            && let Ok(status) = s.child.wait().await {
                                let _ = s.exit_tx.send(Some(status));
                                exit_published = true;
                            }
                    }
                    Ok(n) => {
                        let chunk = Bytes::copy_from_slice(&buf[..n]);
                        // borrow_mut is held only across these sync calls — never
                        // across an .await — so no LocalSet task collision risk.
                        terminal.borrow_mut().vt_write(&chunk);
                        if let Some(tx) = bcast_tx.borrow().as_ref() {
                            let _ = tx.send(chunk);
                        }
                        // Flush any queued PtyWriteFn responses (DA1, DSR, etc.).
                        flush_pending_writes(&pending_writes, &mut s.pty).await;
                    }
                    Err(_) => {
                        // Treat I/O errors the same as EOF: stop reading,
                        // close broadcast, drain remaining commands as Closed.
                        pty_closed = true;
                        *bcast_tx.borrow_mut() = None;
                        if !exit_published
                            && let Ok(status) = s.child.wait().await {
                                let _ = s.exit_tx.send(Some(status));
                                exit_published = true;
                            }
                    }
                }
            },

            // ── External command
            recv = s.cmd_rx.recv_async() => {
                let cmd = match recv {
                    Ok(c) => c,
                    Err(_) => break, // all GhosttyPty handles dropped
                };
                if exit_published {
                    // Post-exit normalisation: reply Closed to everything except
                    // Shutdown (no-op) and Subscribe (still returns final state).
                    match cmd {
                        Command::Shutdown => break,
                        Command::Subscribe { .. } => {} // fall through to normal handler
                        Command::Resize { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                            continue;
                        }
                        Command::Size { reply } => {
                            let _ = reply.send(Err(PtyError::Closed));
                            continue;
                        }
                        Command::Write { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                            continue;
                        }
                        Command::Detach { .. } => continue, // no-op post-exit
                    }
                }
                match cmd {
                    Command::Shutdown => {
                        // Best-effort kill; the child's wait will resolve
                        // shortly after the signal lands.
                        if let Err(e) = s.child.start_kill() {
                            tracing::warn!(
                                error = %e,
                                "failed to signal child on shutdown; \
                                 it may have already exited"
                            );
                        }
                        // Await wait here so we publish status before
                        // teardown. select! has already dropped the
                        // wait-branch future for this iteration, so s.child
                        // is freely borrowable.
                        if !exit_published
                            && let Ok(status) = s.child.wait().await {
                                let _ = s.exit_tx.send(Some(status));
                            }
                        break;
                    }
                    Command::Subscribe { client_id, reply } => {
                        let snapshot = match format_snapshot(&terminal.borrow()) {
                            Ok(bytes) => bytes,
                            Err(e) => { let _ = reply.send(Err(e)); continue; }
                        };
                        let stream = match bcast_tx.borrow().as_ref() {
                            Some(tx) => tx.subscribe(),
                            None => {
                                // Session post-exit: produce a stream that
                                // immediately closes on first recv.
                                let (tmp_tx, rx) = broadcast::channel::<Bytes>(1);
                                drop(tmp_tx);
                                rx
                            }
                        };
                        let sub = Subscription::new(
                            snapshot,
                            stream,
                            primary_count.clone(),
                            client_id,
                            s.cmd_tx.clone(),
                        );
                        let _ = reply.send(Ok(sub));
                    }
                    Command::Resize { client_id, size, reply } => {
                        // Election: empty seat promotes; non-leader rejects.
                        match leader {
                            None => {
                                leader = Some(client_id);
                                tracing::info!(
                                    target: "cairn_pty::election",
                                    client_id = %client_id,
                                    cause = "resize",
                                    previous = ?None::<ClientId>,
                                    "leader promoted"
                                );
                            }
                            Some(current) if current == client_id => {
                                // Already the leader; apply.
                            }
                            Some(current) => {
                                let _ = reply.send(Err(PtyError::NotLeader {
                                    requester: client_id,
                                    current: Some(current),
                                }));
                                continue;
                            }
                        }

                        let res = (|| -> Result<(), PtyError> {
                            terminal
                                .borrow_mut()
                                .resize(size.cols, size.rows, 0, 0)
                                .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
                            s.pty
                                .set_size(size)
                                .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
                            Ok(())
                        })();
                        if res.is_ok() {
                            current_size.set(size);
                        }
                        let _ = reply.send(res);
                    }
                    Command::Size { reply } => {
                        let _ = reply.send(Ok(current_size.get()));
                    }
                    Command::Write { client_id, data, reply } => {
                        if is_user_input(&data) {
                            _last_input_at = Some(std::time::Instant::now());
                            if leader != Some(client_id) {
                                let previous = leader;
                                leader = Some(client_id);
                                tracing::info!(
                                    target: "cairn_pty::election",
                                    client_id = %client_id,
                                    cause = "input",
                                    previous = ?previous,
                                    "leader promoted"
                                );
                            }
                        }
                        let res = s.pty.write_all(&data).await.map_err(PtyError::from);
                        let _ = reply.send(res);
                    }
                    Command::Detach { client_id } => {
                        if leader == Some(client_id) {
                            tracing::info!(
                                target: "cairn_pty::election",
                                client_id = %client_id,
                                "leader vacated"
                            );
                            leader = None;
                        }
                    }
                }
            },

            // ── Child exited (independently of EOF on the PTY master).
            // Guarded by `if !exit_published` so the branch is dormant once
            // exit has been reported; tokio::select! skips the branch on
            // subsequent iterations without polling s.child again.
            //
            // Don't break here — the PTY may still have buffered output, and
            // we want to keep handling commands (returning Closed via
            // post-exit normalisation) until the caller drops GhosttyPty.
            status = s.child.wait(), if !exit_published => {
                match status {
                    Ok(s_val) => { let _ = s.exit_tx.send(Some(s_val)); }
                    Err(e) => {
                        tracing::warn!(error = %e, "child wait failed; reporting synthetic exit code 1");
                        let _ = s.exit_tx.send(Some(synthetic_exit_status(1)));
                    }
                }
                exit_published = true;
            },
        }
    }

    // Teardown:
    //  - drop bcast_tx → existing subscribers observe RecvError::Closed.
    //    (Already None'd in the pty.read EOF arm when applicable; the
    //    explicit assign here covers the case where we exited via cmd_rx
    //    disconnect before EOF, e.g. GhosttyPty dropped while child alive.)
    //  - cmd_rx falls out of scope when SessionState drops → cmd_tx sends fail
    //    on the GhosttyPty side, which we map to PtyError::Closed.
    *bcast_tx.borrow_mut() = None;
}

/// Drain queued PtyWriteFn output to the PTY master.
///
/// Called after every successful `terminal.vt_write` in case the VT parsed a
/// query (DA1/DSR/DECRQM/...) and produced a response. Drains are short and
/// synchronous most of the time; only blocks if the kernel write buffer is
/// full, which is rare for query responses (tens of bytes).
async fn flush_pending_writes<P: Pty>(
    pending: &Rc<RefCell<VecDeque<Bytes>>>,
    pty: &mut P,
) {
    loop {
        let chunk = pending.borrow_mut().pop_front();
        let Some(chunk) = chunk else {
            return;
        };
        if let Err(e) = pty.write_all(&chunk).await {
            tracing::warn!(error = %e, "PtyWriteFn flush failed; dropping response");
            return;
        }
    }
}

/// Reply Closed (via Backend wrapping a synthetic IO error) to any commands
/// the caller has queued before they discover the worker has failed to start.
/// Called from the Terminal-construction error paths.
fn drain_commands_with_construction_error(cmd_rx: &flume::Receiver<Command>) {
    let make_err = || PtyError::Backend {
        source: Box::new(std::io::Error::other("VT terminal construction failed")),
    };
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            Command::Shutdown => {}
            Command::Subscribe { reply, .. } => {
                let _ = reply.send(Err(make_err()));
            }
            Command::Resize { reply, .. } => {
                let _ = reply.send(Err(make_err()));
            }
            Command::Size { reply } => {
                let _ = reply.send(Err(make_err()));
            }
            Command::Write { reply, .. } => {
                let _ = reply.send(Err(make_err()));
            }
            Command::Detach { .. } => {}
        }
    }
}

/// Serialize the current Terminal state as a self-contained VT escape
/// sequence stream. Clients feed this to their local emulator (xterm.js,
/// ghostty-web, etc.) to reconstruct the visible screen + scrollback.
///
/// `None` is passed to `format_alloc` so libghostty uses its own default (C)
/// allocator; the returned bytes are immediately copied into a `bytes::Bytes`,
/// and the libghostty allocation is freed on drop.
fn format_snapshot(terminal: &libghostty_vt::Terminal) -> Result<Bytes, PtyError> {
    use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};

    let opts = FormatterOptions {
        format: Format::Vt,
        trim: false,
        unwrap: false,
    };
    let mut formatter = Formatter::new(terminal, opts).map_err(|e| PtyError::Backend {
        source: Box::new(e),
    })?;
    let vt_bytes = formatter
        .format_alloc(None::<&libghostty_vt::alloc::Allocator<()>>)
        .map_err(|e| PtyError::Backend {
            source: Box::new(e),
        })?;
    Ok(Bytes::copy_from_slice(&vt_bytes))
}

/// Construct a synthetic `std::process::ExitStatus` with the given exit code.
///
/// Used when `child.wait()` itself fails (rare). We surface this as a failing
/// exit so callers see the session as broken rather than reporting success.
#[cfg(unix)]
pub(super) fn synthetic_exit_status(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw((code & 0xff) << 8)
}

#[cfg(test)]
mod tests {
    //! Worker-level tests that drive `run_session` against mock `Pty` and
    //! `ChildProcess` implementations. These tests exercise the actual
    //! worker code — including the libghostty callback installation block,
    //! the primary_count gate inside `on_pty_write`, and the flush path
    //! that delivers canned replies to the PTY — rather than a parallel
    //! copy of the same closures (which is what `tests/callback_gating.rs`
    //! used to do, and what made that file misleading once the worker
    //! and test closures could drift independently).
    //!
    //! As a side effect, these tests also pin libghostty-vt 0.1.1's
    //! default wire bytes (DA1 = `\x1b[?62;22c`, XTWINOPS 18t format, etc.)
    //! — a future libghostty version that changes those bytes will fail
    //! these tests loudly.

    use std::time::Duration;

    use bytes::Bytes;
    use tokio::sync::{mpsc, watch};

    use super::*;
    use crate::ghostty::GhosttyPty;
    use crate::ghostty::process::{ChildProcess, Pty};
    use crate::session::PtySession;
    use crate::{SpawnOptions, TermSize};

    // ─── Mock implementations ─────────────────────────────────────────

    /// PTY mock backed by tokio mpsc channels.
    struct MockPty {
        read_rx: mpsc::UnboundedReceiver<Bytes>,
        write_tx: mpsc::UnboundedSender<Bytes>,
        leftover: Vec<u8>,
    }

    impl Pty for MockPty {
        async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.leftover.is_empty() {
                let n = buf.len().min(self.leftover.len());
                buf[..n].copy_from_slice(&self.leftover[..n]);
                self.leftover.drain(..n);
                return Ok(n);
            }
            match self.read_rx.recv().await {
                Some(chunk) => {
                    let n = buf.len().min(chunk.len());
                    buf[..n].copy_from_slice(&chunk[..n]);
                    if chunk.len() > n {
                        self.leftover.extend_from_slice(&chunk[n..]);
                    }
                    Ok(n)
                }
                None => Ok(0),
            }
        }

        async fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
            let _ = self.write_tx.send(Bytes::copy_from_slice(data));
            Ok(())
        }

        fn set_size(&self, _size: TermSize) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Child mock; `start_kill` triggers exit so the worker's Shutdown
    /// path can complete without hanging.
    struct MockChild {
        exit_rx: watch::Receiver<Option<ExitStatus>>,
        exit_tx: watch::Sender<Option<ExitStatus>>,
    }

    impl ChildProcess for MockChild {
        async fn wait(&mut self) -> std::io::Result<ExitStatus> {
            loop {
                if let Some(status) = *self.exit_rx.borrow() {
                    return Ok(status);
                }
                if self.exit_rx.changed().await.is_err() {
                    return Err(std::io::Error::other("exit channel closed"));
                }
            }
        }

        fn start_kill(&mut self) -> std::io::Result<()> {
            let _ = self.exit_tx.send(Some(synthetic_exit_status(0)));
            Ok(())
        }
    }

    // ─── Harness ─────────────────────────────────────────────────────

    struct MockSession {
        pty: GhosttyPty,
        read_tx: mpsc::UnboundedSender<Bytes>,
        write_rx: mpsc::UnboundedReceiver<Bytes>,
    }

    impl MockSession {
        fn new(opts: SpawnOptions) -> Self {
            let (read_tx, read_rx) = mpsc::unbounded_channel::<Bytes>();
            let (write_tx, write_rx) = mpsc::unbounded_channel::<Bytes>();
            let (exit_tx, exit_rx) = watch::channel::<Option<ExitStatus>>(None);

            let mock_pty = MockPty {
                read_rx,
                write_tx,
                leftover: Vec::new(),
            };
            let mock_child = MockChild { exit_rx, exit_tx };

            let handles = spawn_with(mock_pty, mock_child, opts).expect("spawn_with");
            let pty = GhosttyPty::from_handles(handles);
            Self {
                pty,
                read_tx,
                write_rx,
            }
        }

        fn feed(&self, data: &'static [u8]) {
            self.read_tx
                .send(Bytes::from_static(data))
                .expect("read_tx send");
        }

        async fn recv_write(&mut self, deadline: Duration) -> Option<Bytes> {
            tokio::time::timeout(deadline, self.write_rx.recv())
                .await
                .ok()
                .flatten()
        }

        async fn assert_no_write_within(&mut self, deadline: Duration) {
            if let Some(bytes) = self.recv_write(deadline).await {
                panic!(
                    "expected no PTY write within {deadline:?}, got {:?}",
                    bytes.as_ref()
                );
            }
        }

        #[allow(dead_code)]
        async fn write_as(
            &self,
            client_id: ClientId,
            data: &'static [u8],
        ) -> Result<(), PtyError> {
            use crate::session::PtySession;
            self.pty.write(client_id, Bytes::from_static(data)).await
        }

        async fn resize_as(
            &self,
            client_id: ClientId,
            size: TermSize,
        ) -> Result<(), PtyError> {
            use crate::session::PtySession;
            self.pty.resize(client_id, size).await
        }

        async fn subscribe_as(
            &self,
            client_id: ClientId,
        ) -> Result<Subscription, PtyError> {
            use crate::session::PtySession;
            self.pty.subscribe(client_id).await
        }
    }

    fn default_opts() -> SpawnOptions {
        SpawnOptions::new(tokio::process::Command::new("/bin/true"))
    }

    fn sized_opts(cols: u16, rows: u16) -> SpawnOptions {
        SpawnOptions::new(tokio::process::Command::new("/bin/true"))
            .with_size(TermSize { cols, rows })
    }

    // The Command field of SpawnOptions is unused when spawn_with is the
    // entry point (the mock doesn't spawn a real child), but SpawnOptions
    // is shared with the production path so we still construct one.
    // /bin/true is a safe placeholder.

    // ─── Tests ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn worker_answers_da1_when_no_subscriber() {
        let mut session = MockSession::new(default_opts());
        session.feed(b"\x1b[c");
        let written = session
            .recv_write(Duration::from_secs(1))
            .await
            .expect("expected DA1 reply");
        assert_eq!(written.as_ref(), b"\x1b[?62;22c");
    }

    #[tokio::test]
    async fn worker_suppresses_da1_when_subscriber_attached() {
        let mut session = MockSession::new(default_opts());
        let _sub = session.pty.subscribe(ClientId::from_u64(0)).await.expect("subscribe");
        session.feed(b"\x1b[c");
        session
            .assert_no_write_within(Duration::from_millis(200))
            .await;
    }

    #[tokio::test]
    async fn worker_resumes_replies_after_subscriber_drops() {
        let mut session = MockSession::new(default_opts());

        let sub = session.pty.subscribe(ClientId::from_u64(0)).await.expect("subscribe");
        session.feed(b"\x1b[c");
        session
            .assert_no_write_within(Duration::from_millis(200))
            .await;

        drop(sub);

        session.feed(b"\x1b[c");
        let written = session
            .recv_write(Duration::from_secs(1))
            .await
            .expect("expected DA1 reply after subscription drop");
        assert_eq!(written.as_ref(), b"\x1b[?62;22c");
    }

    #[tokio::test]
    async fn worker_xtversion_brands_as_cairn_when_no_subscriber() {
        let mut session = MockSession::new(default_opts());
        session.feed(b"\x1b[>q");
        let written = session
            .recv_write(Duration::from_secs(1))
            .await
            .expect("expected XTVERSION reply");
        let text = std::str::from_utf8(&written).unwrap_or("<non-utf8>");
        assert!(
            text.contains("cairn "),
            "reply should brand as cairn, got {text:?}"
        );
        assert!(
            text.contains(env!("CARGO_PKG_VERSION")),
            "reply should include the crate version, got {text:?}"
        );
        assert!(
            !text.contains("libghostty"),
            "default libghostty fingerprint leaked, got {text:?}"
        );
    }

    #[tokio::test]
    async fn worker_xtversion_suppressed_when_subscriber_attached() {
        let mut session = MockSession::new(default_opts());
        let _sub = session.pty.subscribe(ClientId::from_u64(0)).await.expect("subscribe");
        session.feed(b"\x1b[>q");
        session
            .assert_no_write_within(Duration::from_millis(200))
            .await;
    }

    #[tokio::test]
    async fn worker_on_size_answers_xtwinops_18t() {
        let mut session = MockSession::new(sized_opts(132, 50));
        session.feed(b"\x1b[18t");
        let written = session
            .recv_write(Duration::from_secs(1))
            .await
            .expect("expected XTWINOPS reply");
        assert_eq!(written.as_ref(), b"\x1b[8;50;132t");
    }

    #[tokio::test]
    async fn worker_on_size_suppressed_when_subscriber_attached() {
        let mut session = MockSession::new(default_opts());
        let _sub = session.pty.subscribe(ClientId::from_u64(0)).await.expect("subscribe");
        session.feed(b"\x1b[18t");
        session
            .assert_no_write_within(Duration::from_millis(200))
            .await;
    }

    #[tokio::test]
    async fn worker_color_scheme_never_replies() {
        let mut session = MockSession::new(default_opts());

        session.feed(b"\x1b[?996n");
        session
            .assert_no_write_within(Duration::from_millis(200))
            .await;

        let _sub = session.pty.subscribe(ClientId::from_u64(0)).await.expect("subscribe");
        session.feed(b"\x1b[?996n");
        session
            .assert_no_write_within(Duration::from_millis(200))
            .await;
    }

    #[tokio::test]
    async fn resize_from_empty_seat_promotes_to_leader() {
        let session = MockSession::new(default_opts());
        let client = ClientId::from_u64(0);
        session
            .resize_as(client, TermSize { cols: 100, rows: 30 })
            .await
            .expect("first resize should succeed and promote");
        // A second resize from the same client must succeed (leader is established).
        session
            .resize_as(client, TermSize { cols: 110, rows: 35 })
            .await
            .expect("leader's subsequent resize should succeed");
    }

    #[tokio::test]
    async fn non_leader_resize_returns_not_leader_error() {
        let session = MockSession::new(default_opts());
        let a = ClientId::from_u64(0);
        let b = ClientId::from_u64(1);

        session
            .resize_as(a, TermSize { cols: 100, rows: 30 })
            .await
            .expect("a's resize promotes a to leader");

        let err = session
            .resize_as(b, TermSize { cols: 110, rows: 35 })
            .await
            .expect_err("b is not leader");
        match err {
            PtyError::NotLeader { requester, current } => {
                assert_eq!(requester, b);
                assert_eq!(current, Some(a));
            }
            other => panic!("expected NotLeader, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn leader_vacates_when_subscription_drops() {
        let session = MockSession::new(default_opts());
        let a = ClientId::from_u64(0);
        let b = ClientId::from_u64(1);

        let sub_a = session.subscribe_as(a).await.expect("subscribe a");
        session
            .resize_as(a, TermSize { cols: 100, rows: 30 })
            .await
            .expect("a becomes leader");

        drop(sub_a);
        // Give the worker a chance to process the Detach.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // b should now be able to resize (seat is empty, b is promoted).
        session
            .resize_as(b, TermSize { cols: 110, rows: 35 })
            .await
            .expect("b should claim empty seat");
    }

    #[tokio::test]
    async fn non_leader_detach_does_not_clear_leader() {
        let session = MockSession::new(default_opts());
        let a = ClientId::from_u64(0);
        let b = ClientId::from_u64(1);

        let _sub_a = session.subscribe_as(a).await.expect("subscribe a");
        let sub_b = session.subscribe_as(b).await.expect("subscribe b");
        session
            .resize_as(a, TermSize { cols: 100, rows: 30 })
            .await
            .expect("a becomes leader");

        drop(sub_b);
        tokio::time::sleep(Duration::from_millis(50)).await;

        // a should still be leader: b's resize attempt fails.
        let err = session
            .resize_as(b, TermSize { cols: 110, rows: 35 })
            .await
            .unwrap_err();
        assert!(matches!(err, PtyError::NotLeader { .. }));
        // and a's own resize still succeeds.
        session
            .resize_as(a, TermSize { cols: 120, rows: 40 })
            .await
            .expect("a still leader after b detaches");
    }
}
