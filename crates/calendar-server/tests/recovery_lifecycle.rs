//! #27/#37: server startup must preserve the durable recovery delivery policy.
//! All databases and jobs are synthetic; no external output is configured.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use calendar_application::{Clock, DeliveryPolicy, DeliveryRepository};
use calendar_server::{Config, Server};
use calendar_storage::{backup, Storage};
use std::{path::Path, time::Duration};
use tower::ServiceExt;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn config(path: &Path) -> Config {
    Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: path.into(),
        max_request_bytes: 1024,
        max_response_bytes: 16384,
        request_timeout: Duration::from_secs(2),
    }
}
async fn health(server: &Server) -> Result {
    let response = server
        .router()
        .oneshot(
            Request::builder()
                .uri("/health/ready")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "production workers are not assembled"
    );
    Ok(())
}
struct Time(i64);
impl Clock for Time {
    fn utc_now_ms(&self) -> i64 {
        self.0
    }
    fn monotonic_now_ms(&self) -> u64 {
        0
    }
}

async fn seed(db: &Storage) -> Result {
    sqlx::raw_sql(r#"
        INSERT INTO sources(id,kind,revision,last_verified_at) VALUES ('s','synthetic',1,1000);
        INSERT INTO calendars(id,source_id,external_id) VALUES ('c','s','main');
        INSERT INTO events(id,source_id,calendar_id,external_uid,revision,document_json,content_hash)
          VALUES ('e','s','c','a',1,'{"schema_version":"1.0","external_uid":"a","title":"Synthetic","status":"confirmed","start":{"kind":"utc","value":"2026-11-02T17:00:00Z"},"duration":"PT1H"}','h');
        INSERT INTO outputs(id,kind) VALUES ('o','synthetic');
        INSERT INTO reminder_rules(id,policy_json) VALUES ('r','{"calendar_ids":["c"]}');
        INSERT INTO rule_outputs VALUES ('r','o');
        INSERT INTO planning_watermarks(scope_id,source_id,rule_id,output_id,source_revision,rule_revision,output_revision,planned_until,tzdata_version)
          VALUES ('w','s','r','o',1,1,1,20000,'2025b');
        INSERT INTO logical_deliveries(id,event_id,original_key,rule_id,output_id)
          VALUES ('old','e','old','r','o'),('equal','e','equal','r','o'),('future','e','future','r','o');
        INSERT INTO delivery_jobs(id,logical_delivery_id,generation,event_revision,rule_revision,output_revision,due_at,state,next_attempt_at,planning_context)
          VALUES ('old','old',1,1,1,1,999,'pending',999,'[null,"2025b"]'),
                 ('equal','equal',1,1,1,1,1000,'pending',1000,'[null,"2025b"]'),
                 ('future','future',1,1,1,1,2000,'pending',2000,'[null,"2025b"]');
    "#).execute(db.pool()).await?;
    Ok(())
}

#[tokio::test]
async fn fresh_server_applies_migration_0009_before_ready() -> Result {
    let dir = tempfile::tempdir()?;
    let server = Server::initialize(config(&dir.path().join("fresh.sqlite3"))).await?;
    let success: bool = sqlx::query_scalar("SELECT success FROM _sqlx_migrations WHERE version=9")
        .fetch_one(server.services.storage.pool())
        .await?;
    assert!(success);
    let policy: (i64, Option<i64>) =
        sqlx::query_as("SELECT paused,cutoff FROM recovery_control WHERE id=1")
            .fetch_one(server.services.storage.pool())
            .await?;
    assert_eq!(policy, (0, None));
    health(&server).await?;
    server.services.drain().await;
    Ok(())
}

#[tokio::test]
async fn server_restart_preserves_recovery_pause_until_explicit_resume() -> Result {
    let dir = tempfile::tempdir()?;
    let original = dir.path().join("original.sqlite3");
    let snapshot = dir.path().join("snapshot");
    let destination = dir.path().join("isolated-recovery");
    let db = Storage::open(&original).await?;
    seed(&db).await?;
    backup::backup(&original, &snapshot).await?;
    db.close().await;
    backup::restore(&snapshot, &destination, 1000).await?;
    let recovered = destination.join("database.sqlite3");
    for _ in 0..2 {
        let server = Server::initialize(config(&recovered)).await?;
        health(&server).await?;
        let policy: (i64, i64) =
            sqlx::query_as("SELECT paused,cutoff FROM recovery_control WHERE id=1")
                .fetch_one(server.services.storage.pool())
                .await?;
        assert_eq!(policy, (1, 1000), "server startup must not resume delivery");
        let claim = server
            .services
            .run_task(async |storage: &Storage| {
                storage
                    .claim("synthetic", &DeliveryPolicy::default(), &Time(2000))
                    .await
            })
            .await
            .unwrap();
        assert!(
            !matches!(claim, Ok(Some(_))),
            "paused recovery must not acquire a lease"
        );
        let attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM delivery_attempts")
            .fetch_one(server.services.storage.pool())
            .await?;
        assert_eq!(attempts, 0);
        supervised_pause(server).await?;
    }
    backup::resume(&recovered).await?;
    let server = Server::initialize(config(&recovered)).await?;
    health(&server).await?;
    let policy: (i64, i64) =
        sqlx::query_as("SELECT paused,cutoff FROM recovery_control WHERE id=1")
            .fetch_one(server.services.storage.pool())
            .await?;
    assert_eq!(policy, (0, 1000));
    let mut jobs = Vec::new();
    for _ in 0..2 {
        jobs.push(
            server
                .services
                .run_task(async |storage: &Storage| {
                    storage
                        .claim("synthetic", &DeliveryPolicy::default(), &Time(2000))
                        .await
                })
                .await
                .unwrap()?
                .unwrap()
                .job_id,
        );
    }
    jobs.sort();
    assert_eq!(jobs, ["equal", "future"]);
    let old: String = sqlx::query_scalar("SELECT state FROM delivery_jobs WHERE id='old'")
        .fetch_one(server.services.storage.pool())
        .await?;
    assert_eq!(old, "expired");
    server.services.drain().await;
    Ok(())
}

// Exercise the real HTTP/DB probe loop on the recovered jobs, not just initialize/drop.
async fn supervised_pause(server: Server) -> Result {
    let services = server.services.clone();
    let mut changes = services.health.subscribe();
    let listener = server.bind().await?;
    let address = listener.local_addr()?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(server.serve(listener, async {
        let _ = rx.await;
    }));
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if matches!(
                services.health.snapshot().database,
                calendar_server::supervision::DatabaseState::Ready {
                    recovery_paused: true
                }
            ) {
                break;
            }
            changes.changed().await.unwrap();
        }
    })
    .await?;
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()?
        .get(format!("http://{address}/health/live"))
        .send()
        .await?;
    assert_eq!(response.status(), 200);
    assert!(!services.health.snapshot().ready);
    tx.send(()).unwrap();
    serving.await??;
    Ok(())
}
