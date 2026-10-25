#![cfg(unix)]
mod common;
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
fn health_deployment(database: &std::path::Path, address: &str) -> std::path::PathBuf {
    let deployment = database.with_extension("health-deployment.json");
    std::fs::write(&deployment, serde_json::json!({"bind":address}).to_string()).unwrap();
    deployment
}
fn spawn(database: &std::path::Path, bind: &str) -> Process {
    let deployment = database.with_extension("deployment.json");
    std::fs::write(
        &deployment,
        common::deployment(database.parent().unwrap(), bind).to_string(),
    )
    .unwrap();
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
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let result = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
            .env_clear()
            .args(["healthcheck", "ready", "--deployment-config"])
            .arg(health_deployment(&database, address))
            .output()
            .unwrap();
        if result.status.success() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "real workers did not become ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
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
    for mode in ["live", "ready"] {
        let probe = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
            .env_clear()
            .args(["healthcheck", mode, "--deployment-config"])
            .arg(health_deployment(&database, address))
            .output()
            .unwrap();
        assert!(
            probe.status.success(),
            "{}",
            String::from_utf8_lossy(&probe.stderr)
        );
    }
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
            .arg("--deployment-config")
            .arg(health_deployment(&database, &address.to_string()))
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

fn wait_unhealthy(address: &str, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            child.try_wait().unwrap().is_none(),
            "health findings must not terminate the core"
        );
        if let Ok(mut stream) = TcpStream::connect(address) {
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            stream
                .write_all(
                    b"GET /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            let mut response = String::new();
            if stream.read_to_string(&mut response).is_ok() && response.starts_with("HTTP/1.1 503")
            {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "core did not expose failed liveness"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn terminate(child: &mut Child) {
    assert!(Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap()
        .success());
    assert!(wait_exit(child).success());
}

#[test]
fn failed_database_probe_exposes_health_without_terminating_or_repairing_policy() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("probe-failure.db");
    let mut process = spawn(&database, "127.0.0.1:0");
    let stderr = process.0.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let _ = tx.send(line.unwrap());
        }
    });
    let listening = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let address = listening.strip_prefix("CalRelay listening on ").unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db=calendar_storage::Storage::open(&database).await.unwrap();
        sqlx::query("CREATE TRIGGER fail_health_write BEFORE UPDATE ON recovery_control BEGIN SELECT RAISE(ABORT,'synthetic fault'); END").execute(db.pool()).await.unwrap(); db.close().await;
    });
    wait_unhealthy(address, &mut process.0);
    let result = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
        .env_clear()
        .args(["healthcheck", "live", "--deployment-config"])
        .arg(health_deployment(&database, address))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(
        process.0.try_wait().unwrap().is_none(),
        "CLI only reads health"
    );
    terminate(&mut process.0);
    reader.join().unwrap();
    assert!(!rx.try_iter().any(|s| s.contains("synthetic fault")));
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = calendar_storage::Storage::open(&database).await.unwrap();
        let policy: (i64, Option<i64>) =
            sqlx::query_as("SELECT paused,cutoff FROM recovery_control WHERE id=1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(policy, (0, None));
        let trigger: i64 =
            sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE name='fail_health_write'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(
            trigger, 1,
            "health must not repair or delete database objects"
        );
        db.close().await;
    });
}

