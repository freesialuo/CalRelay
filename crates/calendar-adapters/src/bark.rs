//! Bark JSON push. Outcomes describe server acceptance, never device display.
use calendar_application::{
    CredentialProvider, DeliveryEnvelope, DeliveryOutcome, PushOutput, SecurityError,
};
use reqwest::{
    header::{CONTENT_TYPE, RETRY_AFTER},
    Url,
};
use std::{
    fmt,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, SystemTime},
};

const MAX_RESPONSE_BYTES: usize = 16 * 1024;
const MAX_REQUEST_TEXT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid Bark output configuration")]
pub struct BarkConfigurationError;

/// One validated output binding. Create one adapter per configured output; a dispatcher may
/// route envelopes by output_id. Only credential handles, never device keys, enter configuration.
pub struct BarkOutput<P> {
    credentials: Arc<P>,
    output_id: String,
    credential_id: String,
    endpoint: Url,
    timeout: Duration,
    #[cfg(test)]
    test_address: Option<SocketAddr>,
    #[cfg(test)]
    root: Option<reqwest::Certificate>,
}
impl<P> fmt::Debug for BarkOutput<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BarkOutput([REDACTED])")
    }
}
impl<P: CredentialProvider> BarkOutput<P> {
    /// `origin` is an HTTPS origin, optionally with `/push`. Paths containing a key,
    /// URL credentials, queries and fragments are rejected. Private destinations fail closed.
    pub fn new(
        credentials: Arc<P>,
        output_id: &str,
        credential_id: &str,
        origin: &str,
        timeout: Duration,
    ) -> Result<Self, BarkConfigurationError> {
        if origin.len() > 8192
            || output_id.is_empty()
            || credential_id.is_empty()
            || timeout.is_zero()
            || timeout > Duration::from_secs(120)
        {
            return Err(BarkConfigurationError);
        }
        let mut endpoint = Url::parse(origin).map_err(|_| BarkConfigurationError)?;
        if endpoint.scheme() != "https"
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !matches!(endpoint.path(), "/" | "/push")
        {
            return Err(BarkConfigurationError);
        }
        endpoint.set_path("/push");
        Ok(Self {
            credentials,
            output_id: output_id.into(),
            credential_id: credential_id.into(),
            endpoint,
            timeout,
            #[cfg(test)]
            test_address: None,
            #[cfg(test)]
            root: None,
        })
    }
    async fn addresses(&self) -> Result<Vec<SocketAddr>, DeliveryOutcome> {
        #[cfg(test)]
        if let Some(address) = self.test_address {
            return Ok(vec![address]);
        }
        let host = self
            .endpoint
            .host_str()
            .ok_or(DeliveryOutcome::PermanentFailure)?;
        let port = self
            .endpoint
            .port_or_known_default()
            .ok_or(DeliveryOutcome::PermanentFailure)?;
        let addresses: Vec<_> = match host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
        {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_) => tokio::net::lookup_host((host, port))
                .await
                .map_err(|_| DeliveryOutcome::Retryable {
                    retry_after_ms: None,
                })?
                .take(65)
                .collect(),
        };
        if addresses.is_empty() {
            return Err(DeliveryOutcome::Retryable {
                retry_after_ms: None,
            });
        }
        if addresses.len() > 64
            || addresses
                .iter()
                .any(|a| !crate::remote::public_address(a.ip()))
        {
            return Err(DeliveryOutcome::PermanentFailure);
        }
        Ok(addresses)
    }
    async fn deliver_inner(
        &self,
        envelope: DeliveryEnvelope,
        observed_retry_after: &mut Option<i64>,
    ) -> DeliveryOutcome {
        if envelope.output_id != self.output_id {
            return DeliveryOutcome::PermanentFailure;
        }
        let addresses = match self.addresses().await {
            Ok(a) => a,
            Err(e) => return e,
        };
        let secret = match self
            .credentials
            .credential(&self.credential_id, "bark")
            .await
        {
            Ok(s) => s,
            Err(
                SecurityError::Denied | SecurityError::InvalidInput | SecurityError::Authentication,
            ) => return DeliveryOutcome::PermanentFailure,
            Err(_) => {
                return DeliveryOutcome::Retryable {
                    retry_after_ms: None,
                }
            }
        };
        let key = match std::str::from_utf8(secret.expose()) {
            Ok(k)
                if !k.is_empty() && k.len() <= 4096 && k.bytes().all(|b| b.is_ascii_graphic()) =>
            {
                k
            }
            _ => return DeliveryOutcome::PermanentFailure,
        };
        let title = &envelope.event.title;
        let body = envelope.event.description.as_deref().unwrap_or(title);
        if title.len().saturating_add(body.len()) > MAX_REQUEST_TEXT_BYTES {
            return DeliveryOutcome::PermanentFailure;
        }
        #[derive(serde::Serialize)]
        struct Payload<'a> {
            device_key: &'a str,
            title: &'a str,
            body: &'a str,
        }
        // No diagnostic path includes the key, request body, response body or transport error.
        let bytes = match serde_json::to_vec(&Payload {
            device_key: key,
            title,
            body,
        }) {
            Ok(bytes) => bytes,
            Err(_) => return DeliveryOutcome::PermanentFailure,
        };
        let builder = reqwest::Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .referer(false)
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .timeout(self.timeout)
            .pool_max_idle_per_host(0)
            .resolve_to_addrs(
                self.endpoint.host_str().expect("validated host"),
                &addresses,
            );
        #[cfg(test)]
        let builder = if let Some(root) = &self.root {
            builder
                .tls_built_in_root_certs(false)
                .add_root_certificate(root.clone())
        } else {
            builder
        };
        let client = match builder.build() {
            Ok(c) => c,
            Err(_) => {
                return DeliveryOutcome::Retryable {
                    retry_after_ms: None,
                }
            }
        };
        let mut response = match client
            .post(self.endpoint.clone())
            .header(CONTENT_TYPE, "application/json; charset=utf-8")
            .header("Accept-Encoding", "identity")
            .body(bytes)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) if e.is_connect() && !e.is_timeout() => {
                return DeliveryOutcome::Retryable {
                    retry_after_ms: None,
                }
            }
            Err(_) => {
                return DeliveryOutcome::Unknown {
                    retry_after_ms: None,
                }
            }
        };
        if !response
            .remote_addr()
            .is_some_and(|peer| addresses.contains(&peer))
        {
            return DeliveryOutcome::Unknown {
                retry_after_ms: None,
            };
        }
        let status = response.status().as_u16();
        let retry_after_ms = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|h| h.to_str().ok())
            .and_then(|s| retry_after(s, SystemTime::now()));
        *observed_retry_after = retry_after_ms;
        // Error statuses are authoritative even if their body is lost, huge or malformed.
        if !(200..=299).contains(&status) {
            return classify(status, &[], retry_after_ms);
        }
        let unknown = DeliveryOutcome::Unknown { retry_after_ms };
        if response
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
            || response
                .headers()
                .get("content-encoding")
                .is_some_and(|v| v != "identity")
        {
            return unknown;
        }
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if chunk.len() <= MAX_RESPONSE_BYTES - body.len() => {
                    body.extend_from_slice(&chunk)
                }
                Ok(None) => break,
                _ => return unknown,
            }
        }
        classify(status, &body, retry_after_ms)
    }
}
impl<P: CredentialProvider> PushOutput for BarkOutput<P> {
    async fn deliver(&self, envelope: DeliveryEnvelope) -> DeliveryOutcome {
        // Bounds credentials, DNS and I/O together. The application retains its own deadline.
        let mut retry_after_ms = None;
        let result = tokio::time::timeout(
            self.timeout,
            self.deliver_inner(envelope, &mut retry_after_ms),
        )
        .await;
        result.unwrap_or(DeliveryOutcome::Unknown { retry_after_ms })
    }
}
fn classify(status: u16, body: &[u8], retry_after_ms: Option<i64>) -> DeliveryOutcome {
    use DeliveryOutcome::*;
    match status {
        429 | 500..=599 => return Retryable { retry_after_ms },
        200..=299 => {}
        _ => return PermanentFailure,
    }
    #[derive(serde::Deserialize)]
    struct Response {
        code: u16,
    }
    match serde_json::from_slice::<Response>(body).map(|r| r.code) {
        Ok(200) => Accepted,
        Ok(429 | 500..=599) => Retryable { retry_after_ms },
        Ok(400..=499) => PermanentFailure,
        _ => Unknown { retry_after_ms },
    }
}
fn retry_after(value: &str, now: SystemTime) -> Option<i64> {
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        // Saturate large valid delays so overflow never turns a lower bound into immediate retry.
        return Some(
            value
                .parse::<i64>()
                .unwrap_or(i64::MAX)
                .saturating_mul(1000),
        );
    }
    let date = httpdate::parse_http_date(value).ok()?;
    let delta = date.duration_since(now).unwrap_or_default().as_millis();
    Some(i64::try_from(delta).unwrap_or(i64::MAX))
}

