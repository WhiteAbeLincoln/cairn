use std::time::Duration;

use bytes::Bytes;
use cairn_http_proxy::{
    BodyChunk, InterceptorAction, InterceptorEvent, ProxyAuthority, ProxySession,
    ProxySessionConfig, ResponseHead, ResponseStart, Route,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

#[tokio::test]
async fn https_connect_uses_a_leaf_certificate_signed_by_the_cairn_ca() {
    let temp = tempfile::tempdir().unwrap();
    let authority = ProxyAuthority::create(temp.path()).unwrap();
    let config = ProxySessionConfig {
        routes: vec![Route {
            methods: vec![],
            host: None,
            path_prefix: None,
        }],
        interceptor_wait: Duration::from_secs(1),
        decision_timeout: Duration::from_secs(1),
        ..ProxySessionConfig::default()
    };
    let proxy = ProxySession::start(&authority, config).await.unwrap();
    let mut interceptor = proxy.attach_interceptor().unwrap();
    let ca_path = authority.ca_path().to_path_buf();
    let addr = proxy.addr();

    let client = tokio::spawn(async move {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"CONNECT bridge.example:443 HTTP/1.1\r\nHost: bridge.example:443\r\n\r\n")
            .await
            .unwrap();
        let connect_response = read_http_head(&mut stream).await;
        assert!(connect_response.contains("200"), "{connect_response}");

        let file = std::fs::File::open(ca_path).unwrap();
        let mut reader = std::io::BufReader::new(file);
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut reader) {
            roots.add(cert.unwrap()).unwrap();
        }
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let tls_config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(tls_config));
        let server_name =
            rustls::pki_types::ServerName::try_from("bridge.example".to_string()).unwrap();
        let mut tls = connector.connect(server_name, stream).await.unwrap();
        tls.write_all(b"GET /events HTTP/1.1\r\nHost: bridge.example\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        tls.read_to_end(&mut response).await.unwrap();
        String::from_utf8(response).unwrap()
    });

    let InterceptorEvent::Request(request) =
        tokio::time::timeout(Duration::from_secs(2), interceptor.recv())
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("expected an intercepted HTTPS request");
    };
    assert_eq!(request.head.method, "GET");
    assert!(
        request.head.uri.ends_with("/events"),
        "{}",
        request.head.uri
    );
    proxy
        .apply_action(InterceptorAction::ResponseStart(ResponseStart {
            id: request.id,
            head: ResponseHead {
                status: 200,
                version: "HTTP/1.1".into(),
                headers: vec![],
            },
        }))
        .await
        .unwrap();
    proxy
        .apply_action(InterceptorAction::ResponseBody(BodyChunk {
            id: request.id,
            bytes: Bytes::from_static(b"secure synthetic response"),
        }))
        .await
        .unwrap();
    proxy
        .apply_action(InterceptorAction::ResponseEnd(request.id))
        .await
        .unwrap();

    let response = client.await.unwrap();
    assert!(response.contains("200 OK"), "{response}");
    assert!(response.contains("secure synthetic response"), "{response}");
    proxy.shutdown().await;
}

#[tokio::test]
async fn matched_request_without_an_interceptor_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let authority = ProxyAuthority::create(temp.path()).unwrap();
    let config = ProxySessionConfig {
        routes: vec![Route {
            methods: vec![],
            host: None,
            path_prefix: None,
        }],
        interceptor_wait: Duration::from_millis(20),
        decision_timeout: Duration::from_millis(20),
        ..ProxySessionConfig::default()
    };
    let proxy = ProxySession::start(&authority, config).await.unwrap();

    let response = request_through_proxy(proxy.addr(), "http://bridge.example/audit").await;
    assert!(response.contains("502 Bad Gateway"), "{response}");
    assert!(
        response.contains("session interceptor is unavailable"),
        "{response}"
    );

    let (snapshot, _) = proxy.subscribe_observations();
    assert_eq!(snapshot.len(), 1);
    assert_eq!(
        snapshot[0]
            .failure
            .as_ref()
            .map(|failure| failure.code.as_str()),
        Some("proxy.interceptor_unavailable")
    );
    proxy.shutdown().await;
}

