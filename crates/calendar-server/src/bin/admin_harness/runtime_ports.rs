use calendar_application::configuration::ReminderConfigurationRepository;
// Network substitutes for the feature-isolated browser system harness. No workers live here.
use calendar_application::{
    CredentialProvider, DeliveryEnvelope, DeliveryOutcome, PushOutput, RemoteAuthentication,
    RemoteDocument, RemoteFetchError as E, RemoteFetchOutcome, RemoteFetchRequest, RemoteFetcher,
    RemoteValidators, SyncLimits,
};
use calendar_server::{
    credentials_bootstrap::RuntimeCredentialProvider, source_worker::IcsSourceRunner, Server,
    Services,
};
use serde::Deserialize;
use std::{path::Path, sync::Arc, time::Duration};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Targets {
    a: String,
    b: String,
    bark: String,
}
fn target(value: &str, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let u = reqwest::Url::parse(value)?;
    if u.scheme() != "http"
        || u.host_str() != Some("127.0.0.1")
        || u.port().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
        || u.path() != path
    {
        return Err("invalid offline fixture target".into());
    }
    Ok(())
}
fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .build()
}
struct Fetcher {
    targets: Targets,
    client: reqwest::Client,
}
impl RemoteFetcher for Fetcher {
    async fn fetch(&self, r: RemoteFetchRequest) -> Result<RemoteFetchOutcome, E> {
        if !matches!(r.authentication, RemoteAuthentication::None) {
            return Err(E::Credential);
        }
        let url = match r.url.as_str() {
            "https://ics-a.system.invalid/calendar.ics" => &self.targets.a,
            "https://ics-b.system.invalid/calendar.ics" => &self.targets.b,
            _ => return Err(E::AddressDenied),
        };
        let mut get = self.client.get(url);
        if let Some(tag) = r.validators.etag {
            get = get.header("if-none-match", tag);
        }
        let response = get.send().await.map_err(|_| E::Transport)?;
        let validators = RemoteValidators {
            etag: response
                .headers()
                .get("etag")
                .and_then(|h| h.to_str().ok())
                .map(str::to_owned),
            last_modified: None,
        };
        if response.status().as_u16() == 304 {
            if !r.has_committed_snapshot {
                return Err(E::UnexpectedNotModified);
            }
            return Ok(RemoteFetchOutcome::NotModified(validators));
        }
        if response.status().as_u16() != 200 {
            return Err(E::Server {
                status: response.status().as_u16(),
                retry_after_ms: Some(1000),
            });
        }
        let mut response = response;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| E::Transport)? {
            if body.len() + chunk.len() > 65536 {
                return Err(E::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(RemoteFetchOutcome::Modified(RemoteDocument {
            credential_revision: None,
            final_url: r.url,
            body,
            validators,
        }))
    }
}
struct Output {
    services: Arc<Services>,
    credentials: RuntimeCredentialProvider,
    target: String,
    client: reqwest::Client,
}
impl PushOutput for Output {
    async fn deliver(&self, e: DeliveryEnvelope) -> DeliveryOutcome {
        // Keep the same durable job/output fence and real encrypted provider boundary.
        let config = match self
            .services
            .storage
            .output_for_delivery(&e.job_id, &e.output_id)
            .await
        {
            Ok(Some(r)) => r.configuration,
            Ok(None) => return DeliveryOutcome::PermanentFailure,
            Err(_) => {
                return DeliveryOutcome::Retryable {
                    retry_after_ms: Some(1000),
                }
            }
        };
        if config.origin != "https://bark.system.invalid/push" {
            return DeliveryOutcome::PermanentFailure;
        }
        let secret = match self
            .credentials
            .credential(&config.credential_id, "bark")
            .await
        {
            Ok(v) => v,
            Err(_) => return DeliveryOutcome::PermanentFailure,
        };
        let key = match std::str::from_utf8(secret.expose()) {
            Ok(v) if v.starts_with("synthetic_") => v,
            _ => return DeliveryOutcome::PermanentFailure,
        };
        let body = serde_json::json!({"device_key":key,"title":e.event.title,"body":"synthetic system delivery"});
        let response = match self
            .client
            .post(&self.target)
            .timeout(Duration::from_millis(config.request_timeout_ms.min(3000)))
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .await
        {
            Ok(r) => r,
            Err(_) => {
                return DeliveryOutcome::Unknown {
                    retry_after_ms: Some(1000),
                }
            }
        };
        if response.status().as_u16() == 503 {
            return DeliveryOutcome::Retryable {
                retry_after_ms: Some(1000),
            };
        }
        if !response.status().is_success() {
            return DeliveryOutcome::PermanentFailure;
        }
        let mut response = response;
        let mut bytes = Vec::new();
        loop {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(_) => {
                    return DeliveryOutcome::Unknown {
                        retry_after_ms: Some(1000),
                    }
                }
            };
            if bytes.len() + chunk.len() > 16384 {
                return DeliveryOutcome::Unknown {
                    retry_after_ms: Some(1000),
                };
            }
            bytes.extend_from_slice(&chunk);
        }
        if serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .is_some_and(|v| v["code"] == 200)
        {
            DeliveryOutcome::Accepted
        } else {
            DeliveryOutcome::Unknown {
                retry_after_ms: Some(1000),
            }
        }
    }
}
pub fn attach(
    server: Server,
    path: &Path,
    clock: Option<Arc<calendar_server::test_runtime::FileBusinessClock>>,
) -> Result<Server, Box<dyn std::error::Error>> {
    if !path.is_absolute() {
        return Err("invalid offline fixture file".into());
    }
    let document = calendar_storage::security::read_secret_file(path, 4096)?;
    let targets: Targets = serde_json::from_slice(document.expose())?;
    target(&targets.a, "/ics/a")?;
    target(&targets.b, "/ics/b")?;
    target(&targets.bark, "/bark")?;
    let services = server.services.clone();
    let credentials = services.credential_provider()?;
    let output = Output {
        services,
        credentials,
        target: targets.bark.clone(),
        client: client()?,
    };
    let runner = IcsSourceRunner {
        fetcher: Arc::new(Fetcher {
            targets,
            client: client()?,
        }),
        ics_limits: server.services.ics_limits,
        sync_limits: SyncLimits::default(),
    };
    Ok(match clock {
        Some(clock) => server.with_test_runtime_ports_and_clock(runner, output, clock)?,
        None => server.with_test_runtime_ports(runner, output)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_targets_reject_external_and_ambiguous_destinations() {
        assert!(target("http://127.0.0.1:41000/ics/a", "/ics/a").is_ok());
        for url in [
            "http://example.com:41000/ics/a",
            "https://127.0.0.1:41000/ics/a",
            "http://user@127.0.0.1:41000/ics/a",
            "http://127.0.0.1:41000/ics/a?x=1",
            "http://127.0.0.1:41000/other",
        ] {
            assert!(target(url, "/ics/a").is_err());
        }
    }
    #[tokio::test]
    async fn unlisted_logical_source_is_denied_before_network() {
        let fetcher = Fetcher {
            targets: Targets {
                a: "http://127.0.0.1:1/ics/a".into(),
                b: "http://127.0.0.1:1/ics/b".into(),
                bark: "http://127.0.0.1:1/bark".into(),
            },
            client: client().unwrap(),
        };
        let r = fetcher
            .fetch(RemoteFetchRequest {
                url: "https://unlisted.invalid/calendar.ics".into(),
                authentication: RemoteAuthentication::None,
                validators: RemoteValidators::default(),
                cached_credential_revision: None,
                has_committed_snapshot: false,
                now_ms: 0,
            })
            .await;
        assert!(matches!(r, Err(E::AddressDenied)));
    }
}
