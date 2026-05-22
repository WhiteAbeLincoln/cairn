//! Session worker thread: bootstraps the current-thread tokio runtime,
//! runs the PTY reader task and the command dispatcher on a `LocalSet`.

use std::io::Read;

use bytes::Bytes;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::sync::broadcast;

use super::Command;
use crate::pty::{PtyError, SpawnOptions, Subscription, TermSize};

pub use portable_pty::ExitStatus;

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
    let (exit_tx, exit_rx) = tokio::sync::watch::channel::<Option<ExitStatus>>(None);

    // Capture broadcast capacity before opts fields are consumed individually.
    // Clamp to at least 1: broadcast::channel(0) panics, and capacity is just
    // a tuning knob — silently promoting 0 → 1 is more forgiving than erroring.
    let broadcast_capacity = opts.broadcast_capacity.max(1);

    // Synchronously open the PTY and spawn the child on this thread so spawn
    // errors surface to the caller rather than getting buried in the worker.
    let pty_system = native_pty_system();
    let pty_pair = pty_system
        .openpty(PtySize {
            rows: opts.size.rows,
            cols: opts.size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| PtyError::Backend { source: e.into() })?;

    // Translate std::process::Command into portable_pty::CommandBuilder.
    // portable-pty wants its own builder type; we copy program + args + env.
    let mut builder = CommandBuilder::new(opts.command.get_program());
    for arg in opts.command.get_args() {
        builder.arg(arg);
    }
    for (k, v) in opts.command.get_envs() {
        if let Some(v) = v {
            builder.env(k, v);
        } else {
            builder.env_remove(k);
        }
    }
    if let Some(cwd) = opts.command.get_current_dir() {
        builder.cwd(cwd);
    }

    let mut child = pty_pair
        .slave
        .spawn_command(builder)
        .map_err(|e| PtyError::Backend { source: e.into() })?;

    // The slave side can be dropped after spawn — the child holds its own
    // open fd to it. Keeping it open in the parent prevents EOF detection.
    drop(pty_pair.slave);

    // Clone a killer handle before moving `child` into the waiter thread.
    // The session thread uses this to signal the child on Shutdown without
    // needing to reach across thread ownership into the waiter.
    let mut killer = child.clone_killer();

    // Clone a reader handle BEFORE moving `master` into the session thread.
    // try_clone_reader() takes &self, so we can do this before the move.
    let reader = pty_pair
        .master
        .try_clone_reader()
        .map_err(|e| PtyError::Backend { source: e.into() })?;

    let master = pty_pair.master;

    // Spawn a dedicated waiter thread so the child's exit status is published
    // independently of the command-loop thread. This lets GhosttyPty::wait()
    // resolve even when no Shutdown command is ever sent — the command loop is
    // a separate concern from child lifetime.
    std::thread::Builder::new()
        .name("cairn-pty-waiter".into())
        .spawn(move || {
            let status = child.wait().unwrap_or_else(|e| {
                tracing::warn!(error = %e, "child wait failed; reporting synthetic exit code 1");
                ExitStatus::with_exit_code(1)
            });
            let _ = exit_tx.send(Some(status));
        })
        .map_err(|e| PtyError::Io { source: e })?;

    // Build the runtime on this (parent) thread so construction failures
    // surface to the caller via spawn() rather than panicking in the
    // worker thread. Runtime is Send; we move it into the closure.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    // Set up the flume channel that bridges the blocking reader thread into
    // the async LocalSet. The sender lives in the reader thread; the receiver
    // is drained by a spawned local task inside the LocalSet.
    let (chunk_tx, chunk_rx) = flume::unbounded::<Bytes>();

    // Pre-spawn the blocking PTY reader thread before entering the LocalSet so
    // that any spawn failure propagates cleanly to the caller of `spawn()`.
    // The thread terminates on EOF (read returns 0) or on any I/O error.
    std::thread::Builder::new()
        .name("cairn-pty-reader".into())
        .spawn(move || {
            let mut reader = reader;
            let mut buf = vec![0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF — child has closed the PTY
                    Ok(n) => {
                        let chunk = Bytes::copy_from_slice(&buf[..n]);
                        if chunk_tx.send(chunk).is_err() {
                            // Receiver dropped — LocalSet has shut down.
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        tracing::warn!(error = %e, "PTY reader error; exiting reader thread");
                        break;
                    }
                }
            }
        })
        .map_err(|e| PtyError::Io { source: e })?;

    std::thread::Builder::new()
        .name("cairn-pty-session".into())
        .spawn(move || {
            // Keep the master fd alive for the lifetime of this thread; it
            // closes when this thread exits and `master` drops here.
            let master = master;
            let local = tokio::task::LocalSet::new();

            local.block_on(&rt, async move {
                let (bcast_tx, _) = broadcast::channel::<Bytes>(broadcast_capacity);

                // Forward bytes from the blocking reader thread into the
                // broadcast channel. Runs as a LocalSet task so it yields
                // between chunks and doesn't starve the command dispatcher.
                // broadcast::Sender is internally Arc-backed, so cloning it
                // directly is sufficient — no Rc wrapper needed.
                let bcast_tx_for_reader = bcast_tx.clone();
                // When the dispatcher loop exits (Shutdown or cmd_rx closed), the LocalSet
                // drops and this task is cancelled — any chunks still in chunk_rx are
                // silently discarded. That's acceptable for shutdown semantics; subscribers
                // observe broadcast Closed when bcast_tx drops along with the LocalSet.
                tokio::task::spawn_local(async move {
                    while let Ok(chunk) = chunk_rx.recv_async().await {
                        let _ = bcast_tx_for_reader.send(chunk);
                    }
                });

                // Command dispatcher
                while let Ok(cmd) = cmd_rx.recv_async().await {
                    match cmd {
                        Command::Shutdown => {
                            // Best-effort kill; the waiter thread observes the
                            // child exit and publishes the status regardless.
                            if let Err(e) = killer.kill() {
                                tracing::warn!(
                                    error = %e,
                                    "failed to kill child on shutdown; \
                                     it may have already exited"
                                );
                            }
                            break;
                        }
                        Command::Subscribe { reply } => {
                            // Snapshot is empty for now — Task 14 wires in the
                            // Formatter-backed scrollback snapshot.
                            let sub = Subscription {
                                snapshot: Bytes::new(),
                                stream: bcast_tx.subscribe(),
                            };
                            let _ = reply.send(Ok(sub));
                        }
                        // Resize, Size, and Write are wired up in Tasks 10–12.
                        Command::Resize { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                        Command::Size { reply } => {
                            let result = master
                                .get_size()
                                .map(|s| TermSize {
                                    cols: s.cols,
                                    rows: s.rows,
                                })
                                .map_err(|e| PtyError::Backend { source: e.into() });
                            let _ = reply.send(result);
                        }
                        Command::Write { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                    }
                }
            });
        })
        .map_err(|e| PtyError::Io { source: e })?;

    Ok(WorkerHandles { cmd_tx, exit_rx })
}