#[test]
fn real_process_stall_is_exposed_until_explicit_sigterm_and_preserves_committed_data() {
    // The production binary has no synthetic-worker fault switches.
    if let Some(path) = std::env::var_os("CALRELAY_TEST_STALL_DATABASE") {
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            use calendar_server::supervision::{Progress, SupervisionConfig, WorkerKind};
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            let config = calendar_server::Config {
                bind: "127.0.0.1:0".parse().unwrap(),
                database: path.into(),
                max_request_bytes: 1024,
                max_response_bytes: 16384,
                request_timeout: Duration::from_secs(1),
            };
            let budgets = SupervisionConfig {
                startup_grace: Duration::from_secs(5),
                stall_timeout: Duration::from_millis(200),
                probe_interval: Duration::from_millis(10),
                probe_timeout: Duration::from_millis(30),
                shutdown_timeout: Duration::from_secs(1),
            };
            let server = calendar_server::Server::initialize_with_supervision(config, budgets)
                .await
                .unwrap();
            let services = server.services.clone();
            let worker_services = services.clone();
            let server = server
                .with_worker(WorkerKind::Planner, move |mut context| async move {
                    worker_services
                        .run_task(async |storage: &calendar_storage::Storage| {
                            sqlx::query("CREATE TABLE committed_witness(value INTEGER)")
                                .execute(storage.pool())
                                .await
                                .unwrap();
                            sqlx::query("INSERT INTO committed_witness VALUES(36)")
                                .execute(storage.pool())
                                .await
                                .unwrap();
                        })
                        .await
                        .unwrap();
                    context.progress(Progress::Completed);
                    context.cancelled().await;
                    Ok(())
                })
                .unwrap();
            let listener = server.bind().await.unwrap();
            eprintln!("CalRelay listening on {}", listener.local_addr().unwrap());
            server
                .serve(listener, async move {
                    terminate.recv().await;
                })
                .await
                .unwrap();
            assert!(services.storage.pool().is_closed());
        });
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("stall.db");
    let mut child = Process(
        Command::new(std::env::current_exe().unwrap())
            .env_clear()
            .env("CALRELAY_TEST_STALL_DATABASE", &database)
            .args([
                "--exact",
                "real_process_stall_is_exposed_until_explicit_sigterm_and_preserves_committed_data",
                "--nocapture",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stderr = child.0.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let _ = tx.send(line.unwrap());
        }
    });
    let listening = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let address = listening.strip_prefix("CalRelay listening on ").unwrap();
    wait_unhealthy(address, &mut child.0);
    terminate(&mut child.0);
    reader.join().unwrap();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = calendar_storage::Storage::open(database).await.unwrap();
        let value: i64 = sqlx::query_scalar("SELECT value FROM committed_witness")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(value, 36);
        let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(integrity, "ok");
        db.close().await;
    });
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

#[test]
fn standard_runtime_refuses_missing_security_before_database_or_bind() {
    let dir = tempfile::tempdir().unwrap();
    for field in ["admin", "credentials"] {
        let database = dir.path().join(field);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut document = common::deployment(dir.path(), &address.to_string());
        document.as_object_mut().unwrap().remove(field);
        let config = dir.path().join(format!("{field}.json"));
        std::fs::write(&config, document.to_string()).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
            .env_clear()
            .env("CALRELAY_DATABASE", &database)
            .arg("--deployment-config")
            .arg(config)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!database.exists());
        assert!(TcpStream::connect(address).is_err());
        assert_eq!(String::from_utf8(output.stderr).unwrap(),"invalid or missing server configuration: production runtime requires admin and credentials configuration\n");
    }
}

