#![cfg(all(unix, feature = "test-harness"))]
use calendar_storage::Storage;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
const NOW: i64 = 1_793_638_800_000;
const ORIGIN: &str = "https://localhost:4443";
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Fixture {
    root: tempfile::TempDir,
    db: PathBuf,
    manifest: PathBuf,
    clock: PathBuf,
    deployment: PathBuf,
}
impl Fixture {
    fn new(targets: Value) -> Self {
        let root = tempfile::Builder::new()
            .prefix("calrelay-system-")
            .tempdir()
            .unwrap();
        let key = root.path().join("key");
        std::fs::write(&key, [81u8; 32]).unwrap();
        let deployment = root.path().join("deployment.json");
        std::fs::write(&deployment,json!({"bind":"127.0.0.1:0","credentials":{"current_key_version":1,"key_files":{"1":key}}}).to_string()).unwrap();
        let clock = root.path().join("clock.json");
        std::fs::write(
            &clock,
            json!({"utc_now_ms":NOW,"monotonic_offset_ms":0}).to_string(),
        )
        .unwrap();
        let manifest = root.path().join("system.json");
        std::fs::write(&manifest,json!({"version":1,"root":root.path(),"deployment_config":deployment,"clock_file":clock,"targets":targets}).to_string()).unwrap();
        let db = root.path().join("db");
        Self {
            root,
            db,
            manifest,
            clock,
            deployment,
        }
    }
    fn mode(&self, mode: &str) {
        let mut value: Value =
            serde_json::from_slice(&std::fs::read(&self.manifest).unwrap()).unwrap();
        value["mode"] = json!(mode);
        std::fs::write(&self.manifest, value.to_string()).unwrap();
    }
    fn advance(&self, utc: i64, offset: u64) {
        let tmp = self.root.path().join("clock.new");
        std::fs::write(
            &tmp,
            json!({"utc_now_ms":utc,"monotonic_offset_ms":offset}).to_string(),
        )
        .unwrap();
        std::fs::rename(tmp, &self.clock).unwrap();
    }
    fn command(&self, db: &Path) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_calendar-admin-harness"));
        c.env_clear().args([
            "--database",
            db.to_str().unwrap(),
            "--browser-origin",
            ORIGIN,
            "--system-runtime",
            self.manifest.to_str().unwrap(),
        ]);
        c
    }
    fn start(&self, db: &Path) -> (Process, Value) {
        let mut p = Process(
            self.command(db)
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let out = p.0.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(out).read_line(&mut line);
            let _ = tx.send(line);
        });
        let line = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let meta: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(meta["synthetic_only"], true);
        assert_eq!(meta["system_runtime"], true);
        (p, meta)
    }
    fn provision(&self) {
        let file = self.root.path().join("bark-secret");
        std::fs::write(&file, b"synthetic_harness_bark").unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
            .env_clear()
            .env("CALRELAY_DATABASE", &self.db)
            .args([
                "credential",
                "provision-bark",
                "--deployment-config",
                self.deployment.to_str().unwrap(),
                "--credential-id",
                "bark-credential",
                "--secret-file",
                file.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!String::from_utf8_lossy(&out.stdout).contains("synthetic_harness_bark"));
    }
    fn offline(&self, mode: &str, source: &Path, destination: &Path, cutoff: Option<i64>) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_calendar-admin-harness"));
        c.env_clear().args([
            mode,
            "--test-root",
            self.root.path().to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--destination",
            destination.to_str().unwrap(),
        ]);
        if let Some(cutoff) = cutoff {
            c.args(["--cutoff-ms", &cutoff.to_string()]);
        }
        let out = c.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
fn stop(p: &mut Process) {
    assert!(Command::new("kill")
        .args(["-TERM", &p.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = p.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}
struct Session {
    client: reqwest::Client,
    base: String,
    jwt: String,
    cookie: String,
    csrf: String,
    nonce: String,
}
impl Session {
    async fn login(meta: &Value) -> Self {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let base = meta["upstream"].as_str().unwrap().to_owned();
        let jwt = meta["access_jwt"].as_str().unwrap().to_owned();
        assert_eq!(
            client
                .get(format!("{base}/api/admin/v1/sources"))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        let r = client
            .post(format!("{base}/api/admin/v1/session"))
            .header("cf-access-jwt-assertion", &jwt)
            .header("origin", ORIGIN)
            .json(&json!({"turnstile_token":"synthetic-challenge:system-first"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let cookie = r.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let body: Value = r.json().await.unwrap();
        Self {
            client,
            base,
            jwt,
            cookie,
            csrf: body["csrf_token"].as_str().unwrap().into(),
            nonce: body["write_nonce"].as_str().unwrap().into(),
        }
    }
    async fn write(&mut self, path: &str, body: &Value, create: bool) {
        let r = self
            .client
            .put(format!("{}{path}", self.base))
            .header("cf-access-jwt-assertion", &self.jwt)
            .header("cookie", &self.cookie)
            .header("origin", ORIGIN)
            .header("x-csrf-token", &self.csrf)
            .header("x-admin-write-nonce", &self.nonce)
            .header(
                if create { "if-none-match" } else { "if-match" },
                if create { "*" } else { "\"1\"" },
            )
            .json(body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            r.status().as_u16(),
            if create { 201 } else { 200 },
            "{path}"
        );
        self.nonce = r.headers()["x-admin-write-nonce"].to_str().unwrap().into();
    }
    async fn configure(&mut self) {
        for id in ["a", "b"] {
            let mut source: Value = serde_json::from_str(include_str!(
                "../../../examples/source-configuration-v1.json"
            ))
            .unwrap();
            source["mode"] = json!("pull");
            source["endpoint"] = json!(format!("https://ics-{id}.system.invalid/calendar.ics"));
            source["refresh_interval_ms"] = json!(60000);
            source["freshness_sla_ms"] = json!(300000);
            source["capabilities"]["conditional_read"] = json!(true);
            source["calendars"][0]["id"] = json!(format!("calendar-{id}"));
            self.write(&format!("/api/admin/v1/sources/{id}"), &source, true)
                .await;
            source["enabled"] = json!(true);
            self.write(&format!("/api/admin/v1/sources/{id}"), &source, false)
                .await;
        }
        let mut output: Value = serde_json::from_str(include_str!(
            "../../../examples/bark-output-configuration-v1.json"
        ))
        .unwrap();
        output["origin"] = json!("https://bark.system.invalid/push");
        self.write("/api/admin/v1/outputs/bark-a", &output, true)
            .await;
        output["enabled"] = json!(true);
        self.write("/api/admin/v1/outputs/bark-a", &output, false)
            .await;
        let mut rule: Value =
            serde_json::from_str(include_str!("../../../examples/rule-configuration-v1.json"))
                .unwrap();
        rule["policy"]["lead_ms"] = json!(0);
        rule["policy"]["calendar_ids"] = json!(["calendar-a", "calendar-b"]);
        self.write("/api/admin/v1/rules/rule-a", &rule, true).await;
        rule["enabled"] = json!(true);
        self.write("/api/admin/v1/rules/rule-a", &rule, false).await;
    }
}
async fn count(db: &Storage, sql: &'static str, expected: i64) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let n: i64 = sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap();
            if n == expected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn controlled_clock_guarded_writes_reopen_and_restore_use_the_same_real_runtime() {
    use axum::{
        extract::{Path as RoutePath, State},
        http::{HeaderMap, StatusCode},
        routing::{get, post},
        Router,
    };
    #[derive(Default)]
    struct Calls {
        fetch: AtomicUsize,
        conditional: AtomicUsize,
        push: AtomicUsize,
    }
    let calls = Arc::new(Calls::default());
    let router=Router::new().route("/ics/{id}",get(|State(c):State<Arc<Calls>>,RoutePath(id):RoutePath<String>,headers:HeaderMap|async move {
        c.fetch.fetch_add(1,Ordering::SeqCst);let tag=format!("\"system-{id}-r1\"");if headers.get("if-none-match").is_some_and(|v|v==tag.as_str()){c.conditional.fetch_add(1,Ordering::SeqCst);return (StatusCode::NOT_MODIFIED,[("etag",tag)],String::new());}
        let start=chrono::DateTime::from_timestamp_millis(NOW+if id=="a"{120000}else{180000}).unwrap().format("%Y%m%dT%H%M%SZ").to_string();
        (StatusCode::OK,[("etag",tag)],format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//System//EN\r\nBEGIN:VEVENT\r\nUID:same-system-uid\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:{start}\r\nSUMMARY:Source {id}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"))
    })).route("/bark",post(|State(c):State<Arc<Calls>>,axum::Json(body):axum::Json<Value>|async move {assert_eq!(body["device_key"],"synthetic_harness_bark");assert!(body["body"].is_string());c.push.fetch_add(1,Ordering::SeqCst);axum::Json(json!({"code":200}))})).with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (http_stop, stopped) = tokio::sync::oneshot::channel();
    let http = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let f = Fixture::new(
        json!({"a":format!("{base}/ics/a"),"b":format!("{base}/ics/b"),"bark":format!("{base}/bark")}),
    );
    let (mut process, meta) = f.start(&f.db);
    f.provision();
    let mut session = Session::login(&meta).await;
    session.configure().await;
    let db = Storage::open(&f.db).await.unwrap();
    count(&db, "SELECT COUNT(*) FROM delivery_jobs", 2).await;
    assert_eq!(calls.push.load(Ordering::SeqCst), 0);
    let namespace:(i64,i64)=sqlx::query_as("SELECT COUNT(*),COUNT(DISTINCT source_id) FROM events WHERE external_uid='same-system-uid'").fetch_one(db.pool()).await.unwrap();
    assert_eq!(namespace, (2, 2));
    f.advance(NOW + 240000, 240000);
    count(
        &db,
        "SELECT COUNT(*) FROM logical_deliveries WHERE accepted_at IS NOT NULL",
        2,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while calls.conditional.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(calls.push.load(Ordering::SeqCst), 2);
    stop(&mut process);
    db.close().await;
    // Both legacy/default and explicit fresh preserve refusal of existing databases.
    let rejected = Command::new(env!("CARGO_BIN_EXE_calendar-admin-harness"))
        .env_clear()
        .args([
            "--database",
            f.db.to_str().unwrap(),
            "--browser-origin",
            ORIGIN,
        ])
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert!(!f.command(&f.db).output().unwrap().status.success());
    f.mode("reopen");
    let (mut process, meta) = f.start(&f.db);
    let client = reqwest::Client::new();
    let base = meta["upstream"].as_str().unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        client
            .get(format!("{base}/health/ready"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(calls.push.load(Ordering::SeqCst), 2);
    stop(&mut process);
    f.mode("restored");
    assert!(
        !f.command(&f.db).output().unwrap().status.success(),
        "restored must reject an unpaused original"
    );
    let snapshot = f.root.path().join("snapshot");
    f.offline("backup", &f.db, &snapshot, None);
    let restore = f.root.path().join("restored");
    f.offline("restore", &snapshot, &restore, Some(NOW + 240000));
    let restored = restore.join("database.sqlite3");
    let requests = (
        calls.fetch.load(Ordering::SeqCst),
        calls.push.load(Ordering::SeqCst),
    );
    let before = Storage::open(&f.db).await.unwrap();
    let windows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT source_id,planned_until FROM planning_watermarks ORDER BY source_id",
    )
    .fetch_all(before.pool())
    .await
    .unwrap();
    before.close().await;
    let (mut process, meta) = f.start(&restored);
    f.advance(NOW + 600000, 600000);
    let base = meta["upstream"].as_str().unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(
        client
            .get(format!("{base}/health/live"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{base}/health/ready"))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    assert_eq!(
        (
            calls.fetch.load(Ordering::SeqCst),
            calls.push.load(Ordering::SeqCst)
        ),
        requests
    );
    let db = Storage::open(&restored).await.unwrap();
    let control: (bool, Option<i64>) = sqlx::query_as("SELECT paused,cutoff FROM recovery_control")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(control, (true, Some(NOW + 240000)));
    let after: Vec<(String, i64)> = sqlx::query_as(
        "SELECT source_id,planned_until FROM planning_watermarks ORDER BY source_id",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    assert_eq!(
        after, windows,
        "paused planner must not extend watermarks after clock advance"
    );
    count(
        &db,
        "SELECT COUNT(*) FROM logical_deliveries WHERE accepted_at IS NOT NULL",
        2,
    )
    .await;
    stop(&mut process);
    db.close().await;
    let _ = http_stop.send(());
    http.await.unwrap();
}

#[test]
fn system_mode_rejects_uncontrolled_roots_targets_clock_and_symlinks_before_database() {
    use calendar_server::test_runtime::SystemHarnessConfig;
    let f = Fixture::new(
        json!({"a":"http://127.0.0.1:1/a","b":"http://127.0.0.1:1/b","bark":"http://127.0.0.1:1/bark"}),
    );
    let original: Value = serde_json::from_slice(&std::fs::read(&f.manifest).unwrap()).unwrap();
    for (field, value) in [
        ("version", json!(2)),
        ("mode", json!("auto")),
        (
            "targets",
            json!({"a":"http://example.com/a","b":"http://127.0.0.1:1/b","bark":"http://127.0.0.1:1/bark"}),
        ),
        ("clock_file", json!(f.root.path().join("../outside"))),
    ] {
        let mut bad = original.clone();
        bad[field] = value;
        std::fs::write(&f.manifest, bad.to_string()).unwrap();
        assert!(SystemHarnessConfig::load(&f.manifest, &f.db).is_err());
        assert!(!f.db.exists());
    }
    std::fs::write(&f.manifest, original.to_string()).unwrap();
    let target = f.root.path().join("linked-clock");
    std::os::unix::fs::symlink(&f.clock, &target).unwrap();
    let mut bad = original.clone();
    bad["clock_file"] = json!(target);
    std::fs::write(&f.manifest, bad.to_string()).unwrap();
    assert!(SystemHarnessConfig::load(&f.manifest, &f.db).is_err());
    assert!(!f.db.exists());
}