#[tokio::test]
async fn unmatched_request_is_forwarded_to_its_origin_and_observed() {
    let (origin_addr, origin_task) = origin_server(b"origin response").await;
    let temp = tempfile::tempdir().unwrap();
    let authority = ProxyAuthority::create(temp.path()).unwrap();
    let proxy = ProxySession::start(&authority, ProxySessionConfig::default())
        .await
        .unwrap();
    let (_snapshot, mut observations) = proxy.subscribe_observations();

    let response =
        request_through_proxy(proxy.addr(), &format!("http://{origin_addr}/hello?audit=1")).await;
    assert!(response.contains("200 OK"), "{response}");
    assert!(response.ends_with("origin response"), "{response}");

    let mut saw_start = false;
    let mut saw_complete = false;
    for _ in 0..12 {
        let Ok(Ok(event)) =
            tokio::time::timeout(Duration::from_millis(250), observations.recv()).await
        else {
            break;
        };
        saw_start |= matches!(event, cairn_http_proxy::ObservationEvent::RequestStart(_));
        saw_complete |= matches!(event, cairn_http_proxy::ObservationEvent::Completed(_, _));
        if saw_start && saw_complete {
            break;
        }
    }
    let (snapshot, _) = proxy.subscribe_observations();
    assert!(saw_start);
    assert!(saw_complete, "snapshot after response: {snapshot:#?}");

    proxy.shutdown().await;
    origin_task.await.unwrap();
}

#[tokio::test]
async fn matched_request_can_receive_a_streamed_synthetic_response() {
    let temp = tempfile::tempdir().unwrap();
    let authority = ProxyAuthority::create(temp.path()).unwrap();
    let config = ProxySessionConfig {
        routes: vec![Route {
            methods: vec!["GET".into()],
            host: Some("bridge.example".into()),
            path_prefix: Some("/events".into()),
        }],
        interceptor_wait: Duration::from_secs(1),
        decision_timeout: Duration::from_secs(1),
        ..ProxySessionConfig::default()
    };
    let proxy = ProxySession::start(&authority, config).await.unwrap();
    let mut interceptor = proxy.attach_interceptor().unwrap();

    let addr = proxy.addr();
    let client =
        tokio::spawn(
            async move { request_through_proxy(addr, "http://bridge.example/events").await },
        );
    let InterceptorEvent::Request(request) =
        tokio::time::timeout(Duration::from_secs(2), interceptor.recv())
            .await
            .unwrap()
            .unwrap()
    else {
        panic!("expected an intercepted request");
    };
    assert_eq!(request.head.method, "GET");
    assert_eq!(request.head.uri, "http://bridge.example/events");

    proxy
        .apply_action(InterceptorAction::ResponseStart(ResponseStart {
            id: request.id,
            head: ResponseHead {
                status: 200,
                version: "HTTP/1.1".into(),
                headers: vec![(
                    "content-type".into(),
                    Bytes::from_static(b"text/event-stream"),
                )],
            },
        }))
        .await
        .unwrap();
    proxy
        .apply_action(InterceptorAction::ResponseBody(BodyChunk {
            id: request.id,
            bytes: Bytes::from_static(b"data: first\n\n"),
        }))
        .await
        .unwrap();
    proxy
        .apply_action(InterceptorAction::ResponseBody(BodyChunk {
            id: request.id,
            bytes: Bytes::from_static(b"data: second\n\n"),
        }))
        .await
        .unwrap();
    proxy
        .apply_action(InterceptorAction::ResponseEnd(request.id))
        .await
        .unwrap();

    let response = client.await.unwrap();
    assert!(
        response
            .to_ascii_lowercase()
            .contains("content-type: text/event-stream"),
        "{response}"
    );
    let first = response.find("data: first").unwrap();
    let second = response.find("data: second").unwrap();
    assert!(first < second, "{response}");
    proxy.shutdown().await;
}

async fn origin_server(body: &'static [u8]) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = vec![0; 4096];
        let _ = stream.read(&mut request).await.unwrap();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        stream.shutdown().await.unwrap();
    });
    (addr, task)
}

async fn request_through_proxy(proxy: std::net::SocketAddr, uri: &str) -> String {
    let authority = uri
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    let mut stream = tokio::net::TcpStream::connect(proxy).await.unwrap();
    stream
        .write_all(
            format!("GET {uri} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    String::from_utf8(response).unwrap()
}

async fn read_http_head(stream: &mut tokio::net::TcpStream) -> String {
    let mut response = Vec::new();
    let mut byte = [0_u8; 1];
    while !response.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.unwrap();
        response.push(byte[0]);
    }
    String::from_utf8(response).unwrap()
}
