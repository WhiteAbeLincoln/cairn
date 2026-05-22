//! Session worker thread: bootstraps the current-thread tokio runtime,
//! runs the PTY reader task and the command dispatcher on a `LocalSet`.

use std::cell::RefCell;
use std::io::Read;
use std::rc::Rc;

use bytes::Bytes;
use libghostty_vt::{Terminal, TerminalOptions};
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

    // Capture size before opts fields are consumed by the builder loop below.
    // TermSize is Copy, so this is a plain copy of two u16 values.
    let initial_size = opts.size;

    // Capture scrollback_lines before opts.command is consumed by CommandBuilder.
    let scrollback_lines = opts.scrollback_lines;

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
                // Take the writer FIRST so the Terminal's on_pty_write callback
                // can capture a clone of the same Rc. Both the callback (which
                // fires synchronously within vt_write) and the Command::Write
                // handler share ownership of the writer via Rc. borrow_mut() is
                // always short and synchronous — never held across an .await —
                // so there is no borrow-collision risk on this single-threaded
                // LocalSet.
                // Wrap the writer in Option for the same reason bcast_tx is
                // wrapped: the chunk-forwarder task sets it to None on EOF so
                // that Command::Write can return PtyError::Closed immediately
                // rather than returning an I/O EIO error after the child exits.
                let writer: Rc<RefCell<Option<Box<dyn std::io::Write + Send>>>> =
                    match master.take_writer() {
                        Ok(w) => Rc::new(RefCell::new(Some(w))),
                        Err(e) => {
                            tracing::error!(error = %e, "failed to take PTY writer");
                            return;
                        }
                    };

                // Owned VT state for this session. Terminal is !Send + !Sync
                // and stays pinned to this thread (the LocalSet guarantees it).
                // Rc allows the terminal to be shared between the chunk-forwarder
                // task and the dispatcher loop without crossing thread boundaries.
                //
                // The Terminal is constructed before being wrapped in Rc so that
                // on_pty_write can be installed via &mut Terminal. The closure
                // captures a clone of the writer Rc so that VT query responses
                // (DA1, DSR, etc.) are written directly back into the PTY master.
                let mut terminal = match Terminal::new(TerminalOptions {
                    cols: initial_size.cols,
                    rows: initial_size.rows,
                    max_scrollback: scrollback_lines,
                }) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!(error = ?e, "failed to construct libghostty-vt Terminal");
                        // Drain any commands that arrived (or will arrive briefly) so callers
                        // get a clean error rather than hanging. The waiter thread + watch
                        // channel still signal exit normally via the child.
                        while let Ok(cmd) = cmd_rx.try_recv() {
                            let construction_err = || PtyError::Backend {
                                source: Box::new(std::io::Error::other(
                                    "VT terminal construction failed",
                                )),
                            };
                            match cmd {
                                Command::Shutdown => {}
                                Command::Subscribe { reply } => {
                                    let _ = reply.send(Err(construction_err()));
                                }
                                Command::Resize { reply, .. } => {
                                    let _ = reply.send(Err(construction_err()));
                                }
                                Command::Size { reply } => {
                                    let _ = reply.send(Err(construction_err()));
                                }
                                Command::Write { reply, .. } => {
                                    let _ = reply.send(Err(construction_err()));
                                }
                            }
                        }
                        return;
                    }
                };

                let writer_for_pty_write = writer.clone();
                if let Err(e) = terminal.on_pty_write(move |_term, data| {
                    use std::io::Write;
                    let mut w = writer_for_pty_write.borrow_mut();
                    if let Some(w) = w.as_mut() {
                        if let Err(err) = w.write_all(data) {
                            tracing::warn!(error = %err, "PtyWriteFn failed to write response bytes");
                        }
                    }
                    // If writer is None (child exited), silently drop the response —
                    // no one is reading it anyway.
                }) {
                    tracing::error!(error = ?e, "failed to install PtyWriteFn callback");
                    return;
                }

                let terminal = Rc::new(RefCell::new(terminal));

                let (bcast_tx, _) = broadcast::channel::<Bytes>(broadcast_capacity);

                // Wrap the broadcast sender in an Rc<RefCell<Option<…>>> so that
                // the forwarder task (which runs on the same LocalSet thread) can
                // drop it — by setting the Option to None — when the PTY reader
                // thread exits on child-process EOF. Setting it to None while the
                // dispatcher is still alive causes all existing broadcast::Receiver
                // handles to observe RecvError::Closed promptly, even if the
                // GhosttyPty handle (and therefore cmd_tx) is still live.
                //
                // Late subscribers (after child exit) receive a snapshot plus a
                // broadcast receiver that immediately returns RecvError::Closed.
                let bcast_tx = Rc::new(RefCell::new(Some(bcast_tx)));

                // Forward bytes from the blocking reader thread into the broadcast
                // channel, AND feed them into the Terminal so its screen state
                // stays current. Runs as a LocalSet task so it yields between
                // chunks and doesn't starve the command dispatcher.
                let bcast_tx_for_reader = bcast_tx.clone();
                let terminal_for_reader = terminal.clone();
                let writer_for_forwarder = writer.clone();
                tokio::task::spawn_local(async move {
                    while let Ok(chunk) = chunk_rx.recv_async().await {
                        // borrow_mut is held only across this synchronous call —
                        // never across an .await — so other LocalSet tasks that
                        // borrow terminal or bcast_tx cannot collide with this one.
                        terminal_for_reader.borrow_mut().vt_write(&chunk);
                        if let Some(tx) = bcast_tx_for_reader.borrow().as_ref() {
                            let _ = tx.send(chunk);
                        }
                    }
                    // PTY reader thread exited (child closed its end / EOF).
                    // Drop the broadcast sender so existing subscribers see Closed
                    // promptly. The Rc on the dispatcher side becomes the only ref,
                    // but with None inside, so the underlying sender is gone.
                    *bcast_tx_for_reader.borrow_mut() = None;
                    // Null the writer at the same time so Command::Write returns
                    // PtyError::Closed rather than EIO when the child is gone.
                    *writer_for_forwarder.borrow_mut() = None;
                });

                // Command dispatcher: runs until Shutdown, cmd_rx disconnects,
                // or the GhosttyPty handle is dropped. The broadcast sender lives
                // in `bcast_tx` (Option), set to None by the forwarder when the
                // child exits — independent of when the handle is dropped.
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
                            // Atomic: format current Terminal state, then
                            // subscribe to subsequent bytes. broadcast::Receiver
                            // only sees messages sent after creation, so no
                            // overlap with the snapshot.
                            let snapshot = match format_snapshot(&terminal.borrow()) {
                                Ok(bytes) => bytes,
                                Err(e) => {
                                    let _ = reply.send(Err(e));
                                    continue;
                                }
                            };
                            // If bcast_tx is None (child already exited), create
                            // a temporary sender just to produce a subscriber whose
                            // channel is immediately closed (the sender drops at
                            // the end of this arm). Callers will see RecvError::Closed
                            // on the first recv(), which is the correct behavior for
                            // a dead session.
                            let stream = match bcast_tx.borrow().as_ref() {
                                Some(tx) => tx.subscribe(),
                                None => {
                                    let (tmp_tx, rx) =
                                        broadcast::channel::<Bytes>(1);
                                    drop(tmp_tx);
                                    rx
                                }
                            };
                            let sub = Subscription { snapshot, stream };
                            let _ = reply.send(Ok(sub));
                        }
                        Command::Resize { size, reply } => {
                            // Update VT state and kernel side together so no
                            // partial state is observable to subscribers between
                            // the two steps. Terminal is updated first so that
                            // any subsequent subscribe() after a resize sees the
                            // new dimensions in the snapshot.
                            if let Err(e) = terminal
                                .borrow_mut()
                                .resize(size.cols, size.rows, 0, 0)
                            {
                                let _ = reply.send(Err(PtyError::Backend {
                                    source: Box::new(e),
                                }));
                                continue;
                            }
                            let result = master
                                .resize(PtySize {
                                    rows: size.rows,
                                    cols: size.cols,
                                    pixel_width: 0,
                                    pixel_height: 0,
                                })
                                .map_err(|e| PtyError::Backend { source: e.into() });
                            let _ = reply.send(result);
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
                        Command::Write { data, reply } => {
                            use std::io::Write;
                            let result = (|| -> Result<(), PtyError> {
                                let mut w = writer.borrow_mut();
                                let w = w.as_mut().ok_or(PtyError::Closed)?;
                                w.write_all(&data)?;
                                w.flush()?;
                                Ok(())
                            })();
                            let _ = reply.send(result);
                        }
                    }
                }
            });
        })
        .map_err(|e| PtyError::Io { source: e })?;

    Ok(WorkerHandles { cmd_tx, exit_rx })
}

