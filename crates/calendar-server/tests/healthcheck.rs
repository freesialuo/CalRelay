//! CLI probes use only ephemeral loopback servers, never production services.
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Output},
    time::Duration,
};

fn probe(address: &str, mode: Option<&str>) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let deployment = dir.path().join("deployment.json");
    std::fs::write(&deployment, serde_json::json!({"bind":address}).to_string()).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_calendar-server"));
    command.env_clear().arg("healthcheck");
    if let Some(mode) = mode {
        command.arg(mode);
    }
    command
        .arg("--deployment-config")
        .arg(deployment)
        .output()
        .unwrap()
}

fn fixture(reply: &'static [u8], mode: Option<&str>, expected_path: &str) -> Output {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let path = expected_path.to_owned();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = vec![];
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if stream.read(&mut byte).unwrap() == 0 {
                return;
            }
            request.push(byte[0]);
        }
        assert!(String::from_utf8(request)
            .unwrap()
            .starts_with(&format!("GET {path} HTTP/1.1\r\n")));
        let _ = stream.write_all(reply);
    });
    let result = probe(&address.to_string(), mode);
    // Wake accept if the CLI rejected the request before connecting (also useful for Red).
    let _ = std::net::TcpStream::connect(address);
    server.join().unwrap();
    result
}

#[test]
fn live_probe_needs_no_database_configuration_and_ready_is_explicit() {
    for (mode, path) in [
        (None, "/health/live"),
        (Some("live"), "/health/live"),
        (Some("ready"), "/health/ready"),
    ] {
        let result = fixture(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n", mode, path);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[test]
fn failed_or_malformed_probes_fail_closed() {
    for reply in [
        &b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n"[..],
        &b"HTTP/1.1 302 Found\r\nLocation: http://example.invalid/\r\n\r\n"[..],
        &b"HTTP/1.1 2000 nope\r\n\r\n"[..],
        &b"HTTP/1.1 200 OK\r\n"[..],
    ] {
        let result = fixture(reply, None, "/health/live");
        assert!(!result.status.success());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("example.invalid"));
    }
    assert!(!probe("192.0.2.1:80", None).status.success());
    assert!(!probe("127.0.0.1:1", Some("unknown")).status.success());
}

#[test]
fn probe_has_a_total_timeout_when_local_peer_stalls() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let started = std::time::Instant::now();
    let dir = tempfile::tempdir().unwrap();
    let deployment = dir.path().join("deployment.json");
    std::fs::write(
        &deployment,
        serde_json::json!({"bind": listener.local_addr().unwrap()}).to_string(),
    )
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
        .env_clear()
        .args(["healthcheck", "--deployment-config"])
        .arg(deployment)
        .args(["--timeout-ms", "100"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("healthcheck request failed"));
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn healthcheck_reuses_the_deployment_contract_and_rejects_legacy_bind_without_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let deployment = dir.path().join("deployment.json");
    std::fs::write(&deployment, r#"{"bind":"127.0.0.1:8080"}"#).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
        .env_clear()
        .env("CALRELAY_BIND", "secret-invalid")
        .args(["healthcheck", "--deployment-config"])
        .arg(&deployment)
        .output()
        .unwrap();
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(error.contains("CALRELAY_BIND was removed"));
    assert!(!error.contains("secret-invalid"));
    std::fs::write(
        &deployment,
        r#"{"bind":"127.0.0.1:8080","unknown":"secret-invalid"}"#,
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
        .env_clear()
        .args(["healthcheck", "--deployment-config"])
        .arg(deployment)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-invalid"));
}

#[test]
fn admin_deployment_probe_does_not_load_secrets_or_require_database() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let deployment = dir.path().join("deployment.json");
    let missing_secret = dir.path().join("must-not-be-read");
    std::fs::write(
        &deployment,
        serde_json::json!({
            "bind":address,
            "admin":{"team":"synthetic-team","audience":"admin-aud",
            "origin":"https://admin.example.test","hostname":"admin.example.test",
            "action":"admin-login","site_key":"synthetic-site-key",
            "administrators":{"alice-sub":"alice@example.test"},"session_seconds":120,
            "turnstile_secret_file":missing_secret}
        })
        .to_string(),
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = [0; 1024];
        let read = stream.read(&mut request).unwrap();
        if read > 0 {
            assert!(request[..read].starts_with(b"GET /health/live HTTP/1.1"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        }
    });
    let result = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
        .env_clear()
        .args(["healthcheck", "--deployment-config"])
        .arg(deployment)
        .output()
        .unwrap();
    let _ = std::net::TcpStream::connect(address);
    server.join().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!missing_secret.exists());
}
