//! Synthetic HTTP transport is confined to tests; production HttpFetcher keeps TLS/SSRF rules.
use calendar_adapters::IcsLimits;
use calendar_application::{configuration::*, *};
use calendar_server::{source_worker::*, Config, Server, Services};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Semaphore,
};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Time {
    utc: AtomicI64,
    mono: AtomicU64,
}
impl Time {
    fn new() -> Self {
        Self {
            utc: AtomicI64::new(1000),
            mono: AtomicU64::new(0),
        }
    }
    fn set(&self, utc: i64, mono: u64) {
        self.utc.store(utc, Ordering::SeqCst);
        self.mono.store(mono, Ordering::SeqCst);
    }
}
impl Clock for Time {
    fn utc_now_ms(&self) -> i64 {
        self.utc.load(Ordering::SeqCst)
    }
    fn monotonic_now_ms(&self) -> u64 {
        self.mono.load(Ordering::SeqCst)
    }
}
fn limits(attempts: usize) -> SourceScanLimits {
    SourceScanLimits::new(
        1,
        attempts,
        1024,
        Duration::from_secs(120),
        Duration::from_secs(60),
        Duration::from_secs(60),
        Duration::from_secs(240),
    )
    .unwrap()
}
async fn service(path: &std::path::Path) -> Result<Arc<Services>> {
    Ok(Server::initialize(Config {
        bind: "127.0.0.1:0".parse()?,
        database: path.into(),
        max_request_bytes: 1024,
        max_response_bytes: 16384,
        request_timeout: Duration::from_secs(1),
    })
    .await?
    .services)
}
async fn source(
    services: &Services,
    id: &str,
    mode: SourceMode,
    enabled: bool,
) -> Result<SourceConfigurationRecord> {
    let mut config: SourceConfigurationV1 = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))?;
    config.name = id.into();
    config.mode = mode.clone();
    config.calendars[0].id = format!("c_{id}");
    if mode == SourceMode::Pull {
        config.endpoint = Some(format!("https://source.example/{id}"));
        config.refresh_interval_ms = Some(60000);
        config.freshness_sla_ms = Some(60000);
        config.capabilities.conditional_read = true;
    }
    if mode == SourceMode::PushOnly {
        config.adapter = AdapterKind::StandardCalendar;
    }
    let record = services
        .storage
        .put_source_configuration(id, ConfigurationCondition::Absent, config, "test", 1000)
        .await?;
    if !enabled {
        return Ok(record);
    }
    let mut config = record.configuration;
    config.enabled = true;
    Ok(services
        .storage
        .put_source_configuration(
            id,
            ConfigurationCondition::Revision(record.config_revision),
            config,
            "test",
            1000,
        )
        .await?)
}
const ICS:&str="BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:one\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20261102T170000Z\r\nSUMMARY:Original\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
struct Gate {
    entered: Semaphore,
    release: Semaphore,
}
#[derive(Default)]
struct MockState {
    statuses: Mutex<BTreeMap<String, u16>>,
    retry_after_s: Mutex<BTreeMap<String, u64>>,
    requests: Mutex<Vec<(String, bool)>>,
    gate: Mutex<Option<Arc<Gate>>>,
}
struct Http {
    address: std::net::SocketAddr,
    state: Arc<MockState>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Http {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task.abort();
    }
}
impl Http {
    async fn new() -> Result<Self> {
        let state = Arc::new(MockState::default());
        let app_state = state.clone();
        let app = axum::Router::new().route(
            "/{id}",
            axum::routing::get(
                move |axum::extract::Path(id): axum::extract::Path<String>,
                      headers: axum::http::HeaderMap| {
                    let state = app_state.clone();
                    async move {
                        let conditional = headers.contains_key("if-none-match");
                        state
                            .requests
                            .lock()
                            .unwrap()
                            .push((id.clone(), conditional));
                        let gate = state.gate.lock().unwrap().clone();
                        if let Some(gate) = gate {
                            gate.entered.add_permits(1);
                            gate.release.acquire().await.unwrap().forget();
                        }
                        let status = *state.statuses.lock().unwrap().get(&id).unwrap_or(&200);
                        let status = if status == 200 && conditional {
                            304
                        } else {
                            status
                        };
                        let retry = state.retry_after_s.lock().unwrap().get(&id).copied();
                        (
                            axum::http::StatusCode::from_u16(status).unwrap(),
                            [("etag", "\"same\"")],
                            retry.map(|seconds| [("retry-after", seconds.to_string())]),
                            if status == 200 { ICS } else { "" },
                        )
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
        });
        Ok(Self {
            address,
            state,
            stop: Some(stop),
            task,
        })
    }
    fn runner(&self) -> IcsSourceRunner<TestFetcher> {
        IcsSourceRunner {
            fetcher: Arc::new(TestFetcher(self.address)),
            ics_limits: IcsLimits::default(),
            sync_limits: SyncLimits {
                timeout: Duration::from_secs(2),
                ..Default::default()
            },
        }
    }
    fn requests(&self) -> Vec<(String, bool)> {
        self.state.requests.lock().unwrap().clone()
    }
}
struct TestFetcher(std::net::SocketAddr);
struct Unexpected304;
impl RemoteFetcher for Unexpected304 {
    async fn fetch(
        &self,
        _: RemoteFetchRequest,
    ) -> std::result::Result<RemoteFetchOutcome, RemoteFetchError> {
        Ok(RemoteFetchOutcome::NotModified(RemoteValidators::default()))
    }
}
impl RemoteFetcher for TestFetcher {
    async fn fetch(
        &self,
        request: RemoteFetchRequest,
    ) -> std::result::Result<RemoteFetchOutcome, RemoteFetchError> {
        let mut socket = tokio::net::TcpStream::connect(self.0)
            .await
            .map_err(|_| RemoteFetchError::Transport)?;
        let path = request.url.strip_prefix("https://source.example").unwrap();
        let conditional = request
            .validators
            .etag
            .as_ref()
            .map(|tag| format!("If-None-Match: {tag}\r\n"))
            .unwrap_or_default();
        socket.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{conditional}\r\n").as_bytes()).await.map_err(|_|RemoteFetchError::Transport)?;
        let mut bytes = Vec::new();
        socket
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| RemoteFetchError::Transport)?;
        let text = String::from_utf8(bytes).unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap();
        let status: u16 = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        let validators = RemoteValidators {
            etag: Some("\"same\"".into()),
            last_modified: None,
        };
        match status {
            200 => Ok(RemoteFetchOutcome::Modified(RemoteDocument {
                credential_revision: None,
                final_url: request.url,
                body: body.as_bytes().to_vec(),
                validators,
            })),
            304 if request.has_committed_snapshot => {
                Ok(RemoteFetchOutcome::NotModified(validators))
            }
            304 => Err(RemoteFetchError::UnexpectedNotModified),
            _ => {
                let retry_after_ms = head.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    let seconds: i64 = name
                        .eq_ignore_ascii_case("retry-after")
                        .then(|| value.trim().parse().ok())??;
                    seconds.checked_mul(1000)
                });
                Err(if status == 429 {
                    RemoteFetchError::RateLimited { retry_after_ms }
                } else {
                    RemoteFetchError::Server {
                        status,
                        retry_after_ms,
                    }
                })
            }
        }
    }
}

