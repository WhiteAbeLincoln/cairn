//! The interactive attach driver: bridges the local terminal to a session's
//! `attach` bidi stream, with auto-reconnect-and-repaint on transient loss.

use std::future::pending;
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use futures::channel::mpsc;

use cairn_protocol::cairn::daemon::types::{AttachInit, ClientEvent, ServerEvent};
use cairn_protocol::client::cairn::daemon::sessions;
use cairn_protocol::error_codes;

use crate::connect::Endpoint;
use crate::detach::{DetachKeys, Matcher};
use crate::signals::{Termination, window_changes};
use crate::terminal::{self, RawGuard};

pub struct AttachOptions {
    pub no_stdin: bool,
    pub detach_keys: DetachKeys,
}

enum Outcome {
    Detached,
    Exited { code: Option<i32>, signal: Option<u8> },
    Fatal(String),
    Reconnect,
}

/// Attach to `id` and run until detach, child-exit, fatal error, or giving up
/// on reconnect. Returns the process exit code.
pub async fn run(endpoint: &Endpoint, id: &str, opts: AttachOptions) -> Result<i32> {
    let client = endpoint.client();
    let guard = RawGuard::engage()?;
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        eprintln!("cairn: stdout is not a terminal; output will include raw escape sequences");
    }
    // `raw` means stdin is a real TTY. The detach-key matcher only runs in raw
    // mode; with piped/non-TTY stdin we forward bytes verbatim (the spec's
    // non-TTY behavior) so a literal detach sequence in piped data can't trigger
    // a detach. `guard` is held for the whole attach and restores the TTY on drop.
    let raw_mode = guard.is_raw();

    let mut stdin_rx = if opts.no_stdin { None } else { Some(spawn_stdin_reader()?) };
    let mut matcher = Matcher::new(opts.detach_keys.clone());
    let mut term = Termination::install()?;
    let mut winch = window_changes()?;

    let budget = reconnect_budget();
    let mut backoff = Duration::from_millis(100);
    let mut deadline: Option<Instant> = None;

    loop {
        let (cols, rows) = terminal::window_size().unwrap_or((80, 24));
        let init = AttachInit { cols, rows, no_stdin: opts.no_stdin };
        let (mut events_tx, events_rx) = mpsc::channel::<Vec<ClientEvent>>(64);
        let events: std::pin::Pin<Box<dyn futures::Stream<Item = Vec<ClientEvent>> + Send>> =
            Box::pin(events_rx);

        let outcome = match sessions::attach(&client, (), id, &init, events).await {
            Err(_e) => Outcome::Reconnect, // couldn't establish the stream
            Ok((mut server, io)) => {
                deadline = None; // connected: reset the give-up clock
                backoff = Duration::from_millis(100);
                {
                    let mut out = std::io::stdout().lock();
                    let _ = terminal::clear_screen(&mut out);
                }
                // The io future pumps the transport (both directions). Drive it
                // concurrently with the select loop; if it ends, the connection
                // is gone.
                let io_fut = async move {
                    if let Some(f) = io {
                        let _ = f.await;
                    } else {
                        pending::<()>().await
                    }
                };
                tokio::pin!(io_fut);

                loop {
                    tokio::select! {
                        _ = &mut io_fut => break Outcome::Reconnect,

                        maybe = server.next() => match maybe {
                            Some(batch) => {
                                if let Some(o) = handle_server_batch(batch) {
                                    break o;
                                }
                            }
                            None => break Outcome::Reconnect,
                        },

                        _ = term.recv() => {
                            let _ = events_tx.send(vec![ClientEvent::Detach]).await;
                            break Outcome::Detached;
                        }

                        _ = winch.recv() => {
                            if let Some((c, r)) = terminal::window_size() {
                                let _ = events_tx.try_send(vec![ClientEvent::Resize((c, r))]);
                            }
                        }

                        chunk = recv_stdin(&mut stdin_rx) => match chunk {
                            Some(bytes) => {
                                let (forward, detached) = if raw_mode {
                                    let mut f = Vec::new();
                                    let d = matcher.feed(&bytes, &mut f);
                                    (f, d)
                                } else {
                                    // Non-TTY stdin: forward verbatim, no detach-key scan.
                                    (bytes.to_vec(), false)
                                };
                                if !forward.is_empty()
                                    && events_tx
                                        .send(vec![ClientEvent::Input(Bytes::from(forward))])
                                        .await
                                        .is_err()
                                {
                                    break Outcome::Reconnect;
                                }
                                if detached {
                                    let _ = events_tx.send(vec![ClientEvent::Detach]).await;
                                    break Outcome::Detached;
                                }
                            }
                            None => {
                                // stdin EOF: stop forwarding, keep streaming output.
                                stdin_rx = None;
                            }
                        }
                    }
                }
            }
        };

        match outcome {
            Outcome::Detached => return Ok(0),
            Outcome::Exited { code, signal } => return Ok(exit_code(code, signal)),
            Outcome::Fatal(msg) => {
                drop(guard);
                eprintln!("cairn: {msg}");
                return Ok(1);
            }
            Outcome::Reconnect => {} // fall through to backoff
        }

        if endpoint.is_gone() {
            eprintln!(
                "cairn: connection lost (daemon socket {} is gone)",
                endpoint.label()
            );
            return Ok(1);
        }
        let now = Instant::now();
        let dl = *deadline.get_or_insert(now + budget);
        if budget != Duration::ZERO && now >= dl {
            eprintln!("cairn: connection lost (gave up reconnecting after {:?})", budget);
            return Ok(1);
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(2));
    }
}

