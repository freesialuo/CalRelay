#![cfg(unix)]
mod common;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn first_line(reader: impl std::io::Read + Send + 'static) -> String {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(reader).read_line(&mut line);
        let _ = send.send(line);
    });
    receive
        .recv_timeout(Duration::from_secs(10))
        .expect("local child startup timed out")
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap()
}
#[tokio::test]
async fn real_standard_binary_mounts_guard_and_redacts_invalid_secret_before_listen() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret");
    std::fs::write(&secret, b"synthetic-production-test-secret").unwrap();
    let deployment = dir.path().join("deployment.json");
    let mut document = json!({"bind":"127.0.0.1:0","admin":{"team":"synthetic-team","audience":"admin-aud","origin":"https://admin.example.test","hostname":"admin.example.test","action":"admin-login","site_key":"synthetic-site-key","administrators":{"alice-sub":"alice@example.test"},"session_seconds":120,"turnstile_secret_file":secret}});
    document["credentials"] = common::deployment(dir.path(), "127.0.0.1:0")["credentials"].clone();
    std::fs::write(&deployment, document.to_string()).unwrap();
    let mut child = Process(
        Command::new(env!("CARGO_BIN_EXE_calendar-server"))
            .env_clear()
            .arg("--deployment-config")
            .arg(&deployment)
            .env("CALRELAY_DATABASE", dir.path().join("db"))
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let line = first_line(child.0.stderr.take().unwrap());
    let addr = line.trim().strip_prefix("CalRelay listening on ").unwrap();
    let base = format!("http://{addr}");
    let response = client()
        .get(format!("{base}/api/admin/v1/bootstrap"))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.headers()["cache-control"], "no-store");
    let value: Value = response.json().await.unwrap();
    assert_eq!(
        value,
        json!({"enabled":true,"turnstile":{"site_key":"synthetic-site-key","hostname":"admin.example.test","action":"admin-login"}})
    );
    let response = client()
        .post(format!("{base}/api/admin/v1/session"))
        .header("origin", "https://admin.example.test")
        .header("cf-access-jwt-assertion", "forged")
        .json(&json!({"turnstile_token":"synthetic-never-network"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 403);
    assert!(Command::new("kill")
        .args(["-TERM", &child.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    assert!(child.0.wait().unwrap().success());
    std::fs::write(&secret, b"synthetic-production-test-secret\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_calendar-server"))
        .env_clear()
        .arg("--deployment-config")
        .arg(&deployment)
        .env("CALRELAY_DATABASE", dir.path().join("must-not-exist"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(
        String::from_utf8(result.stderr).unwrap(),
        "invalid or missing server configuration: admin secret unavailable or invalid\n"
    );
    assert!(!dir.path().join("must-not-exist").exists());
}

#[cfg(feature = "test-harness")]
#[tokio::test]
async fn offline_harness_uses_real_guard_and_sqlite_and_rejects_replay_and_cross_identity() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("harness.db");
    let browser_origin = "https://localhost:4443";
    let mut child = Process(
        Command::new(env!("CARGO_BIN_EXE_calendar-admin-harness"))
            .env_clear()
            .args([
                "--database",
                database.to_str().unwrap(),
                "--browser-origin",
                browser_origin,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let fixture: Value = serde_json::from_str(&first_line(child.0.stdout.take().unwrap())).unwrap();
    assert_eq!(fixture["synthetic_only"], true);
    let base = fixture["upstream"].as_str().unwrap();
    let jwt = fixture["access_jwt"].as_str().unwrap();
    let client = client();
    let bootstrap: Value = client
        .get(format!("{base}/api/admin/v1/bootstrap"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bootstrap["turnstile"]["site_key"], "synthetic-site-key");
    assert_eq!(bootstrap["turnstile"]["hostname"], "localhost");
    // Locally sign additional negative identities/expiry with the same public synthetic fixture.
    for (field, value) in [
        ("sub", json!("other-sub")),
        ("email", json!("other@example.test")),
        ("exp", json!(1)),
    ] {
        let mut claims = json!({"iss":"https://synthetic-team.cloudflareaccess.com","aud":["synthetic-admin"],"type":"app","sub":"synthetic-admin","email":"admin@example.test","iat":0,"nbf":0,"exp":4102444800_i64});
        claims[field] = value;
        let forged = synthetic_jwt(claims);
        assert_eq!(
            client
                .post(format!("{base}/api/admin/v1/session"))
                .header("cf-access-jwt-assertion", forged)
                .header("origin", browser_origin)
                .json(&json!({"turnstile_token":"synthetic-challenge:negative"}))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    let login = |token: &str, origin: &str, challenge: &str| {
        client
            .post(format!("{base}/api/admin/v1/session"))
            .header("cf-access-jwt-assertion", token)
            .header("origin", origin)
            .json(&json!({"turnstile_token":challenge}))
    };
    for (token, origin, challenge) in [
        ("forged", browser_origin, "synthetic-challenge:one"),
        (jwt, "https://evil.test", "synthetic-challenge:one"),
        (jwt, browser_origin, "forged-challenge"),
    ] {
        assert_eq!(
            login(token, origin, challenge)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    let response = login(jwt, browser_origin, "synthetic-challenge:first")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let session: Value = response.json().await.unwrap();
    assert_eq!(
        login(jwt, browser_origin, "synthetic-challenge:first")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let mut source: Value = serde_json::from_str(include_str!(
        "../../../examples/source-configuration-v1.json"
    ))
    .unwrap();
    source["adapter"] = json!("standard-calendar");
    source["mode"] = json!("push-only");
    let write = |nonce: &str| {
        client
            .put(format!("{base}/api/admin/v1/sources/harness-source"))
            .header("cf-access-jwt-assertion", jwt)
            .header("cookie", &cookie)
            .header("origin", browser_origin)
            .header("x-csrf-token", session["csrf_token"].as_str().unwrap())
            .header("x-admin-write-nonce", nonce)
            .header("if-none-match", "*")
            .json(&source)
    };
    assert_eq!(write("forged").send().await.unwrap().status(), 403);
    let mut missing_csrf = write(session["write_nonce"].as_str().unwrap())
        .build()
        .unwrap();
    missing_csrf.headers_mut().remove("x-csrf-token");
    assert_eq!(client.execute(missing_csrf).await.unwrap().status(), 403);

    let nonce = session["write_nonce"].as_str().unwrap();
    let response = write(nonce).send().await.unwrap();
    assert_eq!(response.status(), 201);
    let next = response.headers()["x-admin-write-nonce"]
        .to_str()
        .unwrap()
        .to_owned();
    let saved: Value = response.json().await.unwrap();
    assert_ne!(next, nonce);
    assert_eq!(write(nonce).send().await.unwrap().status(), 403);
    let list: Value = client
        .get(format!("{base}/api/admin/v1/sources"))
        .header("cf-access-jwt-assertion", jwt)
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list
        .to_string()
        .contains(saved["source_id"].as_str().unwrap()));
    // Creation remains disabled; enabling uses the real configuration CAS and next nonce.
    let mut enabled_source = source.clone();
    enabled_source["enabled"] = json!(true);
    let response = client
        .put(format!("{base}/api/admin/v1/sources/harness-source"))
        .header("cf-access-jwt-assertion", jwt)
        .header("cookie", &cookie)
        .header("origin", browser_origin)
        .header("x-csrf-token", session["csrf_token"].as_str().unwrap())
        .header("x-admin-write-nonce", &next)
        .header(
            "if-match",
            format!("\"{}\"", saved["config_revision"].as_i64().unwrap()),
        )
        .json(&enabled_source)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_ne!(
        response.headers()["x-admin-write-nonce"].to_str().unwrap(),
        next
    );
    let db = calendar_storage::Storage::open(&database).await.unwrap();
    // Real management, machine ingest and feed routers coexist without sharing credentials.
    let api = db
        .issue_api_token("harness-api", "harness-source", true, true, None)
        .await
        .unwrap();
    let api = std::str::from_utf8(api.expose()).unwrap();
    let feed = db
        .issue_feed_token("harness-feed", "public", "harness-source", None)
        .await
        .unwrap();
    let feed = std::str::from_utf8(feed.expose()).unwrap();
    let event = json!({"calendar_id":source["calendars"][0]["id"],"event":{"schema_version":"1.0","external_uid":"synthetic-ingest","title":"Synthetic integration","status":"confirmed","start":{"kind":"utc","value":"2026-11-02T17:00:00Z"}}});
    assert_eq!(
        client
            .put(format!(
                "{base}/api/ingest/v1/sources/harness-source/events/synthetic-ingest"
            ))
            .bearer_auth(api)
            .header("idempotency-key", "integration-one")
            .header("if-none-match", "*")
            .json(&event)
            .send()
            .await
            .unwrap()
            .status(),
        201
    );
    let response = client
        .get(format!("{base}/feeds/public/{feed}.ics"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "private, no-cache");
    assert_eq!(
        client
            .get(format!("{base}/api/admin/v1/sources"))
            .header("cf-access-jwt-assertion", jwt)
            .header("cookie", format!("__Host-calrelay_admin={api}"))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    sqlx::query("UPDATE admin_sessions SET expires_at=created_at,created_at=created_at-1")
        .execute(db.pool())
        .await
        .unwrap();
    db.close().await;
    assert_eq!(
        client
            .get(format!("{base}/api/admin/v1/sources"))
            .header("cf-access-jwt-assertion", jwt)
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = login(jwt, browser_origin, "synthetic-challenge:second")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let session: Value = response.json().await.unwrap();
    let next = session["write_nonce"].as_str().unwrap();
    assert_eq!(
        client
            .delete(format!("{base}/api/admin/v1/session"))
            .header("cf-access-jwt-assertion", jwt)
            .header("cookie", &cookie)
            .header("origin", browser_origin)
            .header("x-csrf-token", session["csrf_token"].as_str().unwrap())
            .header("x-admin-write-nonce", next)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        client
            .get(format!("{base}/api/admin/v1/sources"))
            .header("cf-access-jwt-assertion", jwt)
            .header("cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert!(Command::new("kill")
        .args(["-TERM", &child.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    assert!(child.0.wait().unwrap().success());
    let db = calendar_storage::Storage::open(&database).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM sources")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(count, 1);
    db.close().await;
    // Harness never opens an existing database, and cannot bind a public interface.
    let result = Command::new(env!("CARGO_BIN_EXE_calendar-admin-harness"))
        .env_clear()
        .args([
            "--database",
            database.to_str().unwrap(),
            "--browser-origin",
            browser_origin,
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let result = Command::new(env!("CARGO_BIN_EXE_calendar-admin-harness"))
        .env_clear()
        .args([
            "--database",
            dir.path().join("never").to_str().unwrap(),
            "--browser-origin",
            "https://public.example.test",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!dir.path().join("never").exists());
}

#[cfg(feature = "test-harness")]
fn synthetic_jwt(claims: Value) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
    use ring::{
        rand::SystemRandom,
        signature::{RsaKeyPair, RSA_PKCS1_SHA256},
    };
    let fixture: Value = serde_json::from_str(include_str!(
        "../../calendar-adapters/tests/support/admin-fixture.json"
    ))
    .unwrap();
    let key =
        RsaKeyPair::from_pkcs8(&B64.decode(fixture["pkcs8"].as_str().unwrap()).unwrap()).unwrap();
    let input = format!(
        "{}.{}",
        B64.encode(
            serde_json::to_vec(&json!({"alg":"RS256","kid":"synthetic","typ":"JWT"})).unwrap()
        ),
        B64.encode(serde_json::to_vec(&claims).unwrap())
    );
    let mut sig = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        input.as_bytes(),
        &mut sig,
    )
    .unwrap();
    format!("{input}.{}", B64.encode(sig))
}
