//! CLI probes use only ephemeral loopback servers, never production services.
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Output},
    time::Duration,
};

fn probe(address: &str, mode: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_calendar-server"));
    command
        .env_clear()
        .env("CALRELAY_BIND", address)
        .arg("healthcheck");
    if let Some(mode) = mode {
        command.arg(mode);
    }
    command.output().unwrap()
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
    let result = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
        .env_clear()
        .env("CALRELAY_BIND", listener.local_addr().unwrap().to_string())
        .env("CALRELAY_HEALTHCHECK_TIMEOUT_MS", "100")
        .arg("healthcheck")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(started.elapsed() < Duration::from_secs(3));
}
