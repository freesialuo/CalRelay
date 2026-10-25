#![cfg(unix)]
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn spawn(database: &std::path::Path, bind: &str) -> Process {
    let deployment = database.with_extension("deployment.json");
    std::fs::write(&deployment, serde_json::json!({"bind":bind}).to_string()).unwrap();
    Process(
        Command::new(env!("CARGO_BIN_EXE_calendar-server"))
            .env_clear()
            .arg("--deployment-config")
            .arg(deployment)
            .env("CALRELAY_DATABASE", database)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    )
}
fn wait_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "server failed to exit within 5 seconds"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn sigterm_stops_listener_and_database_reopens_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("process.db");
    let mut process = spawn(&database, "127.0.0.1:0");
    let stderr = process.0.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let _ = tx.send(line.unwrap());
        }
    });
    let line = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let address = line.strip_prefix("CalRelay listening on ").unwrap();
    let mut connection = TcpStream::connect(address).unwrap();
    connection
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    connection
        .write_all(b"GET /health/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut response = String::new();
    connection.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(Command::new("kill")
        .args(["-TERM", &process.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    assert!(wait_exit(&mut process.0).success());
    reader.join().unwrap();
    assert!(TcpStream::connect(address).is_err());
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let storage = calendar_storage::Storage::open(&database).await.unwrap();
        let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(storage.pool())
            .await
            .unwrap();
        assert_eq!(integrity, "ok");
        storage.close().await;
    });
}
#[test]
fn bad_config_and_migration_failure_do_not_listen() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("bad.db");
    // Reserve and release an address so each failed startup has a known target to probe.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut missing = Process(
        Command::new(env!("CARGO_BIN_EXE_calendar-server"))
            .env_clear()
            .env("CALRELAY_BIND", address.to_string())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    assert!(!wait_exit(&mut missing.0).success());
    assert!(TcpStream::connect(address).is_err());
    std::fs::write(&database, b"this is not a database").unwrap();
    let mut corrupt = spawn(&database, &address.to_string());
    assert!(!wait_exit(&mut corrupt.0).success());
    let mut diagnostic = String::new();
    corrupt
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut diagnostic)
        .unwrap();
    assert_eq!(diagnostic, "database initialization failed\n");
    assert!(TcpStream::connect(address).is_err());
    assert_eq!(std::fs::read(&database).unwrap(), b"this is not a database");
}

#[test]
fn legacy_bind_is_rejected_with_upgrade_diagnostic_before_creating_database() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("must-not-exist.db");
    let result = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
        .env_clear()
        .env("CALRELAY_BIND", "secret-invalid")
        .env("CALRELAY_DATABASE", &database)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let diagnostic = String::from_utf8(result.stderr).unwrap();
    assert!(diagnostic.contains("--deployment-config"), "{diagnostic}");
    assert!(!diagnostic.contains("secret-invalid"));
    assert!(!database.exists());
}
