//! Integration tests for the non-interactive `cairn` commands. Each test
//! spins a fresh in-process daemon (via `common::Harness`) and invokes the
//! real `cairn` binary against it.

mod common;

use common::{Harness, stderr_str, stdout_str};

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
    assert!(out.status.success(), "exit: {:?} stderr: {}", out.status, stderr_str(&out));
    assert!(!stdout_str(&out).trim().is_empty(), "whoami stdout should be non-empty");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_prints_client_and_daemon_rows_exit_zero() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["version"], b"")?;
    assert!(out.status.success(), "exit: {:?} stderr: {}", out.status, stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains("cairn"), "missing client row: {stdout}");
    assert!(stdout.contains("daemon"), "missing daemon row: {stdout}");
    assert!(stdout.contains("cairn:daemon@0.1.0"), "missing protocol id: {stdout}");
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
    assert!(out.status.success(), "version must exit 0 even when daemon is down");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("cairn"), "client row missing: {stdout}");
    assert!(stdout.contains("unreachable"), "daemon row missing 'unreachable': {stdout}");
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
    assert!(!out.status.success(), "whoami is a connectivity probe; must exit non-zero on unreachable");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot reach") || stderr.contains("error"), "stderr should describe the failure: {stderr}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_on_empty_registry_says_no_sessions() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["list"], b"")?;
    assert!(out.status.success(), "exit: {:?} stderr: {}", out.status, stderr_str(&out));
    assert!(stdout_str(&out).contains("no sessions"), "got {}", stdout_str(&out));
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