#[tokio::test]
async fn capability_period_304_and_monotonic_deadlines_use_real_http_sqlite() -> Result {
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    let http = Http::new().await?;
    source(&services, "a", SourceMode::Pull, true).await?;
    source(&services, "disabled", SourceMode::Pull, false).await?;
    source(&services, "static", SourceMode::ManualStatic, true).await?;
    source(&services, "push", SourceMode::PushOnly, true).await?;
    let clock = Time::new();
    let mut scanner = SourceScanner::new(limits(8));
    let runner = http.runner();
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |_| {})
            .await?
            .attempted,
        1
    );
    let before = services.storage.source_state("a").await?;
    clock.set(1_000_000_000, 59999);
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |_| {})
            .await?
            .attempted,
        0,
        "UTC forward jump must not trigger early polling"
    );
    clock.set(-10_000, 60000);
    let mut progress = vec![];
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |p| progress.push(p))
            .await?
            .attempted,
        1
    );
    assert!(progress.iter().any(|p| matches!(
        p,
        SourceProgress::Synchronized {
            not_modified: true,
            ..
        }
    )));
    assert_eq!(http.requests(), [("a".into(), false), ("a".into(), true)]);
    assert_eq!(
        services.storage.source_state("a").await?.revision,
        before.revision
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events")
        .fetch_one(services.storage.pool())
        .await?;
    assert_eq!(count, 1);
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn failure_preserves_snapshot_and_backoff_does_not_block_another_source() -> Result {
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    let http = Http::new().await?;
    source(&services, "a", SourceMode::Pull, true).await?;
    source(&services, "b", SourceMode::Pull, true).await?;
    let clock = Time::new();
    let runner = http.runner();
    let mut scanner = SourceScanner::new(limits(8));
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    let before = services.storage.source_state("a").await?;
    http.state.statuses.lock().unwrap().insert("a".into(), 503);
    clock.set(61000, 60000);
    let mut progress = vec![];
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |p| progress.push(p))
            .await?
            .attempted,
        2
    );
    assert_eq!(services.storage.source_state("a").await?, before);
    assert!(progress
        .iter()
        .any(|p| matches!(p,SourceProgress::ExternalFailure{source_id,..} if source_id=="a")));
    assert!(progress
        .iter()
        .any(|p| matches!(p,SourceProgress::Synchronized{source_id,external_degraded:true,..} if source_id=="b")));
    clock.set(120999, 119999);
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |_| {})
            .await?
            .attempted,
        0
    );
    clock.set(121000, 120000);
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |_| {})
            .await?
            .attempted,
        2
    );
    // Second failure doubles only A's retry delay; B continues its configured period.
    clock.set(181000, 180000);
    let mut synchronized = vec![];
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |p| synchronized.push(p))
            .await?
            .attempted,
        1
    );
    assert!(synchronized
        .iter()
        .any(|p| matches!(p,SourceProgress::Synchronized{source_id,..} if source_id=="b")));
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn upstream_retry_after_outlasts_the_local_backoff_cap_without_blocking_another_source(
) -> Result {
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    let http = Http::new().await?;
    source(&services, "a", SourceMode::Pull, true).await?;
    source(&services, "b", SourceMode::Pull, true).await?;
    let clock = Time::new();
    let runner = http.runner();
    let mut scanner = SourceScanner::new(limits(8));
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    let count = |id: &str| http.requests().iter().filter(|(r, _)| r == id).count();
    // (status, Retry-After seconds, expected wait): an hour beats the 240 s local cap for 429 and
    // 503, a one-second hint never undercuts the 60 s local backoff, and 30 days is bounded to 7.
    let mut mono = 0;
    for (status, seconds, wait) in [
        (429, 3600, 3_600_000),
        (503, 3600, 3_600_000),
        (429, 1, 60_000),
        (503, 2_592_000, 604_800_000),
    ] {
        mono += 60_000;
        let before = services.storage.source_state("a").await?;
        let set = |status| {
            http.state
                .statuses
                .lock()
                .unwrap()
                .insert("a".into(), status)
        };
        set(status);
        http.state
            .retry_after_s
            .lock()
            .unwrap()
            .insert("a".into(), seconds);
        clock.set(1000 + mono as i64, mono);
        let mut progress = vec![];
        scanner
            .round(&services, &runner, &clock, |p| progress.push(p))
            .await?;
        let expected = SourceRunError::Sync(IngestionError::RetryAfter {
            retry_after_ms: seconds * 1000,
        });
        assert!(progress.iter().any(|p| matches!(
            p,
            SourceProgress::ExternalFailure { source_id, error }
                if source_id == "a" && *error == expected
        )));
        assert_eq!(services.storage.source_state("a").await?, before);
        let (a, b) = (count("a"), count("b"));
        mono += wait;
        clock.set(1000 + mono as i64 - 1, mono - 1);
        scanner.round(&services, &runner, &clock, |_| {}).await?;
        assert_eq!(count("a"), a, "{status} must wait {wait} ms");
        if wait > 60_000 {
            assert_eq!(count("b"), b + 1, "another source keeps its period");
        }
        set(200);
        clock.set(1000 + mono as i64, mono);
        scanner.round(&services, &runner, &clock, |_| {}).await?;
        assert_eq!(count("a"), a + 1, "{status} may retry at the boundary");
    }
    services.drain().await;
    Ok(())
}
#[tokio::test]
async fn manual_refresh_skips_the_period_but_not_retry_after_and_only_for_pull_sources() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    let services = service(&path).await?;
    let http = Http::new().await?;
    source(&services, "a", SourceMode::Pull, true).await?;
    source(&services, "b", SourceMode::Pull, true).await?;
    source(&services, "disabled", SourceMode::Pull, false).await?;
    source(&services, "push", SourceMode::PushOnly, true).await?;
    source(&services, "static", SourceMode::ManualStatic, true).await?;
    let db = &services.storage;
    for id in ["disabled", "push", "static"] {
        assert!(matches!(
            db.request_source_refresh(id, 1).await,
            Err(ConfigurationError::Conflict)
        ));
    }
    assert!(matches!(
        db.request_source_refresh("missing", 1).await,
        Err(ConfigurationError::NotFound)
    ));
    let clock = Time::new();
    let runner = http.runner();
    // One-second scan wait and local floor, 240 s backoff cap, one-minute configured period.
    let second = Duration::from_secs(1);
    let limits = SourceScanLimits::new(
        1,
        8,
        1024,
        Duration::from_secs(120),
        second,
        second,
        Duration::from_secs(240),
    )
    .unwrap();
    let mut scanner = SourceScanner::new(limits);
    let count = |id: &str| http.requests().iter().filter(|(r, _)| r == id).count();
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    assert_eq!((count("a"), count("b")), (1, 1));

    // Repeats merge; the local floor after the last sync holds the request, then it is served
    // long before the period while the other source is left alone.
    db.request_source_refresh("a", 10).await?;
    db.request_source_refresh("a", 11).await?;
    let pending = db.source_refresh_request("a").await?.unwrap();
    assert_eq!((pending.sequence, pending.requested_at), (2, 11));
    clock.set(1999, 999);
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    assert_eq!(count("a"), 1);
    assert!(db.source_refresh_request("a").await?.is_some());
    clock.set(2000, 1000);
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    assert_eq!((count("a"), count("b")), (2, 1));
    assert_eq!(db.source_refresh_request("a").await?, None);
    clock.set(12000, 11000);
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    assert_eq!(count("a"), 2, "a served request does not repeat");

    // Upstream Retry-After outranks a manual request, which stays pending until the boundary.
    http.state.statuses.lock().unwrap().insert("a".into(), 429);
    http.state
        .retry_after_s
        .lock()
        .unwrap()
        .insert("a".into(), 100);
    db.request_source_refresh("a", 12).await?;
    clock.set(13000, 12000);
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    assert_eq!(count("a"), 3);
    db.request_source_refresh("a", 13).await?;
    clock.set(112_999, 111_999);
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    assert_eq!(count("a"), 3);
    assert!(db.source_refresh_request("a").await?.is_some());
    let revision = db.source_configuration("a").await?.config_revision;
    assert_eq!(
        db.source_sync_status("a").await?,
        Some(calendar_storage::SourceSyncStatus {
            config_revision: revision,
            attempted_at: 13000,
            outcome: calendar_storage::SourceSyncOutcome::RetryAfter,
            consecutive_failures: 1,
            next_allowed_at: 113_000,
        })
    );

    // The request and the upstream wait both survive a restart: a fresh scanner with a new
    // monotonic origin still waits for the persisted boundary, then serves the request.
    drop(scanner);
    services.drain().await;
    let services = service(&path).await?;
    let db = &services.storage;
    assert!(db.source_refresh_request("a").await?.is_some());
    http.state.statuses.lock().unwrap().insert("a".into(), 200);
    let mut scanner = SourceScanner::new(limits);
    clock.set(112_999, 0);
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    assert_eq!(count("a"), 3);
    clock.set(113_000, 1);
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    assert_eq!(count("a"), 4);
    assert_eq!(db.source_refresh_request("a").await?, None);
    let status = db.source_sync_status("a").await?.unwrap();
    assert_eq!(
        (
            status.outcome,
            status.consecutive_failures,
            status.next_allowed_at
        ),
        (calendar_storage::SourceSyncOutcome::NotModified, 0, 114_000)
    );

    // A request made against a superseded configuration is dropped, not replayed later.
    db.request_source_refresh("a", 14).await?;
    let record = db.source_configuration("a").await?;
    let mut config = record.configuration;
    config.name = "renamed".into();
    db.put_source_configuration(
        "a",
        ConfigurationCondition::Revision(record.config_revision),
        config,
        "test",
        1000,
    )
    .await?;
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    assert_eq!(db.source_refresh_request("a").await?, None);
    services.drain().await;
    Ok(())
}
#[tokio::test]
async fn restart_reconstructs_remaining_period_and_handles_future_verified_time() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    let services = service(&path).await?;
    let http = Http::new().await?;
    source(&services, "a", SourceMode::Pull, true).await?;
    let clock = Time::new();
    let runner = http.runner();
    SourceScanner::new(limits(8))
        .round(&services, &runner, &clock, |_| {})
        .await?;
    services.drain().await;
    let services = service(&path).await?;
    clock.set(31000, 5000);
    let mut scanner = SourceScanner::new(limits(8));
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |_| {})
            .await?
            .attempted,
        0
    );
    clock.set(60999, 34999);
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |_| {})
            .await?
            .attempted,
        0
    );
    clock.set(61000, 35000);
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |_| {})
            .await?
            .attempted,
        1
    );
    let mut restarted = SourceScanner::new(limits(8));
    clock.set(0, 0);
    assert_eq!(
        restarted
            .round(&services, &runner, &clock, |_| {})
            .await?
            .attempted,
        0
    );
    clock.set(1_000_000, 59999);
    assert_eq!(
        restarted
            .round(&services, &runner, &clock, |_| {})
            .await?
            .attempted,
        0
    );
    clock.set(1_000_001, 60000);
    assert_eq!(
        restarted
            .round(&services, &runner, &clock, |_| {})
            .await?
            .attempted,
        1
    );
    assert_eq!(http.requests().len(), 3);
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn old_configuration_is_fenced_before_network_and_inflight_edits_or_retirement_cannot_commit(
) -> Result {
    for retire in [false, true] {
        let dir = tempfile::tempdir()?;
        let services = service(&dir.path().join("db")).await?;
        let http = Http::new().await?;
        let old = source(&services, "a", SourceMode::Pull, true).await?;
        let clock = Time::new();
        let runner = http.runner();
        let mut scanner = SourceScanner::new(limits(8));
        scanner.round(&services, &runner, &clock, |_| {}).await?;
        let current = services.storage.source_configuration("a").await?;
        assert!(matches!(
            runner.run(&services.storage, &old, 1000).await,
            Err(SourceRunError::Sync(IngestionError::Conflict))
        ));
        assert_eq!(
            http.requests().len(),
            1,
            "old pinned record must fail before HTTP"
        );
        let gate = Arc::new(Gate {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        });
        *http.state.gate.lock().unwrap() = Some(gate.clone());
        clock.set(61000, 60000);
        let mut progress = vec![];
        let round = scanner.round(&services, &runner, &clock, |p| progress.push(p));
        let edit = async {
            gate.entered.acquire().await.unwrap().forget();
            if retire {
                services
                    .storage
                    .retire_source_configuration("a", current.config_revision, "test", 61000)
                    .await
                    .unwrap();
            } else {
                let mut config = current.configuration;
                config.name = "changed".into();
                services
                    .storage
                    .put_source_configuration(
                        "a",
                        ConfigurationCondition::Revision(current.config_revision),
                        config,
                        "test",
                        61000,
                    )
                    .await
                    .unwrap();
            }
            gate.release.add_permits(1);
        };
        let (result, ()) = tokio::join!(round, edit);
        assert_eq!(result?.attempted, 1);
        assert!(progress.iter().any(|p| matches!(
            p,
            SourceProgress::ExternalFailure {
                error: SourceRunError::Sync(IngestionError::Conflict),
                ..
            }
        )));
        assert_eq!(
            services.storage.source_state("a").await?.last_verified_at,
            if retire { Some(1000) } else { None }
        );
        let title: String = sqlx::query_scalar(
            "SELECT json_extract(document_json,'$.title') FROM events WHERE source_id='a'",
        )
        .fetch_one(services.storage.pool())
        .await?;
        assert_eq!(title, "Original");
        if retire {
            assert_eq!(
                scanner
                    .round(&services, &runner, &clock, |_| {})
                    .await?
                    .attempted,
                0
            );
            assert_eq!(http.requests().len(), 2);
        }
        services.drain().await;
    }
    Ok(())
}

