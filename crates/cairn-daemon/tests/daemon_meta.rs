//! Integration tests for the `meta` interface: version, whoami, authenticate.

mod common;

use cairn_protocol as bindings;
use common::DaemonHarness;

#[tokio::test]
async fn version_reports_daemon_and_protocol() {
    let h = DaemonHarness::start().await;
    let info = bindings::client::cairn::daemon::meta::version(&h.client(), ())
        .await
        .expect("version invocation");
    assert!(
        info.daemon.starts_with("cairn-daemon/"),
        "daemon version prefix mismatch: {}",
        info.daemon
    );
    assert_eq!(info.protocol, "cairn:daemon@0.1.0");
}

#[tokio::test]
async fn whoami_returns_caller_uid() {
    let h = DaemonHarness::start().await;
    let who = bindings::client::cairn::daemon::meta::whoami(&h.client(), ())
        .await
        .expect("whoami invocation")
        .expect("whoami result");
    let uid = nix::unistd::geteuid();
    let expected = nix::unistd::User::from_uid(uid)
        .ok()
        .flatten()
        .map(|u| u.name)
        .unwrap_or_else(|| uid.to_string());
    assert_eq!(who, expected, "whoami should report the caller's username");
}

#[tokio::test]
async fn authenticate_is_ok_on_uds() {
    let h = DaemonHarness::start().await;
    let r = bindings::client::cairn::daemon::meta::authenticate(&h.client(), (), "ignored")
        .await
        .expect("authenticate invocation");
    assert!(r.is_ok(), "UDS auth should always succeed: {r:?}");
}