/// Serialize the current Terminal state as a self-contained VT escape
/// sequence stream. Clients feed this to their local emulator (xterm.js,
/// ghostty-web, etc.) to reconstruct the visible screen + scrollback.
///
/// `None` is passed to `format_alloc` so that libghostty uses its own
/// default (C) allocator; the returned `Bytes` are immediately copied into
/// a `bytes::Bytes`, and the libghostty allocation is freed on drop.
fn format_snapshot(terminal: &libghostty_vt::Terminal) -> Result<Bytes, PtyError> {
    use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};

    let opts = FormatterOptions {
        format: Format::Vt,
        trim: false,
        unwrap: false,
    };
    let mut formatter = Formatter::new(terminal, opts)
        .map_err(|e| PtyError::Backend { source: Box::new(e) })?;

    // Pass None to use libghostty's default allocator. The returned
    // `libghostty_vt::alloc::Bytes` derefs to `[u8]`; we copy into a
    // heap-owned `bytes::Bytes` so the caller has no lifetime dependency
    // on the libghostty allocation.
    let vt_bytes = formatter
        .format_alloc(None::<&libghostty_vt::alloc::Allocator<()>>)
        .map_err(|e| PtyError::Backend { source: Box::new(e) })?;

    Ok(Bytes::copy_from_slice(&vt_bytes))
}
