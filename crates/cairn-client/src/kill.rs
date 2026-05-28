//! `cairn kill`: signal one-or-more sessions, optionally wait for exit and
//! optionally arm a daemon-side SIGKILL escalation.

use std::time::Duration;

use anyhow::Result;
use cairn_protocol::cairn::daemon::types::{Signal as WireSignal, SignalName as WireSignalName};
use cairn_protocol::client::cairn::daemon::sessions;
use futures::stream::{FuturesUnordered, StreamExt as _};

use crate::cli::{SessionTargets, Signal, SignalName};
use crate::connect::Endpoint;
use crate::targets;

pub async fn run(
    endpoint: &Endpoint,
    sessions_arg: &SessionTargets,
    signal: Signal,
    no_wait: bool,
    timeout: Option<Duration>,
) -> Result<i32> {
    let resolved = match targets::resolve_many(endpoint, sessions_arg).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let mut exit_code = 0;
    for t in &resolved.unresolved {
        eprintln!("error: {t}: no session matches");
        exit_code = 1;
    }
    if resolved.matched.is_empty() {
        eprintln!("no sessions matched");
        return Ok(2);
    }
    let grace_ms = grace_ms(timeout);
    let wire_sig = into_wire_signal(signal);
    let client = endpoint.client();
    let mut tasks = FuturesUnordered::new();
    for t in &resolved.matched {
        let client = client.clone();
        let id = t.id.clone();
        let token = t.name.clone().unwrap_or_else(|| t.id.clone());
        tasks.push(async move {
            let sig_result = sessions::kill(&client, (), &id, &wire_sig, grace_ms).await;
            let wait_result = if no_wait {
                Ok(())
            } else {
                // `wait` returns (future, Option<io_future>); drive both.
                match sessions::wait(&client, (), &id).await {
                    Ok((future, io)) => {
                        if let Some(io) = io {
                            tokio::spawn(async move {
                                let _ = io.await;
                            });
                        }
                        future.await;
                        Ok(())
                    }
                    Err(e) => Err(anyhow::anyhow!("{e}")),
                }
            };
            (token, sig_result, wait_result)
        });
    }
    while let Some((token, sig_result, wait_result)) = tasks.next().await {
        match sig_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                eprintln!("error: {}: {}: {}", token, e.code, e.message);
                exit_code = 1;
                continue;
            }
            Err(e) => {
                eprintln!("error: {token}: {e}");
                exit_code = 1;
                continue;
            }
        }
        if let Err(e) = wait_result {
            eprintln!("error: {token}: wait: {e}");
            exit_code = 1;
        }
    }
    Ok(exit_code)
}

fn grace_ms(timeout: Option<Duration>) -> Option<u32> {
    timeout.map(|d| u32::try_from(d.as_millis()).unwrap_or(u32::MAX))
}

fn into_wire_signal(sig: Signal) -> WireSignal {
    match sig {
        Signal::Named(name) => WireSignal::Named(into_wire_name(name)),
        Signal::Number(n) => WireSignal::Numbered(n),
    }
}

fn into_wire_name(name: SignalName) -> WireSignalName {
    use SignalName::*;
    match name {
        Hup => WireSignalName::Hup,
        Int => WireSignalName::Int,
        Quit => WireSignalName::Quit,
        Ill => WireSignalName::Ill,
        Trap => WireSignalName::Trap,
        Abrt => WireSignalName::Abrt,
        Bus => WireSignalName::Bus,
        Fpe => WireSignalName::Fpe,
        Kill => WireSignalName::Kill,
        Usr1 => WireSignalName::Usr1,
        Segv => WireSignalName::Segv,
        Usr2 => WireSignalName::Usr2,
        Pipe => WireSignalName::Pipe,
        Alrm => WireSignalName::Alrm,
        Term => WireSignalName::Term,
        Chld => WireSignalName::Chld,
        Cont => WireSignalName::Cont,
        Stop => WireSignalName::Stop,
        Tstp => WireSignalName::Tstp,
        Ttin => WireSignalName::Ttin,
        Ttou => WireSignalName::Ttou,
        Urg => WireSignalName::Urg,
        Xcpu => WireSignalName::Xcpu,
        Xfsz => WireSignalName::Xfsz,
        Vtalrm => WireSignalName::Vtalrm,
        Prof => WireSignalName::Prof,
        Winch => WireSignalName::Winch,
        Io => WireSignalName::Io,
        Sys => WireSignalName::Sys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grace_ms_none_when_no_timeout() {
        assert_eq!(grace_ms(None), None);
    }

    #[test]
    fn grace_ms_round_trips_small_durations() {
        assert_eq!(grace_ms(Some(Duration::from_millis(1500))), Some(1500));
    }

    #[test]
    fn grace_ms_saturates_at_u32_max() {
        let huge = Duration::from_secs(100 * 24 * 60 * 60);
        assert_eq!(grace_ms(Some(huge)), Some(u32::MAX));
    }
}
