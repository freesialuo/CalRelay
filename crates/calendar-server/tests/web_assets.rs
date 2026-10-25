#![cfg(feature = "production-web")]
#[allow(dead_code)]
#[path = "../build.rs"]
mod web_build;

use calendar_server::{Config, Server};
use std::time::Duration;

#[tokio::test]
async fn built_assets_share_real_http_without_swallowing_api_or_paths() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::initialize(Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        database: dir.path().join("web.db"),
        max_request_bytes: 1024,
        max_response_bytes: 1_048_576,
        request_timeout: Duration::from_secs(5),
    })
    .await
    .unwrap();
    let listener = server.bind().await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(server.serve(listener, async {
        let _ = stopped.await;
    }));
    let client = reqwest::Client::new();
    let home = client.get(format!("{base}/admin/")).send().await.unwrap();
    assert_eq!(home.status(), 200);
    assert_eq!(home.headers()["cache-control"], "no-store");
    assert_eq!(home.headers()["x-content-type-options"], "nosniff");
    assert_eq!(home.headers()["referrer-policy"], "no-referrer");
    assert_eq!(home.headers()["x-frame-options"], "DENY");
    let csp = home.headers()["content-security-policy"].to_str().unwrap();
    let directives: std::collections::BTreeMap<_, _> = csp
        .split("; ")
        .map(|directive| directive.split_once(' ').unwrap())
        .collect();
    // These exact origins must compose with the real Turnstile page's meta CSP.
    assert_eq!(
        directives["script-src"],
        "'self' https://challenges.cloudflare.com"
    );
    assert_eq!(directives["frame-src"], "https://challenges.cloudflare.com");
    assert_eq!(directives["connect-src"], "'self'");
    assert_eq!(directives["style-src"], "'self'");
    assert_eq!(directives["frame-ancestors"], "'none'");
    assert!(!csp.contains("unsafe-") && !csp.contains('*'));
    let html = home.text().await.unwrap();
    assert!(html.contains("CalRelay"));
    assert!(!html.contains("/src/main.ts"));
    let mut count = 0;
    for asset in html.split('"').filter(|s| s.starts_with("/admin/assets/")) {
        count += 1;
        let response = client.get(format!("{base}{asset}")).send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["cache-control"],
            "public, max-age=31536000, immutable"
        );
        let mime = response.headers()["content-type"].to_str().unwrap();
        assert!(mime.starts_with(if asset.ends_with(".js") {
            "text/javascript"
        } else {
            "text/css"
        }));
        let content = response.bytes().await.unwrap();
        assert!(!content.is_empty());
        let head = client.head(format!("{base}{asset}")).send().await.unwrap();
        assert_eq!(head.status(), 200);
        assert!(head.bytes().await.unwrap().is_empty());
    }
    assert_eq!(count, 2);
    for path in [
        "/api/missing",
        "/health/missing",
        "/feed/missing",
        "/admin/missing",
        "/admin/assets/missing.js",
        "/admin/assets/%2e%2e%2findex.html",
        "/admin/assets/%252e%252e%252findex.html",
        "/admin/assets/.env",
        "/admin/assets/source.js.map",
    ] {
        let response = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(response.status(), 404, "{path}");
        assert_eq!(
            response.json::<serde_json::Value>().await.unwrap()["error"]["code"],
            "not_found"
        );
    }
    let response = client.post(format!("{base}/admin/")).send().await.unwrap();
    assert_eq!(response.status(), 405);
    let response = client
        .get(format!("{base}/api/v1/version"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["name"],
        "CalRelay"
    );
    assert_eq!(
        client
            .get(format!("{base}/health/live"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}
