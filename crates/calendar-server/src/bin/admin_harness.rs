//! OFFLINE TEST EXECUTABLE ONLY. Never compiled by default or selected by the production binary.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use calendar_adapters::admin_auth::{AdminConfig, AdminError, AdminTransport, CloudflareVerifier};
use calendar_application::SecretBytes;
use calendar_server::{Config, Server};
use ring::{
    rand::SystemRandom,
    signature::{RsaKeyPair, RSA_PKCS1_SHA256},
};
use serde_json::{json, Value};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
struct OfflineTransport {
    jwks: Vec<u8>,
    hostname: String,
}
impl AdminTransport for OfflineTransport {
    fn request(
        &self,
        url: String,
        form: Option<Vec<(String, String)>>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, AdminError>> + Send + '_>> {
        Box::pin(async move {
            if url == "https://synthetic-team.cloudflareaccess.com/cdn-cgi/access/certs" {
                return Ok(self.jwks.clone());
            }
            if url != "https://challenges.cloudflare.com/turnstile/v0/siteverify" {
                return Err(AdminError);
            }
            let form = form.ok_or(AdminError)?;
            let success = form
                .iter()
                .any(|(k, v)| k == "secret" && v == "synthetic-harness-secret")
                && form.iter().any(|(k, v)| {
                    k == "response"
                        && v.starts_with("synthetic-challenge:")
                        && v.len() > 20
                        && v.len() < 128
                });
            Ok(serde_json::to_vec(&json!({"success":success,"hostname":self.hostname,"action":"admin-login","challenge_ts":chrono::DateTime::from_timestamp(now(),0).unwrap().to_rfc3339()})).unwrap())
        })
    }
}
#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(_) => {
            eprintln!("offline harness startup failed");
            std::process::ExitCode::FAILURE
        }
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|s| matches!(s.as_str(), "backup" | "restore"))
    {
        calendar_server::test_runtime::offline_snapshot(&args).await?;
        println!("{}", json!({"synthetic_only":true,"operation":args[0]}));
        return Ok(());
    }
    let (base, system_file) = match args.as_slice() {
        [_, _, _, _] => (&args[..], None),
        [_, _, _, _, flag, file] if flag == "--system-runtime" => (&args[..4], Some(file)),
        _ => return Err("expected harness arguments".into()),
    };
    let [db_flag, path, origin_flag, origin] = base else {
        unreachable!()
    };
    if db_flag != "--database"
        || origin_flag != "--browser-origin"
        || !std::path::Path::new(path).is_absolute()
    {
        return Err("invalid harness arguments".into());
    }
    let uri: axum::http::Uri = origin.parse()?;
    if uri.scheme_str() != Some("https") || !matches!(uri.host(), Some("127.0.0.1" | "localhost")) {
        return Err("harness origin must be HTTPS loopback".into());
    }
    let hostname = uri.host().unwrap().to_owned();
    let config = AdminConfig {
        team: "synthetic-team".into(),
        audience: "synthetic-admin".into(),
        origin: origin.clone(),
        hostname: hostname.clone(),
        action: "admin-login".into(),
        administrators: [("synthetic-admin".into(), "admin@example.test".into())].into(),
        session_seconds: 900,
    };
    config.validate()?;
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../calendar-adapters/tests/support/admin-fixture.json"
    ))?;
    let key = RsaKeyPair::from_pkcs8(&B64.decode(fixture["pkcs8"].as_str().unwrap())?)
        .map_err(|_| "synthetic key invalid")?;
    let claims = json!({"iss":"https://synthetic-team.cloudflareaccess.com","aud":["synthetic-admin"],"type":"app","sub":"synthetic-admin","email":"admin@example.test","iat":now(),"nbf":now(),"exp":now()+3600});
    let input = format!(
        "{}.{}",
        B64.encode(serde_json::to_vec(
            &json!({"alg":"RS256","kid":"synthetic","typ":"JWT"})
        )?),
        B64.encode(serde_json::to_vec(&claims)?)
    );
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        input.as_bytes(),
        &mut signature,
    )
    .map_err(|_| "synthetic signing failed")?;
    let jwt = format!("{input}.{}", B64.encode(signature));
    let verifier = CloudflareVerifier::with_transport(
        config,
        SecretBytes::new(b"synthetic-harness-secret".to_vec()),
        Arc::new(OfflineTransport {
            jwks: serde_json::to_vec(&fixture["jwks"])?,
            hostname,
        }),
    )?;
    let system = system_file
        .map(|file| {
            calendar_server::test_runtime::SystemHarnessConfig::load(
                std::path::Path::new(file),
                std::path::Path::new(path),
            )
        })
        .transpose()?;
    // Legacy/default mode still exclusively creates and rejects every existing file.
    if system.is_none() {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
    }
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let server = if let Some(system) = &system {
        system.initialize(std::path::Path::new(path)).await?
    } else {
        Server::initialize(Config {
            bind: "127.0.0.1:0".parse()?,
            database: path.into(),
            max_request_bytes: 1_048_576,
            max_response_bytes: 1_048_576,
            request_timeout: Duration::from_secs(10),
        })
        .await?
    }
    .with_admin_site_key(Arc::new(verifier), "synthetic-site-key".into())?;
    let server = if let Some(system) = &system {
        system.install(server)?
    } else {
        server
    };
    let listener = server.bind().await?;
    println!(
        "{}",
        json!({"upstream":format!("http://{}",listener.local_addr()?),"browser_origin":origin,"access_jwt":jwt,"challenge_prefix":"synthetic-challenge:","synthetic_only":true})
    );
    server
        .serve(listener, async move {
            #[cfg(unix)]
            tokio::select! {_=terminate.recv()=>{},_=tokio::signal::ctrl_c()=>{}}
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