#[cfg(test)]
#[path = "bark_tests.rs"]
mod network_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac2_http_and_application_code_both_gate_acceptance() {
        use DeliveryOutcome::*;
        for (status, body, expected) in [
            (200, r#"{"code":200,"message":"success"}"#, Accepted),
            (201, r#"{"code":200}"#, Accepted),
            (400, r#"{"code":200}"#, PermanentFailure),
            (401, r#"{"code":200}"#, PermanentFailure),
            (403, r#"{"code":200}"#, PermanentFailure),
            (200, r#"{"code":400}"#, PermanentFailure),
            (200, r#"{"code":410}"#, PermanentFailure),
            (
                429,
                r#"{"code":200}"#,
                Retryable {
                    retry_after_ms: Some(3000),
                },
            ),
            (
                503,
                "invalid JSON",
                Retryable {
                    retry_after_ms: Some(3000),
                },
            ),
            (
                200,
                r#"{"code":429}"#,
                Retryable {
                    retry_after_ms: Some(3000),
                },
            ),
            (
                200,
                r#"{"code":500}"#,
                Retryable {
                    retry_after_ms: Some(3000),
                },
            ),
            (
                200,
                "",
                Unknown {
                    retry_after_ms: Some(3000),
                },
            ),
            (
                200,
                r#"{"code":"200"}"#,
                Unknown {
                    retry_after_ms: Some(3000),
                },
            ),
            (
                200,
                r#"{"message":"ok"}"#,
                Unknown {
                    retry_after_ms: Some(3000),
                },
            ),
            (
                200,
                r#"{"code":0}"#,
                Unknown {
                    retry_after_ms: Some(3000),
                },
            ),
            (302, r#"{"code":200}"#, PermanentFailure),
        ] {
            assert_eq!(
                classify(status, body.as_bytes(), Some(3000)),
                expected,
                "{status} {body}"
            );
        }
    }
}
