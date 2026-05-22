//! Session worker thread: bootstraps the current-thread tokio runtime,
//! runs the PTY reader task and the command dispatcher on a `LocalSet`.

use std::io::Read;
use std::rc::Rc;

use bytes::Bytes;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::sync::broadcast;

use super::Command;
use crate::pty::{PtyError, SpawnOptions, Subscription};

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

    let reader = pty_pair
        .master
        .try_clone_reader()
        .map_err(|e| PtyError::Backend { source: e.into() })?;
    let master = pty_pair.master;
    let broadcast_capacity = opts.broadcast_capacity;

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

    std::thread::Builder::new()
        .name("cairn-pty-session".into())
        .spawn(move || {
            // Keep the master fd alive for the lifetime of this thread; it
            // closes when this thread exits and `_master` drops here.
            let _master = master;
            let local = tokio::task::LocalSet::new();

            local.block_on(&rt, async move {
                let (bcast_tx, _bcast_rx_dropped) =
                    broadcast::channel::<Bytes>(broadcast_capacity);
                let bcast_tx = Rc::new(bcast_tx);

                // PTY reader: portable_pty's reader is blocking std::io::Read.
                // Run it on a dedicated std::thread that forwards chunks to the
                // LocalSet via a flume channel.
                let (chunk_tx, chunk_rx) = flume::unbounded::<Bytes>();
                let _reader_thread = std::thread::Builder::new()
                    .name("cairn-pty-reader".into())
                    .spawn({
                        let mut reader = reader;
                        move || {
                            let mut buf = vec![0u8; 65536];
                            loop {
                                match reader.read(&mut buf) {
                                    Ok(0) => break, // EOF
                                    Ok(n) => {
                                        let chunk = Bytes::copy_from_slice(&buf[..n]);
                                        if chunk_tx.send(chunk).is_err() {
                                            break;
                                        }
                                    }
                                    Err(e)
                                        if e.kind() == std::io::ErrorKind::Interrupted =>
                                    {
                                        continue
                                    }
                                    Err(_) => break,
                                }
                            }
                        }
                    });

                // Local task that drains chunk_rx and broadcasts.
                // All active subscribers receive each chunk; lagged receivers
                // get RecvError::Lagged and should re-subscribe to recover.
                let bcast_tx_in_task = bcast_tx.clone();
                let _broadcast_task = tokio::task::spawn_local(async move {
                    while let Ok(chunk) = chunk_rx.recv_async().await {
                        let _ = bcast_tx_in_task.send(chunk);
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
                            // Snapshot stays empty until libghostty-vt
                            // Formatter is wired in Task 14. Live stream works.
                            let sub = Subscription {
                                snapshot: Bytes::new(),
                                stream: bcast_tx.subscribe(),
                            };
                            let _ = reply.send(Ok(sub));
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
        })
        .map_err(|e| PtyError::Io { source: e })?;

    Ok(WorkerHandles { cmd_tx, exit_rx })
}
