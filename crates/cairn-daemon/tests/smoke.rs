//! Subprocess smoke test: spawn the real `cairn-daemon` binary and hit it.

use std::time::Duration;

#[tokio::test]
async fn binary_starts_and_serves_version() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("cairn").join("cairn.sock");

    // `CARGO_BIN_EXE_cairn-daemon` is set by cargo for integration tests.
    let bin = env!("CARGO_BIN_EXE_cairn-daemon");
    let mut child = tokio::process::Command::new(bin)
        .env("CAIRN_SOCKET", &socket)
        .env("CAIRN_LOG", "warn")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn cairn-daemon");

    // Wait for the socket to appear.
    let mut ready = false;
    for _ in 0..100 {
        if socket.exists() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ready, "daemon did not create its socket");

    let client = wrpc_transport::unix::Client::from(socket.clone());
    let info = cairn_protocol::client::cairn::daemon::meta::version(&client, ())
        .await
        .expect("version invocation");
    assert!(info.daemon.starts_with("cairn-daemon/"));

    // Graceful shutdown via SIGTERM.
    let pid = child.id().expect("pid");
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("SIGTERM");
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
}
