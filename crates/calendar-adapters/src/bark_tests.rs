use super::*;
use calendar_application::{SecretBytes, SecurityError};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tokio_rustls::{
    rustls::{self, pki_types::PrivatePkcs8KeyDer},
    TlsAcceptor,
};

const KEY: &str = "synthetic-device-key-33";
struct Credentials {
    calls: AtomicUsize,
    error: Option<SecurityError>,
}
impl CredentialProvider for Credentials {
    async fn credential(&self, id: &str, kind: &str) -> Result<SecretBytes, SecurityError> {
        assert_eq!((id, kind), ("handle-33", "bark"));
        self.calls.fetch_add(1, Ordering::SeqCst);
        match self.error {
            Some(e) => Err(e),
            None => Ok(SecretBytes::new(KEY.as_bytes().to_vec())),
        }
    }
}
fn envelope() -> DeliveryEnvelope {
    let mut event: calendar_application::EventV1 =
        serde_json::from_str(include_str!("../../../examples/event-v1.json")).unwrap();
    event.title = "提醒 \"会议\"".into();
    event.description = Some("line one\nline two".into());
    DeliveryEnvelope {
        job_id: "job".into(),
        request_id: "request".into(),
        output_id: "output-33".into(),
        event,
        original_key: "single".into(),
        due_at: 0,
    }
}
struct Server {
    origin: String,
    address: SocketAddr,
    root: reqwest::Certificate,
    received: mpsc::Receiver<String>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server(replies: Vec<Vec<u8>>) -> Server {
    let signed = rcgen::generate_simple_self_signed(vec!["bark.example".into()]).unwrap();
    let root = reqwest::Certificate::from_der(signed.cert.der()).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![signed.cert.der().clone()],
        PrivatePkcs8KeyDer::from(signed.signing_key.serialize_der()).into(),
    )
    .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("https://bark.example:{}", address.port());
    let (tx, received) = mpsc::channel(32);
    let task = tokio::spawn(async move {
        for reply in replies {
            let (socket, _) = listener.accept().await.unwrap();
            let mut tls = acceptor.accept(socket).await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                bytes.push(tls.read_u8().await.unwrap());
                assert!(bytes.len() < 16384);
            }
            let headers = String::from_utf8(bytes.clone()).unwrap();
            let len: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|s| s.trim().parse().unwrap())
                })
                .unwrap();
            let mut body = vec![0; len];
            tls.read_exact(&mut body).await.unwrap();
            bytes.extend(body);
            tx.send(String::from_utf8(bytes).unwrap()).await.unwrap();
            if reply == b"stall" {
                // Client timeout releases the read; no arbitrary sleeps.
                let _ = tls.read_u8().await;
            } else if !reply.is_empty() {
                let _ = tls.write_all(&reply).await;
                let _ = tls.flush().await;
                if reply.ends_with(b"stall-body") {
                    let _ = tls.read_u8().await;
                }
            }
            let _ = tls.shutdown().await;
        }
    });
    Server {
        origin,
        address,
        root,
        received,
        task,
    }
}
fn reply(status: u16, headers: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    )
    .into_bytes()
}
fn output(server: &Server, error: Option<SecurityError>) -> BarkOutput<Credentials> {
    let mut output = BarkOutput::new(
        Arc::new(Credentials {
            calls: AtomicUsize::new(0),
            error,
        }),
        "output-33",
        "handle-33",
        &server.origin,
        Duration::from_millis(500),
    )
    .unwrap();
    output.test_address = Some(server.address);
    output.root = Some(server.root.clone());
    output
}
#[tokio::test]
async fn ac1_https_json_key_only_in_body_and_handle_reloaded_per_attempt() {
    let mut server = server(vec![reply(200, "", r#"{"code":200}"#); 2]).await;
    let output = output(&server, None);
    assert!(!format!("{output:?}").contains(KEY));
    for _ in 0..2 {
        assert_eq!(output.deliver(envelope()).await, DeliveryOutcome::Accepted);
        let request = server.received.recv().await.unwrap();
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /push HTTP/1.1\r\n"));
        assert!(headers
            .to_lowercase()
            .contains("content-type: application/json"));
        assert!(!headers.contains(KEY));
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"device_key": KEY, "title": "提醒 \"会议\"", "body": "line one\nline two"})
        );
    }
    assert_eq!(output.credentials.calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn ac3_lost_response_then_explicit_retry_can_duplicate() {
    let mut server = server(vec![Vec::new(), reply(200, "", r#"{"code":200}"#)]).await;
    let output = output(&server, None);
    assert_eq!(
        output.deliver(envelope()).await,
        DeliveryOutcome::Unknown {
            retry_after_ms: None
        }
    );
    assert_eq!(output.deliver(envelope()).await, DeliveryOutcome::Accepted);
    assert!(server.received.recv().await.unwrap().contains(KEY));
    assert!(server.received.recv().await.unwrap().contains(KEY));
}
#[tokio::test]
async fn ac2_timeout_is_unknown_without_hidden_retry() {
    let mut server = server(vec![b"stall".to_vec()]).await;
    let output = output(&server, None);
    assert_eq!(
        output.deliver(envelope()).await,
        DeliveryOutcome::Unknown {
            retry_after_ms: None
        }
    );
    assert!(server.received.recv().await.unwrap().contains(KEY));
    assert_eq!(output.credentials.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn ac2_ac3_http_classification_and_retry_after_survive_body_failures() {
    let cases = [
        (
            reply(429, "Retry-After: 7\r\n", ""),
            DeliveryOutcome::Retryable {
                retry_after_ms: Some(7000),
            },
        ),
        (
            reply(503, "Retry-After: 9\r\n", r#"{"code":200}"#),
            DeliveryOutcome::Retryable {
                retry_after_ms: Some(9000),
            },
        ),
        (
            reply(401, "", r#"{"code":200}"#),
            DeliveryOutcome::PermanentFailure,
        ),
        (
            reply(200, "Retry-After: 4\r\n", "malformed"),
            DeliveryOutcome::Unknown {
                retry_after_ms: Some(4000),
            },
        ),
        (
            b"HTTP/1.1 200 OK\r\nRetry-After: 5\r\nContent-Length: 100\r\n\r\n{\"code\":".to_vec(),
            DeliveryOutcome::Unknown {
                retry_after_ms: Some(5000),
            },
        ),
        (
            reply(200, "", &"x".repeat(MAX_RESPONSE_BYTES + 1)),
            DeliveryOutcome::Unknown {
                retry_after_ms: None,
            },
        ),
        (
            reply(307, "Location: https://elsewhere.example/push\r\n", ""),
            DeliveryOutcome::PermanentFailure,
        ),
    ];
    let mut server = server(cases.iter().map(|c| c.0.clone()).collect()).await;
    let output = output(&server, None);
    for (_, expected) in cases {
        assert_eq!(output.deliver(envelope()).await, expected);
        server.received.recv().await.unwrap();
    }
}
#[tokio::test]
async fn ac1_credentials_and_wrong_output_fail_before_send() {
    let server = server(vec![]).await;
    let output = output(&server, Some(SecurityError::Denied));
    let mut other = envelope();
    other.output_id = "other".into();
    assert_eq!(
        output.deliver(other).await,
        DeliveryOutcome::PermanentFailure
    );
    assert_eq!(output.credentials.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        output.deliver(envelope()).await,
        DeliveryOutcome::PermanentFailure
    );
}
#[test]
fn ac1_reject_url_credentials_key_paths_query_and_non_https() {
    let credentials = Arc::new(Credentials {
        calls: AtomicUsize::new(0),
        error: None,
    });
    for url in [
        "http://bark.example",
        "https://user:secret@bark.example",
        "https://bark.example/key",
        "https://bark.example/push?device_key=secret",
        "https://bark.example/#secret",
    ] {
        let error = BarkOutput::new(
            credentials.clone(),
            "out",
            "handle",
            url,
            Duration::from_secs(1),
        )
        .err()
        .unwrap();
        assert_eq!(error.to_string(), "invalid Bark output configuration");
    }
}

#[tokio::test]
async fn ac3_retry_after_survives_timeout_after_response_headers() {
    let mut server = server(vec![
        b"HTTP/1.1 200 OK\r\nRetry-After: 11\r\nContent-Length: 100\r\n\r\nstall-body".to_vec(),
    ])
    .await;
    let output = output(&server, None);
    assert_eq!(
        output.deliver(envelope()).await,
        DeliveryOutcome::Unknown {
            retry_after_ms: Some(11000)
        }
    );
    server.received.recv().await.unwrap();
}
#[tokio::test]
async fn ac1_private_destinations_rejected_before_loading_credentials() {
    let credentials = Arc::new(Credentials {
        calls: AtomicUsize::new(0),
        error: None,
    });
    for origin in [
        "https://127.0.0.1",
        "https://169.254.169.254",
        "https://[::1]",
        "https://10.0.0.1",
    ] {
        let output = BarkOutput::new(
            credentials.clone(),
            "output-33",
            "handle-33",
            origin,
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(
            output.deliver(envelope()).await,
            DeliveryOutcome::PermanentFailure
        );
    }
    assert_eq!(credentials.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn ac3_retry_after_delta_date_malformed_and_overflow() {
    let now = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    for (raw, expected) in [
        ("0", Some(0)),
        ("7", Some(7000)),
        ("-1", None),
        ("garbage", None),
        ("92233720368547758079999", Some(i64::MAX)),
    ] {
        assert_eq!(retry_after(raw, now), expected);
    }
    assert_eq!(
        retry_after(&httpdate::fmt_http_date(now + Duration::from_secs(25)), now),
        Some(25000)
    );
    assert_eq!(
        retry_after(&httpdate::fmt_http_date(now - Duration::from_secs(25)), now),
        Some(0)
    );
}
