//! Real local HTTP process: no request target or capability may appear in stdout/stderr.
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
#[test]
fn capability_urls_are_absent_from_process_logs_on_success_and_errors() {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("db");
    let (token, aggregate_url, aggregate_token) = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let db = calendar_storage::Storage::open(&database).await.unwrap();
        sqlx::query("INSERT INTO sources(id,kind) VALUES ('s','test')")
            .execute(db.pool())
            .await
            .unwrap();
        let token = db
            .issue_feed_token("sub", "public", "s", None)
            .await
            .unwrap();
        use calendar_application::{aggregate::AggregateConfiguration, configuration::ConfigurationCondition as Condition};
        let mut config: AggregateConfiguration=serde_json::from_value(serde_json::json!({"version":1,"name":"Empty","enabled":false,"sources":[],"visible_fields":[],"stale_policy":"reject","expires_at":null})).unwrap();
        let (r,secret)=db.put_aggregate("aggregate",Condition::Absent,config.clone(),"test",1).await.unwrap();
        config.enabled=true;db.put_aggregate("aggregate",Condition::Revision(1),config,"test",2).await.unwrap();
        let secret=secret.unwrap();let secret=std::str::from_utf8(secret.expose()).unwrap().to_owned();
        let url=format!("/feeds/aggregates/{}/{}.ics",r.public_id,secret);
        db.close().await;
        (std::str::from_utf8(token.expose()).unwrap().to_owned(),url,secret)
    });
    let deployment = dir.path().join("deployment.json");
    std::fs::write(&deployment, r#"{"bind":"127.0.0.1:0"}"#).unwrap();
    let stdout = dir.path().join("stdout");
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_calendar-server"))
            .env_clear()
            .args(["--deployment-config", deployment.to_str().unwrap()])
            .env("CALRELAY_DATABASE", &database)
            .stdout(std::fs::File::create(&stdout).unwrap())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stderr = process.0.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut all = String::new();
        for line in BufReader::new(stderr).lines() {
            let line = line.unwrap();
            let _ = tx.send(line.clone());
            all.push_str(&line);
            all.push('\n');
        }
        all
    });
    let ready = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let address = ready.strip_prefix("CalRelay listening on ").unwrap();
    let url = format!("/feeds/public/{token}.ics");
    for (method, target, status) in [
        ("GET", aggregate_url.clone(), 200),
        ("HEAD", aggregate_url.clone(), 200),
        ("POST", aggregate_url.clone(), 405),
        (
            "GET",
            format!("/feeds/aggregates/unknown/{aggregate_token}.ics"),
            404,
        ),
        (
            "GET",
            format!("/feeds/aggregates/unknown/%FF{aggregate_token}.ics"),
            400,
        ),
        ("GET", url.clone(), 200),
        ("HEAD", url.clone(), 200),
        ("POST", url.clone(), 405),
        ("GET", format!("/feeds/unknown/{token}.ics"), 404),
        ("GET", format!("/feeds/public/%FF{token}.ics"), 400),
    ] {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        write!(
            stream,
            "{method} {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(
            response.starts_with(&format!("HTTP/1.1 {status}")),
            "{response}"
        );
        assert!(!response.contains(&token));
        assert!(!response.contains(&aggregate_token));
        assert!(!response.contains(&target));
    }
    assert!(Command::new("kill")
        .args(["-TERM", &process.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let diagnostic = reader.join().unwrap() + &std::fs::read_to_string(stdout).unwrap();
    assert!(diagnostic.contains("CalRelay listening on"));
    for hidden in [
        &token,
        &url,
        &aggregate_token,
        &aggregate_url,
        "/feeds/",
        "%FF",
    ] {
        assert!(!diagnostic.contains(hidden));
    }
}
