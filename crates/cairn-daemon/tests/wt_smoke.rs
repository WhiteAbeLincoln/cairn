//! Smoke tests: daemon with a WebTransport listener, client connects via QUIC.
//!
//! Validates the end-to-end WT pipeline: daemon binds a QUIC endpoint with a
//! self-signed cert, client connects with cert validation disabled (test-only),
//! and wRPC invocations round-trip over the WebTransport connection.

mod common;

use common::DaemonHarness;

/// Build a `wrpc_transport_web::Client` that connects to the WT endpoint.
///
/// Uses `with_no_cert_validation()` because the daemon generates Ed25519
/// self-signed certs, while `with_server_certificate_hashes()` requires
/// ECDSA P-256. This is acceptable for integration tests.
async fn build_wt_client(addr: std::net::SocketAddr) -> wrpc_transport_web::Client {
    let config = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_no_cert_validation()
        .build();

    let endpoint = wtransport::Endpoint::client(config).unwrap();
    let url = format!("https://{addr}");
    let conn = endpoint.connect(&url).await.unwrap();
    wrpc_transport_web::Client::from(conn)
}

#[tokio::test]
async fn wt_version_round_trip() {
    let harness = DaemonHarness::start_with_wt().await;
    let wt_addr = harness.wt_addr.unwrap();

    let wt_client = build_wt_client(wt_addr).await;

    let info = cairn_protocol::client::cairn::daemon::meta::version(&wt_client, ())
        .await
        .expect("version via WT");
    assert!(
        info.daemon.starts_with("cairn-daemon/"),
        "unexpected daemon version: {}",
        info.daemon
    );
    assert_eq!(info.protocol, "cairn:daemon@0.1.0");
}

#[tokio::test]
async fn uds_and_wt_coexist() {
    let harness = DaemonHarness::start_with_wt().await;
    let wt_addr = harness.wt_addr.unwrap();

    // Query version over UDS.
    let uds_client = harness.client();
    let uds_info = cairn_protocol::client::cairn::daemon::meta::version(&uds_client, ())
        .await
        .expect("version via UDS");

    // Query version over WebTransport.
    let wt_client = build_wt_client(wt_addr).await;
    let wt_info = cairn_protocol::client::cairn::daemon::meta::version(&wt_client, ())
        .await
        .expect("version via WT");

    // Both transports talk to the same daemon, so the version must match.
    assert_eq!(uds_info.daemon, wt_info.daemon);
    assert_eq!(uds_info.protocol, wt_info.protocol);
}