/// Apply one batch of server events to the terminal; return an `Outcome` if the
/// batch is terminal (exit / fatal error / recoverable lag).
fn handle_server_batch(batch: Vec<ServerEvent>) -> Option<Outcome> {
    for ev in batch {
        match ev {
            ServerEvent::Snapshot(b) | ServerEvent::Output(b) => terminal::write_stdout(&b),
            ServerEvent::Exited(st) => {
                return Some(Outcome::Exited { code: st.code, signal: st.signal });
            }
            ServerEvent::Error(e) => {
                if e.code == error_codes::CLIENT_LAGGED {
                    return Some(Outcome::Reconnect);
                }
                return Some(Outcome::Fatal(format!("{}: {}", e.code, e.message)));
            }
        }
    }
    None
}

/// Await the next stdin chunk, or pend forever when there's no stdin source.
async fn recv_stdin(rx: &mut Option<tokio::sync::mpsc::Receiver<Bytes>>) -> Option<Bytes> {
    match rx {
        Some(r) => r.recv().await, // None on channel close (EOF / reader gone)
        None => pending().await,
    }
}

/// Dedicated blocking-read thread on STDIN_FILENO. Never sets O_NONBLOCK on fd 0
/// (that flag is shared with the parent shell). Leaked at process exit.
fn spawn_stdin_reader() -> std::io::Result<tokio::sync::mpsc::Receiver<Bytes>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(64);
    std::thread::Builder::new()
        .name("cairn-stdin".to_string())
        .spawn(move || {
            use std::io::Read;
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        if tx.blocking_send(Bytes::copy_from_slice(&buf[..n])).is_err() {
                            break; // driver gone
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        })?;
    Ok(rx)
}

fn exit_code(code: Option<i32>, signal: Option<u8>) -> i32 {
    code.unwrap_or_else(|| signal.map(|s| 128 + s as i32).unwrap_or(1))
}

/// Reconnect give-up budget from `CAIRN_RECONNECT_TIMEOUT` (humantime; `0`/`off`
/// = retry forever; default 30s).
fn reconnect_budget() -> Duration {
    match std::env::var("CAIRN_RECONNECT_TIMEOUT") {
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("off") => Duration::ZERO,
        Ok(v) => humantime::parse_duration(&v).unwrap_or(Duration::from_secs(30)),
        Err(_) => Duration::from_secs(30),
    }
}
