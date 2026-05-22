//! Session worker thread: bootstraps the current-thread tokio runtime,
//! runs the PTY reader task and the command dispatcher on a `LocalSet`.

use std::sync::Arc;

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use super::Command;
use crate::pty::{PtyError, SpawnOptions};

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
        .map_err(|e| PtyError::Backend {
            source: Into::<Box<dyn std::error::Error + Send + Sync>>::into(e),
        })?;

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
        .map_err(|e| PtyError::Backend {
            source: Into::<Box<dyn std::error::Error + Send + Sync>>::into(e),
        })?;

    // The slave side can be dropped after spawn — the child holds its own
    // open fd to it. Keeping it open in the parent prevents EOF detection.
    drop(pty_pair.slave);

    let master = Arc::new(std::sync::Mutex::new(pty_pair.master));

    // Spawn a dedicated waiter thread so the child's exit status is published
    // independently of the command-loop thread. This lets GhosttyPty::wait()
    // resolve even when no Shutdown command is ever sent — the command loop is
    // a separate concern from child lifetime.
    std::thread::Builder::new()
        .name("cairn-pty-waiter".into())
        .spawn(move || {
            let status = child
                .wait()
                .unwrap_or_else(|_| ExitStatus::with_exit_code(1));
            let _ = exit_tx.send(Some(status));
        })
        .map_err(|e| PtyError::Io { source: e })?;

    std::thread::Builder::new()
        .name("cairn-pty-session".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build current-thread runtime");

            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                // For now: just drain commands until Shutdown or channel close.
                // (Reader task, dispatcher, etc. are added in later tasks.)
                while let Ok(cmd) = cmd_rx.recv_async().await {
                    match cmd {
                        Command::Shutdown => break,
                        // Other commands are not yet handled — reply with Closed
                        // so callers get a clear error in this skeleton stage.
                        Command::Subscribe { reply } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                        Command::Resize { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                        Command::Size { reply } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                        Command::Write { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                    }
                }
            });

            drop(master);
        })
        .map_err(|e| PtyError::Io { source: e })?;

    Ok(WorkerHandles { cmd_tx, exit_rx })
}