#[tokio::test]
async fn bounded_discovery_attempts_capacity_and_stop_are_observable() -> Result {
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    let http = Http::new().await?;
    let runner = http.runner();
    let clock = Time::new();
    for id in ["a", "b", "c"] {
        source(&services, id, SourceMode::Pull, true).await?;
    }
    let mut scanner = SourceScanner::new(limits(1));
    assert_eq!(
        scanner.round(&services, &runner, &clock, |_| {}).await?.end,
        SourceRoundEnd::AttemptLimit
    );
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |_| {})
            .await?
            .attempted,
        1
    );
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |_| {})
            .await?
            .attempted,
        1
    );
    assert_eq!(
        http.requests(),
        [
            ("a".into(), false),
            ("b".into(), false),
            ("c".into(), false)
        ]
    );
    let small = SourceScanLimits::new(
        1,
        8,
        1,
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(60),
    )?;
    assert_eq!(
        SourceScanner::new(small)
            .round(&services, &runner, &clock, |_| {})
            .await,
        Err(SourceScanError::Capacity)
    );
    let mut seen = 0;
    scanner
        .run(&services, &runner, &clock, |p| {
            if matches!(p, SourceProgress::Scanned { .. }) {
                seen += 1;
                services.stop();
            }
        })
        .await?;
    assert_eq!(seen, 1);
    assert_eq!(http.requests().len(), 3);
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn pagination_and_monotonic_round_budget_are_bounded() -> Result {
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    let http = Http::new().await?;
    for i in 0..101 {
        source(&services, &format!("s{i:03}"), SourceMode::Pull, false).await?;
    }
    let clock = Time::new();
    let runner = http.runner();
    let mut scanner = SourceScanner::new(limits(2));
    assert_eq!(
        scanner.round(&services, &runner, &clock, |_| {}).await?.end,
        SourceRoundEnd::PageLimit
    );
    assert_eq!(
        scanner.round(&services, &runner, &clock, |_| {}).await?.end,
        SourceRoundEnd::Complete
    );
    let round = scanner
        .round(&services, &runner, &clock, |_| {
            clock.mono.store(120000, Ordering::SeqCst)
        })
        .await?;
    assert_eq!(round.end, SourceRoundEnd::Deadline);
    assert_eq!(round.attempted, 0);
    assert!(http.requests().is_empty());
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn stopping_drains_admitted_http_sync_and_never_fetches_the_next_source() -> Result {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("db");
    let services = service(&path).await?;
    let http = Http::new().await?;
    source(&services, "a", SourceMode::Pull, true).await?;
    source(&services, "b", SourceMode::Pull, true).await?;
    let gate = Arc::new(Gate {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
    });
    *http.state.gate.lock().unwrap() = Some(gate.clone());
    let worker_services = services.clone();
    let runner = http.runner();
    let worker = tokio::spawn(async move {
        SourceScanner::new(limits(8))
            .run(&worker_services, &runner, &Time::new(), |_| {})
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), gate.entered.acquire())
        .await??
        .forget();
    services.stop();
    let draining = services.clone();
    let drain = tokio::spawn(async move {
        draining.drain().await;
    });
    assert!(!drain.is_finished());
    gate.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(2), worker).await???;
    tokio::time::timeout(Duration::from_secs(2), drain).await??;
    let reopened = service(&path).await?;
    assert!(reopened.storage.source_state("a").await?.has_snapshot);
    assert!(!reopened.storage.source_state("b").await?.has_snapshot);
    assert_eq!(http.requests(), [("a".into(), false)]);
    reopened.drain().await;
    Ok(())
}