#[test]
fn standard_binary_plans_and_dispatches_real_configuration_without_private_network_access() {
    use calendar_application::{
        configuration::*, EventRepository, SourceRepository, SyncBatch, SyncMode, SyncRecord,
    };
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("workflow.db");
    let document = common::deployment(dir.path(), "127.0.0.1:0");
    let deployment = dir.path().join("workflow.json");
    std::fs::write(&deployment, document.to_string()).unwrap();
    let secret = dir.path().join("synthetic-bark-secret");
    std::fs::write(&secret, b"synthetic-device-key").unwrap();
    let provision = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
        .env_clear()
        .env("CALRELAY_DATABASE", &database)
        .args(["credential", "provision-bark", "--deployment-config"])
        .arg(&deployment)
        .args(["--credential-id", "bark-credential", "--secret-file"])
        .arg(&secret)
        .output()
        .unwrap();
    assert!(
        provision.status.success(),
        "{}",
        String::from_utf8_lossy(&provision.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&provision.stdout).unwrap();
    assert_eq!(receipt["credential_id"], "bark-credential");
    assert!(!String::from_utf8_lossy(&provision.stdout).contains("synthetic-device-key"));
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let start = chrono::DateTime::from_timestamp_millis(now + 10_000)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let db=calendar_storage::Storage::open(&database).await.unwrap();
        let mut source:SourceConfigurationV1=serde_json::from_str(include_str!("../../../examples/source-configuration-v1.json")).unwrap();
        source.mode=SourceMode::Pull;source.endpoint=Some("https://127.0.0.1:9/blocked.ics".into());
        source.refresh_interval_ms=Some(60_000);source.freshness_sla_ms=Some(60_000);
        db.put_source_configuration("source",ConfigurationCondition::Absent,source.clone(),"test",now).await.unwrap();
        source.enabled=true;db.put_source_configuration("source",ConfigurationCondition::Revision(1),source,"test",now).await.unwrap();
        let mut output:BarkOutputConfigurationV1=serde_json::from_str(include_str!("../../../examples/bark-output-configuration-v1.json")).unwrap();
        output.origin="https://127.0.0.1:9".into();
        db.put_bark_output_configuration("bark-a",ConfigurationCondition::Absent,output.clone(),"test",now).await.unwrap();
        output.enabled=true;db.put_bark_output_configuration("bark-a",ConfigurationCondition::Revision(1),output,"test",now).await.unwrap();
        let mut rule:RuleConfigurationV1=serde_json::from_str(include_str!("../../../examples/rule-configuration-v1.json")).unwrap();rule.policy.lead_ms=0;
        db.put_rule_configuration("rule",ConfigurationCondition::Absent,rule.clone(),"test",now).await.unwrap();
        rule.enabled=true;db.put_rule_configuration("rule",ConfigurationCondition::Revision(1),rule,"test",now).await.unwrap();
        let event=serde_json::from_value(serde_json::json!({"schema_version":"1.0","external_uid":"future-event","title":"Synthetic process workflow","status":"confirmed","start":{"kind":"utc","value":start}})).unwrap();
        db.commit_sync(SyncBatch { source_id:"source".into(),base_revision:db.source_state("source").await.unwrap().revision,
            mode:SyncMode::Delta,records:vec![SyncRecord::Upsert { calendar_id:"calendar-a".into(),event:Box::new(event) }],proposed_checkpoint:None,verified_at:now }).await.unwrap();
        db.close().await;
    });
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_calendar-server"))
            .env_clear()
            .env("CALRELAY_DATABASE", &database)
            .arg("--deployment-config")
            .arg(&deployment)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stderr = process.0.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let _ = tx.send(line.unwrap());
        }
    });
    let line = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let address = line.strip_prefix("CalRelay listening on ").unwrap();
    let observed = runtime
        .block_on(calendar_storage::Storage::open(&database))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let state: Option<String> = runtime
            .block_on(
                sqlx::query_scalar("SELECT state FROM delivery_jobs")
                    .fetch_optional(observed.pool()),
            )
            .unwrap();
        if state.as_deref() == Some("failed_permanent") {
            break;
        }
        assert!(
            process.0.try_wait().unwrap().is_none(),
            "external degradation must not terminate the binary"
        );
        assert!(
            Instant::now() < deadline,
            "configured planner/delivery did not run; state={state:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    runtime.block_on(async {
        let audit: (i64, i64, i64) = sqlx::query_as(
            "SELECT COUNT(*),COUNT(sent_at),COUNT(finished_at) FROM delivery_attempts",
        )
        .fetch_one(observed.pool())
        .await
        .unwrap();
        assert_eq!(audit, (1, 1, 1));
        let watermarks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM planning_watermarks")
            .fetch_one(observed.pool())
            .await
            .unwrap();
        assert_eq!(watermarks, 1);
        let verified: Option<i64> =
            sqlx::query_scalar("SELECT last_verified_at FROM sources WHERE id='source'")
                .fetch_one(observed.pool())
                .await
                .unwrap();
        assert_eq!(
            verified,
            Some(now),
            "blocked refresh keeps the previous snapshot"
        );
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        for path in ["/health/live", "/health/ready"] {
            assert_eq!(
                client
                    .get(format!("http://{address}{path}"))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                200
            );
        }
        assert_eq!(
            client
                .get(format!("http://{address}/api/admin/v1/sources"))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        #[cfg(feature = "production-web")]
        {
            let page = client
                .get(format!("http://{address}/admin/"))
                .send()
                .await
                .unwrap();
            assert_eq!(page.status(), 200);
            assert!(page.text().await.unwrap().contains("/admin/assets/"));
        }
        observed.close().await;
    });
    terminate(&mut process.0);
    reader.join().unwrap();
    assert!(!rx.try_iter().any(|s| s.contains("synthetic-device-key")));
    runtime.block_on(async {
        let db = calendar_storage::Storage::open(&database).await.unwrap();
        let accepted: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM logical_deliveries WHERE accepted_at IS NOT NULL",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(accepted, 0);
        let state: String = sqlx::query_scalar("SELECT state FROM delivery_jobs")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(state, "failed_permanent");
        db.close().await;
    });
}
