//! Smoke tests: daemon with a WebTransport listener, client connects via QUIC.
//!
//! Validates the end-to-end WT pipeline: daemon binds a QUIC endpoint with a
//! self-signed ECDSA P-256 cert, client connects with cert hash pinning via
//! `serverCertificateHashes`, and wRPC invocations round-trip over the
//! WebTransport connection.

mod common;

use common::DaemonHarness;

/// Build a `wrpc_transport_web::Client` that connects to the WT endpoint,
/// pinning the server cert by its SHA-256 hash.
///
/// Uses `with_server_certificate_hashes()` as required by the W3C WebTransport
/// spec, which mandates ECDSA P-256 or P-384 certs for hash pinning.
async fn build_wt_client(
    addr: std::net::SocketAddr,
    cert_hash_hex: &str,
) -> wrpc_transport_web::Client {
    let mut hash_bytes = [0u8; 32];
    for (i, byte) in hash_bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&cert_hash_hex[i * 2..i * 2 + 2], 16).unwrap();
    }

    let config = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes(vec![wtransport::tls::Sha256Digest::new(hash_bytes)])
        .build();

    let endpoint = wtransport::Endpoint::client(config).unwrap();
    let conn = endpoint.connect(format!("https://{addr}")).await.unwrap();
    wrpc_transport_web::Client::from(conn)
}

#[tokio::test]
async fn wt_version_round_trip() {
    let harness = DaemonHarness::start_with_wt().await;
    let wt_addr = harness.wt_addr.unwrap();
    let cert_hash = harness.cert_hash.as_deref().unwrap();

    let wt_client = build_wt_client(wt_addr, cert_hash).await;

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
    let cert_hash = harness.cert_hash.as_deref().unwrap();

    // Query version over UDS.
    let uds_client = harness.client();
    let uds_info = cairn_protocol::client::cairn::daemon::meta::version(&uds_client, ())
        .await
        .expect("version via UDS");

    // Query version over WebTransport.
    let wt_client = build_wt_client(wt_addr, cert_hash).await;
    let wt_info = cairn_protocol::client::cairn::daemon::meta::version(&wt_client, ())
        .await
        .expect("version via WT");

    // Both transports talk to the same daemon, so the version must match.
    assert_eq!(uds_info.daemon, wt_info.daemon);
    assert_eq!(uds_info.protocol, wt_info.protocol);
}