#[tokio::test]
async fn unsupported_mapping_and_credential_kinds_fail_closed_without_http() -> Result {
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    let http = Http::new().await?;
    let record = source(&services, "a", SourceMode::Pull, true).await?;
    let mut config = record.configuration;
    let mut calendar = config.calendars[0].clone();
    calendar.id = "second".into();
    calendar.external_id = "second".into();
    config.calendars.push(calendar);
    services
        .storage
        .put_source_configuration(
            "a",
            ConfigurationCondition::Revision(record.config_revision),
            config,
            "test",
            1000,
        )
        .await?;
    let mut progress = vec![];
    SourceScanner::new(limits(8))
        .round(&services, &http.runner(), &Time::new(), |p| {
            progress.push(p)
        })
        .await?;
    assert!(progress.iter().any(|p| matches!(
        p,
        SourceProgress::ExternalFailure {
            error: SourceRunError::Unsupported,
            ..
        }
    )));
    assert!(http.requests().is_empty());
    for kind in ["basic", "bearer", "bark"] {
        // Only immutable kind metadata is exercised; these bytes are not decryptable credentials.
        sqlx::query("INSERT INTO credentials(id,kind,ciphertext,nonce,key_version,rotated_at) VALUES (?,?,zeroblob(17),zeroblob(24),1,1000)")
            .bind(kind).bind(kind).execute(services.storage.pool()).await?;
    }
    assert!(matches!(
        services.storage.source_authentication(None).await?,
        RemoteAuthentication::None
    ));
    assert!(
        matches!(services.storage.source_authentication(Some("basic")).await?,RemoteAuthentication::Basic{credential_id} if credential_id=="basic")
    );
    assert!(
        matches!(services.storage.source_authentication(Some("bearer")).await?,RemoteAuthentication::Bearer{credential_id} if credential_id=="bearer")
    );
    assert!(matches!(
        services.storage.source_authentication(Some("bark")).await,
        Err(IngestionError::Unavailable)
    ));
    assert!(matches!(
        services
            .storage
            .source_authentication(Some("missing"))
            .await,
        Err(IngestionError::Unavailable)
    ));
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn conditional_reads_can_be_disabled_and_storage_errors_are_not_external_progress() -> Result
{
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    let http = Http::new().await?;
    let record = source(&services, "a", SourceMode::Pull, true).await?;
    let mut config = record.configuration;
    config.capabilities.conditional_read = false;
    services
        .storage
        .put_source_configuration(
            "a",
            ConfigurationCondition::Revision(record.config_revision),
            config,
            "test",
            1000,
        )
        .await?;
    let clock = Time::new();
    let mut scanner = SourceScanner::new(limits(8));
    let runner = http.runner();
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    clock.set(61000, 60000);
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    assert_eq!(http.requests(), [("a".into(), false), ("a".into(), false)]);
    let record = services.storage.source_configuration("a").await?;
    let bad = IcsSourceRunner {
        fetcher: Arc::new(Unexpected304),
        ics_limits: IcsLimits::default(),
        sync_limits: SyncLimits::default(),
    };
    assert!(matches!(
        bad.run(&services.storage, &record, 121000).await,
        Err(SourceRunError::Sync(IngestionError::UnexpectedNotModified))
    ));
    assert_eq!(
        services.storage.source_state("a").await?.last_verified_at,
        Some(61000)
    );
    services.storage.close().await;
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |_| panic!(
                "failed storage must not report completed progress"
            ))
            .await,
        Err(SourceScanError::Storage)
    );
    assert!(SourceScanLimits::new(
        0,
        1,
        1,
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(1),
        Duration::from_secs(2)
    )
    .is_err());
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn timed_out_http_sync_is_bounded_and_keeps_the_trusted_snapshot() -> Result {
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    let http = Http::new().await?;
    source(&services, "a", SourceMode::Pull, true).await?;
    let clock = Time::new();
    let mut scanner = SourceScanner::new(limits(8));
    let mut runner = http.runner();
    scanner.round(&services, &runner, &clock, |_| {}).await?;
    let before = services.storage.source_state("a").await?;
    *http.state.gate.lock().unwrap() = Some(Arc::new(Gate {
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
    }));
    runner.sync_limits.timeout = Duration::from_secs(1);
    clock.set(61000, 60000);
    let mut progress = vec![];
    let round = tokio::time::timeout(
        Duration::from_secs(3),
        scanner.round(&services, &runner, &clock, |p| progress.push(p)),
    )
    .await??;
    assert_eq!(round.attempted, 1);
    assert!(progress.iter().any(|p| matches!(
        p,
        SourceProgress::ExternalFailure {
            error: SourceRunError::Sync(IngestionError::Timeout),
            ..
        }
    )));
    assert_eq!(http.requests().len(), 2);
    assert_eq!(services.storage.source_state("a").await?, before);
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn replacing_later_inactive_sources_reclaims_capacity_without_erasing_active_backoff(
) -> Result {
    for failed_peer in [false, true] {
        let dir = tempfile::tempdir()?;
        let services = service(&dir.path().join("db")).await?;
        let http = Http::new().await?;
        if failed_peer {
            source(&services, "m", SourceMode::Pull, true).await?;
            http.state.statuses.lock().unwrap().insert("m".into(), 503);
        }
        let z = source(&services, "z", SourceMode::Pull, true).await?;
        let max_tracked = if failed_peer { 2 } else { 1 };
        let limits = SourceScanLimits::new(
            1,
            8,
            max_tracked,
            Duration::from_secs(120),
            Duration::from_secs(60),
            Duration::from_secs(60),
            Duration::from_secs(240),
        )?;
        let clock = Time::new();
        let mut scanner = SourceScanner::new(limits);
        let runner = http.runner();
        scanner.round(&services, &runner, &clock, |_| {}).await?;
        let mut config = z.configuration;
        config.enabled = false;
        services
            .storage
            .put_source_configuration(
                "z",
                ConfigurationCondition::Revision(z.config_revision),
                config,
                "test",
                1000,
            )
            .await?;
        source(&services, "a", SourceMode::Pull, true).await?;
        let mut progress = vec![];
        assert_eq!(
            scanner
                .round(&services, &runner, &clock, |p| progress.push(p))
                .await?
                .attempted,
            1
        );
        assert!(progress.iter().any(|p|matches!(p,SourceProgress::Synchronized{source_id,external_degraded,..} if source_id=="a"&&*external_degraded==failed_peer)));
        let requests = http.requests();
        assert_eq!(requests.iter().filter(|(id, _)| id == "a").count(), 1);
        assert_eq!(requests.iter().filter(|(id, _)| id == "z").count(), 1);
        assert_eq!(
            requests.iter().filter(|(id, _)| id == "m").count(),
            usize::from(failed_peer),
            "active failed source must retain its backoff"
        );
        services.drain().await;
    }
    Ok(())
}

struct MetadataElapsed(AtomicUsize);
impl Clock for MetadataElapsed {
    fn utc_now_ms(&self) -> i64 {
        1000
    }
    fn monotonic_now_ms(&self) -> u64 {
        // Initial and pre-query admission samples fit; the first post-metadata
        // sample reaches the deadline. SQLite reads remain real, with no sleeps.
        if self.0.fetch_add(1, Ordering::SeqCst) < 3 {
            0
        } else {
            120000
        }
    }
}
#[tokio::test]
async fn metadata_consuming_the_round_budget_never_admits_http_and_preserves_continuation() -> Result
{
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    let http = Http::new().await?;
    source(&services, "a", SourceMode::Pull, true).await?;
    let clock = MetadataElapsed(AtomicUsize::new(0));
    let runner = http.runner();
    let mut scanner = SourceScanner::new(limits(8));
    let round = scanner.round(&services, &runner, &clock, |_| {}).await?;
    assert_eq!((round.attempted, round.end), (0, SourceRoundEnd::Deadline));
    assert!(http.requests().is_empty());
    assert_eq!(
        scanner
            .round(&services, &runner, &clock, |_| {})
            .await?
            .attempted,
        1,
        "the deferred source must remain discoverable next round"
    );
    assert_eq!(http.requests().len(), 1);
    services.drain().await;
    Ok(())
}

#[tokio::test]
async fn schedule_revalidation_query_accepts_its_bound_and_rejects_excess() -> Result {
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    source(&services, "a", SourceMode::Pull, true).await?;
    let mut ids: Vec<_> = (0..16_382).map(|i| format!("absent_{i}")).collect();
    ids.push("a".into());
    ids.push("' OR 1=1--".into());
    assert_eq!(services.storage.active_pull_source_ids(&ids).await?, ["a"]);
    ids.push("extra".into());
    assert!(matches!(
        services.storage.active_pull_source_ids(&ids).await,
        Err(IngestionError::Budget)
    ));
    services.drain().await;
    Ok(())
}
#[tokio::test]
async fn restart_restores_persisted_waits_bounded_against_utc_jumps_and_ignores_old_revisions(
) -> Result {
    use calendar_storage::{SourceSyncOutcome, SourceSyncStatus};
    let dir = tempfile::tempdir()?;
    let services = service(&dir.path().join("db")).await?;
    let http = Http::new().await?;
    let db = &services.storage;
    // No source has a trusted verification, so without restored state each would sync at once.
    const NOW: i64 = 1_000_000_000;
    const DAY: i64 = 86_400_000;
    for (id, revision_offset, attempted_at, outcome, failures, next_allowed_at) in [
        // UTC moved back behind the attempt: wait the whole recorded span, no less.
        (
            "back",
            0,
            NOW + 1000,
            SourceSyncOutcome::RetryAfter,
            1,
            NOW + 1000 + 3_600_000,
        ),
        // A corrupt or excessive span is capped at seven days, never a permanent stall.
        (
            "cap",
            0,
            NOW,
            SourceSyncOutcome::RetryAfter,
            1,
            NOW + 30 * DAY,
        ),
        // Exponential local backoff continues from the persisted failure count (60 s * 2^2).
        (
            "fails",
            0,
            NOW,
            SourceSyncOutcome::Unavailable,
            3,
            NOW + 60_000,
        ),
        // The wait has already elapsed.
        (
            "ahead",
            0,
            NOW - 10 * DAY,
            SourceSyncOutcome::RetryAfter,
            1,
            NOW - 10 * DAY + 60_000,
        ),
        // A status of a superseded configuration revision never constrains the new one.
        (
            "stale",
            -1,
            NOW,
            SourceSyncOutcome::RetryAfter,
            1,
            NOW + 3_600_000,
        ),
    ] {
        let record = source(&services, id, SourceMode::Pull, true).await?;
        db.record_source_sync_status(
            id,
            SourceSyncStatus {
                config_revision: record.config_revision + revision_offset,
                attempted_at,
                outcome,
                consecutive_failures: failures,
                next_allowed_at,
            },
        )
        .await?;
    }
    db.request_source_refresh("back", 1).await?;
    http.state
        .statuses
        .lock()
        .unwrap()
        .insert("fails".into(), 503);
    let clock = Time::new();
    let runner = http.runner();
    let mut scanner = SourceScanner::new(limits(8));
    let count = |id: &str| http.requests().iter().filter(|(r, _)| r == id).count();
    macro_rules! at {
        ($mono:expr) => {{
            clock.set(NOW, $mono);
            scanner.round(&services, &runner, &clock, |_| {})
        }};
    }
    at!(0).await?;
    assert_eq!((count("ahead"), count("stale")), (1, 1));
    assert_eq!((count("back"), count("cap"), count("fails")), (0, 0, 0));
    at!(239_999).await?;
    assert_eq!(count("fails"), 0);
    at!(240_000).await?;
    assert_eq!(count("fails"), 1);
    let fails = db.source_sync_status("fails").await?.unwrap();
    assert_eq!(
        (
            fails.outcome,
            fails.consecutive_failures,
            fails.attempted_at
        ),
        (SourceSyncOutcome::Adapter, 4, NOW)
    );
    assert_eq!(fails.next_allowed_at, NOW + 60_000);
    at!(3_599_999).await?;
    assert_eq!(count("back"), 0, "a manual refresh obeys the restored wait");
    assert!(db.source_refresh_request("back").await?.is_some());
    at!(3_600_000).await?;
    assert_eq!(count("back"), 1);
    assert_eq!(db.source_refresh_request("back").await?, None);
    let back = db.source_sync_status("back").await?.unwrap();
    assert_eq!(
        (
            back.outcome,
            back.consecutive_failures,
            back.next_allowed_at
        ),
        (SourceSyncOutcome::Success, 0, NOW + 60_000)
    );
    at!(604_799_999).await?;
    assert_eq!(count("cap"), 0);
    at!(604_800_000).await?;
    assert_eq!(count("cap"), 1);
    services.drain().await;
    Ok(())
}
