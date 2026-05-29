//! `cairn wait <target> [--timeout T]`: block until exit, propagate the
//! child's exit code (`128+signal` if killed), or exit 124 on timeout.

use std::time::Duration;

use anyhow::Result;
use cairn_protocol::cairn::daemon::types::ExitStatus;
use cairn_protocol::client::cairn::daemon::sessions;

use crate::cli::SessionTarget;
use crate::connect::Endpoint;
use crate::targets;

const TIMEOUT_EXIT_CODE: i32 = 124;

pub async fn run(
    endpoint: &Endpoint,
    target: &SessionTarget,
    timeout: Option<Duration>,
) -> Result<i32> {
    let resolved = match targets::resolve_one(endpoint, target).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let client = endpoint.client();
    // `sessions::wait` returns `Result<(future, Option<io_future>)>`: the
    // first future yields the ExitStatus, the second drives the underlying
    // transport and must be spawned for the call to make progress.
    let (future, io) = match sessions::wait(&client, (), &resolved.id).await {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    if let Some(io) = io {
        tokio::spawn(async move {
            let _ = io.await;
        });
    }

    let status = match timeout {
        Some(d) => match tokio::time::timeout(d, future).await {
            Ok(s) => s,
            Err(_) => return Ok(TIMEOUT_EXIT_CODE),
        },
        None => future.await,
    };
    Ok(exit_code_of(&status))
}

fn exit_code_of(status: &ExitStatus) -> i32 {
    if let Some(c) = status.code {
        return c;
    }
    if let Some(s) = status.signal {
        return 128 + i32::from(s);
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(code: Option<i32>, signal: Option<u8>) -> ExitStatus {
        ExitStatus {
            code,
            signal,
            unix_ms: 0,
            reason: None,
        }
    }

    #[test]
    fn exit_code_uses_code_when_present() {
        assert_eq!(exit_code_of(&st(Some(7), None)), 7);
    }

    #[test]
    fn exit_code_uses_128_plus_signal_when_killed() {
        assert_eq!(exit_code_of(&st(None, Some(9))), 137);
    }

    #[test]
    fn exit_code_falls_back_to_one() {
        assert_eq!(exit_code_of(&st(None, None)), 1);
    }
}
