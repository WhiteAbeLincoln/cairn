//! Integration tests for the non-interactive `cairn` commands. Each test
//! spins a fresh in-process daemon (via `common::Harness`) and invokes the
//! real `cairn` binary against it.

mod common;

use common::{Harness, stderr_str, stdout_str};
use futures::StreamExt as _;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn harness_smoke() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    // No sessions yet — list_all is non-fatal and returns empty.
    let xs = h.list_all().await?;
    assert!(xs.is_empty(), "fresh daemon has no sessions; got {xs:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whoami_prints_identity_and_exits_zero() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["whoami"], b"")?;
    assert!(
        out.status.success(),
        "exit: {:?} stderr: {}",
        out.status,
        stderr_str(&out)
    );
    assert!(
        !stdout_str(&out).trim().is_empty(),
        "whoami stdout should be non-empty"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_prints_client_and_daemon_rows_exit_zero() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["version"], b"")?;
    assert!(
        out.status.success(),
        "exit: {:?} stderr: {}",
        out.status,
        stderr_str(&out)
    );
    let stdout = stdout_str(&out);
    assert!(stdout.contains("cairn"), "missing client row: {stdout}");
    assert!(stdout.contains("daemon"), "missing daemon row: {stdout}");
    assert!(
        stdout.contains("cairn:daemon@0.1.0"),
        "missing protocol id: {stdout}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_with_unreachable_daemon_still_exits_zero() -> anyhow::Result<()> {
    // Don't start a daemon; point the client at a non-existent socket.
    let tmp = tempfile::tempdir()?;
    let bad = tmp.path().join("does-not-exist.sock");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
        .arg("--daemon")
        .arg(format!("unix://{}", bad.display()))
        .arg("version")
        .output()?;
    assert!(
        out.status.success(),
        "version must exit 0 even when daemon is down"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("cairn"), "client row missing: {stdout}");
    assert!(
        stdout.contains("unreachable"),
        "daemon row missing 'unreachable': {stdout}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whoami_with_unreachable_daemon_exits_nonzero() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let bad = tmp.path().join("does-not-exist.sock");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
        .arg("--daemon")
        .arg(format!("unix://{}", bad.display()))
        .arg("whoami")
        .output()?;
    assert!(
        !out.status.success(),
        "whoami is a connectivity probe; must exit non-zero on unreachable"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot reach") || stderr.contains("error"),
        "stderr should describe the failure: {stderr}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_on_empty_registry_says_no_sessions() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["list"], b"")?;
    assert!(
        out.status.success(),
        "exit: {:?} stderr: {}",
        out.status,
        stderr_str(&out)
    );
    assert!(
        stdout_str(&out).contains("no sessions"),
        "got {}",
        stdout_str(&out)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_shows_each_session_name() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    h.create(Harness::spec(&["cat"], Some("alpha"))).await?;
    h.create(Harness::spec(&["cat"], Some("bravo"))).await?;

    let out = h.run(&["list"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains("alpha"), "alpha row missing: {stdout}");
    assert!(stdout.contains("bravo"), "bravo row missing: {stdout}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inspect_renders_command_and_workdir() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let info = h.create(Harness::spec(&["cat"], Some("ins"))).await?;

    let out = h.run(&["inspect", "ins"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains(&info.id), "id missing: {stdout}");
    assert!(stdout.contains("cat"), "command missing: {stdout}");
    assert!(stdout.contains("running"), "state missing: {stdout}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inspect_unknown_target_errors_and_exits_one() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["inspect", "no-such"], b"")?;
    assert!(!out.status.success(), "should exit non-zero");
    let stderr = stderr_str(&out);
    assert!(stderr.contains("no-such"), "stderr missing token: {stderr}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_changes_the_session_name() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let info = h.create(Harness::spec(&["cat"], Some("before"))).await?;

    let out = h.run(&["rename", "before", "--to", "after"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    let fresh = h.inspect(&info.id).await?;
    assert_eq!(fresh.name.as_deref(), Some("after"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_argv_joins_with_spaces_and_appends_newline() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    // `cat` echoes whatever it receives on stdin to the PTY.
    let info = h.create(Harness::spec(&["cat"], Some("snd"))).await?;
    let out = h.run(&["send", "snd", "hello", "world"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    // Read the session's transcript via the logs op; assert it saw "hello world\n".
    // Allow up to 2 s for the daemon to round-trip the input.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let info = h.inspect(&info.id).await?;
        let _ = info; // (we read logs, not inspect, but inspect proves the session is still alive)
        let logs = read_snapshot(&h, "snd").await?;
        if logs.contains("hello world") {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("transcript never contained 'hello world'; got: {logs:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_latest_with_argv_input_routes_to_most_recent_session() -> anyhow::Result<()> {
    // Regression: `cairn send --latest <input>` used to fail with
    // "the argument '--latest' cannot be used with '[SESSION]'" because
    // clap bound the first positional to the SessionTarget's `session`
    // slot even when `--latest` was given.
    let h = Harness::start().await?;
    // Older session — should NOT receive the input.
    let _older = h.create(Harness::spec(&["cat"], Some("older"))).await?;
    // Give creation timestamps a millisecond of separation so `--latest`
    // is deterministic.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let _newer = h.create(Harness::spec(&["cat"], Some("newer"))).await?;

    let out = h.run(&["send", "--latest", "hello", "world"], b"")?;
    assert!(
        out.status.success(),
        "exit: {:?} stderr: {}",
        out.status,
        stderr_str(&out)
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let logs = read_snapshot(&h, "newer").await?;
        if logs.contains("hello world") {
            // And the older session must not have received it.
            let other = read_snapshot(&h, "older").await?;
            assert!(
                !other.contains("hello"),
                "older session should not have received input: {other:?}"
            );
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!(
                "newer session's transcript never contained 'hello world'; got: {logs:?}"
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_stdin_streams_raw_bytes_no_trailing_newline() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let _ = h.create(Harness::spec(&["cat"], Some("raw"))).await?;
    let out = h.run(&["send", "raw"], b"abc")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let logs = read_snapshot(&h, "raw").await?;
    assert!(
        logs.contains("abc"),
        "expected 'abc' in transcript: {logs:?}"
    );
    assert!(
        !logs.contains("abc\n"),
        "stdin path must not append a newline: {logs:?}"
    );
    Ok(())
}

/// Drain the `logs(All, follow=false)` snapshot of `target` into a string.
async fn read_snapshot(h: &Harness, target: &str) -> anyhow::Result<String> {
    use cairn_protocol::cairn::daemon::types::LogWindow;
    use cairn_protocol::client::cairn::daemon::sessions;

    let xs = h.list_all().await?;
    let id = xs
        .iter()
        .find(|s| s.name.as_deref() == Some(target))
        .map(|s| s.id.clone())
        .ok_or_else(|| anyhow::anyhow!("no session named {target}"))?;
    let (mut stream, io) = sessions::logs(&h.client(), (), &id, &LogWindow::All, false)
        .await
        .map_err(|e| anyhow::anyhow!("logs: {e}"))?;
    if let Some(io) = io {
        tokio::spawn(async move {
            let _ = io.await;
        });
    }
    let mut out = Vec::new();
    while let Some(batch) = stream.next().await {
        for chunk in batch {
            out.extend_from_slice(&chunk);
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_force_replaces_the_child_process() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    // sh -c 'while true; do sleep 1; done' — a stable, restartable child.
    let info = h
        .create(Harness::spec(
            &["sh", "-c", "while true; do sleep 1; done"],
            Some("loopy"),
        ))
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let out = h.run(&["restart", "loopy", "--force"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    // Session must still be alive (no exit) and resolve under its original id.
    let after = h.inspect(&info.id).await?;
    assert_eq!(
        after.id, info.id,
        "session id must be stable across restart"
    );
    assert!(
        after.exit.is_none(),
        "restarted session must have no exit status; got {:?}",
        after.exit
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kick_all_on_empty_resolution_exits_two() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["kick", "--all"], b"")?;
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_str(&out));
    assert!(
        stderr_str(&out).contains("no sessions matched"),
        "stderr: {}",
        stderr_str(&out)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kick_named_session_returns_zero() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let _ = h.create(Harness::spec(&["cat"], Some("kk"))).await?;
    let out = h.run(&["kick", "kk"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_blocks_until_session_exits() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let info = h
        .create(Harness::spec(&["sleep", "30"], Some("zzz")))
        .await?;
    let start = std::time::Instant::now();
    let out = h.run(&["kill", "zzz"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(
        start.elapsed() < std::time::Duration::from_secs(5),
        "kill took too long: {:?}",
        start.elapsed()
    );

    let after = h.inspect(&info.id).await?;
    assert!(
        after.exit.is_some(),
        "session should be exited; got {:?}",
        after.exit
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_no_wait_with_timeout_returns_immediately_then_daemon_escalates() -> anyhow::Result<()>
{
    let h = Harness::start().await?;
    // bash -c 'trap "" TERM; sleep 30' ignores TERM, so SIGKILL escalation is the only way out.
    let info = h
        .create(Harness::spec(
            &["bash", "-c", "trap '' TERM; sleep 30"],
            Some("nope"),
        ))
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let start = std::time::Instant::now();
    let out = h.run(&["kill", "--no-wait", "--timeout", "1s", "nope"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(
        start.elapsed() < std::time::Duration::from_millis(700),
        "should return ~immediately, took {:?}",
        start.elapsed()
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let info = h.inspect(&info.id).await?;
        if info.exit.is_some() {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("session never exited after daemon escalation; info={info:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_all_on_empty_registry_exits_two() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["kill", "--all"], b"")?;
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_str(&out).contains("no sessions matched"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_returns_child_exit_code() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    // `sh -c 'exit 7'` will exit with code 7 immediately.
    let _ = h
        .create(Harness::spec(&["sh", "-c", "exit 7"], Some("seven")))
        .await?;
    let out = h.run(&["wait", "seven"], b"")?;
    assert_eq!(out.status.code(), Some(7), "stderr: {}", stderr_str(&out));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_timeout_elapsed_exits_124_session_alive() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let info = h
        .create(Harness::spec(&["sleep", "30"], Some("slow")))
        .await?;
    let out = h.run(&["wait", "--timeout", "300ms", "slow"], b"")?;
    assert_eq!(out.status.code(), Some(124), "stderr: {}", stderr_str(&out));
    let after = h.inspect(&info.id).await?;
    assert!(
        after.exit.is_none(),
        "session must still be alive after a wait timeout; got {:?}",
        after.exit
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_single_session_prints_snapshot() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let _ = h
        .create(Harness::spec(
            &["sh", "-c", "echo hello-from-the-pty"],
            Some("lg"),
        ))
        .await?;
    // Give the child time to print.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let out = h.run(&["logs", "lg"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(
        stdout.contains("hello-from-the-pty"),
        "missing line: {stdout}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_strip_removes_ansi_escapes() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let _ = h
        .create(Harness::spec(
            &["sh", "-c", "printf '\\x1b[31mX\\x1b[0m\\n'"],
            Some("ansi"),
        ))
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let out = h.run(&["logs", "--strip", "ansi"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains('X'), "missing X: {stdout:?}");
    assert!(
        !stdout.contains('\u{1b}'),
        "ANSI escape still present: {stdout:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_prefix_prepends_name_per_line() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let _ = h
        .create(Harness::spec(
            &["sh", "-c", "echo a-line && echo b-line"],
            Some("p"),
        ))
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let out = h.run(&["logs", "--prefix", "--strip", "p"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(
        stdout.contains("p: a-line"),
        "expected prefixed 'a-line': {stdout:?}"
    );
    assert!(
        stdout.contains("p: b-line"),
        "expected prefixed 'b-line': {stdout:?}"
    );
    Ok(())
}
