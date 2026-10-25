//! Fail-closed Cloudflare Access and Turnstile verification. No identity headers are trusted.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use calendar_application::SecretBytes;
use ring::signature::{RsaPublicKeyComponents, RSA_PKCS1_2048_8192_SHA256};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc, time::Duration};

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("administrator verification denied")]
pub struct AdminError;

#[derive(Clone)]
pub struct AdminConfig {
    /// Team slug, never a URL read from a token or request.
    pub team: String,
    pub audience: String,
    pub origin: String,
    pub hostname: String,
    pub action: String,
    /// Exact Access subject => verified email pairs; an empty allowlist is invalid.
    pub administrators: BTreeMap<String, String>,
    pub session_seconds: i64,
}
impl AdminConfig {
    fn validate(&self) -> Result<(), AdminError> {
        let url = reqwest::Url::parse(&self.origin).map_err(|_| AdminError)?;
        if self.team.is_empty()
            || self.team.len() > 63
            || !self
                .team
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || url.scheme() != "https"
            || url.host_str() != Some(&self.hostname)
            || url.origin().ascii_serialization() != self.origin
            || self.hostname.is_empty()
            || self.audience.is_empty()
            || self.action.is_empty()
            || self.administrators.is_empty()
            || self
                .administrators
                .iter()
                .any(|(s, e)| s.is_empty() || e.is_empty())
            || !(1..=900).contains(&self.session_seconds)
        {
            return Err(AdminError);
        }
        Ok(())
    }
}

/// Trusted transport injection boundary for offline tests. HTTP implementation below is fixed
/// to official endpoints, TLS verification, no redirects, no proxy, time and byte budgets.
pub trait AdminTransport: Send + Sync {
    fn request(
        &self,
        url: String,
        form: Option<Vec<(String, String)>>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, AdminError>> + Send + '_>>;
}
fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(5))
}
struct CloudflareHttp {
    client: reqwest::Client,
}
impl AdminTransport for CloudflareHttp {
    fn request(
        &self,
        url: String,
        form: Option<Vec<(String, String)>>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, AdminError>> + Send + '_>> {
        Box::pin(async move {
            let builder = if let Some(form) = form {
                self.client.post(url).form(&form)
            } else {
                self.client.get(url)
            };
            let mut response = builder.send().await.map_err(|_| AdminError)?;
            if !response.status().is_success() {
                return Err(AdminError);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| AdminError)? {
                if bytes.len() + chunk.len() > 65536 {
                    return Err(AdminError);
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
        })
    }
}

pub struct CloudflareVerifier {
    config: AdminConfig,
    transport: Arc<dyn AdminTransport>,
    secret: SecretBytes,
    policy: String,
}
/// Only produced after complete JWT signature/claims/admin checks.
pub struct AdminIdentity {
    binding: String,
    expires: i64,
}
impl AdminIdentity {
    pub fn binding(&self) -> &str {
        &self.binding
    }
    pub fn expires_at(&self) -> i64 {
        self.expires
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    kid: String,
    typ: Option<String>,
}
#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: Vec<String>,
    exp: i64,
    iat: i64,
    nbf: i64,
    sub: String,
    email: String,
    #[serde(rename = "type")]
    kind: String,
}
#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}
#[derive(Deserialize)]
struct Jwk {
    kid: String,
    kty: String,
    alg: String,
    #[serde(rename = "use")]
    usage: String,
    n: String,
    e: String,
}
#[derive(Deserialize)]
struct Siteverify {
    success: bool,
    hostname: String,
    action: String,
    challenge_ts: String,
}
impl CloudflareVerifier {
    pub fn new(config: AdminConfig, secret: SecretBytes) -> Result<Self, AdminError> {
        let client = client_builder().build().map_err(|_| AdminError)?;
        Self::with_transport(config, secret, Arc::new(CloudflareHttp { client }))
    }
    pub fn with_transport(
        config: AdminConfig,
        secret: SecretBytes,
        transport: Arc<dyn AdminTransport>,
    ) -> Result<Self, AdminError> {
        config.validate()?;
        if secret.expose().is_empty()
            || secret.expose().len() > 4096
            || std::str::from_utf8(secret.expose()).is_err()
        {
            return Err(AdminError);
        }
        let policy = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(
                    &config.team,
                    &config.audience,
                    &config.origin,
                    &config.hostname,
                    &config.action,
                    &config.administrators,
                    config.session_seconds
                ))
                .map_err(|_| AdminError)?
            )
        );
        Ok(Self {
            config,
            secret,
            transport,
            policy,
        })
    }
    pub fn origin(&self) -> &str {
        &self.config.origin
    }
    pub fn policy(&self) -> &str {
        &self.policy
    }
    pub fn session_seconds(&self) -> i64 {
        self.config.session_seconds
    }
    async fn request(
        &self,
        url: String,
        form: Option<Vec<(String, String)>>,
    ) -> Result<Vec<u8>, AdminError> {
        let bytes = tokio::time::timeout(Duration::from_secs(5), self.transport.request(url, form))
            .await
            .map_err(|_| AdminError)??;
        if bytes.len() > 65536 {
            return Err(AdminError);
        }
        Ok(bytes)
    }
    /// Fetch JWKS on every verification: no stale-key fallback when unavailable or rotated.
    pub async fn access(&self, jwt: &str, now: i64) -> Result<AdminIdentity, AdminError> {
        if jwt.len() > 16384 || now < 0 {
            return Err(AdminError);
        }
        let parts: Vec<_> = jwt.split('.').collect();
        if parts.len() != 3 {
            return Err(AdminError);
        }
        let header: Header = serde_json::from_slice(&B64.decode(parts[0]).map_err(|_| AdminError)?)
            .map_err(|_| AdminError)?;
        if header.alg != "RS256"
            || header.kid.is_empty()
            || header.typ.as_deref().is_some_and(|s| s != "JWT")
        {
            return Err(AdminError);
        }
        let issuer = format!("https://{}.cloudflareaccess.com", self.config.team);
        let bytes = self
            .request(format!("{issuer}/cdn-cgi/access/certs"), None)
            .await?;
        let jwks: Jwks = serde_json::from_slice(&bytes).map_err(|_| AdminError)?;
        let keys: Vec<_> = jwks.keys.iter().filter(|k| k.kid == header.kid).collect();
        if keys.len() != 1 {
            return Err(AdminError);
        }
        let key = keys[0];
        if key.kty != "RSA" || key.alg != "RS256" || key.usage != "sig" {
            return Err(AdminError);
        }
        let public = RsaPublicKeyComponents {
            n: B64.decode(&key.n).map_err(|_| AdminError)?,
            e: B64.decode(&key.e).map_err(|_| AdminError)?,
        };
        public
            .verify(
                &RSA_PKCS1_2048_8192_SHA256,
                format!("{}.{}", parts[0], parts[1]).as_bytes(),
                &B64.decode(parts[2]).map_err(|_| AdminError)?,
            )
            .map_err(|_| AdminError)?;
        let c: Claims = serde_json::from_slice(&B64.decode(parts[1]).map_err(|_| AdminError)?)
            .map_err(|_| AdminError)?;
        if c.iss != issuer
            || !c.aud.contains(&self.config.audience)
            || c.kind != "app"
            || c.sub.is_empty()
            || self.config.administrators.get(&c.sub) != Some(&c.email)
            || c.iat < 0
            || c.nbf < 0
            || c.iat > now
            || c.nbf > now
            || c.exp <= now
            || c.exp <= c.iat
            || c.exp <= c.nbf
        {
            return Err(AdminError);
        }
        Ok(AdminIdentity {
            binding: serde_json::to_string(&(c.iss, c.sub, c.email)).map_err(|_| AdminError)?,
            expires: c.exp,
        })
    }
    pub async fn turnstile(&self, token: &str, now: i64) -> Result<(), AdminError> {
        if token.is_empty() || token.len() > 2048 {
            return Err(AdminError);
        }
        let form = vec![
            (
                "secret".into(),
                std::str::from_utf8(self.secret.expose())
                    .map_err(|_| AdminError)?
                    .to_owned(),
            ),
            ("response".into(), token.into()),
        ];
        let bytes = self
            .request(
                "https://challenges.cloudflare.com/turnstile/v0/siteverify".into(),
                Some(form),
            )
            .await?;
        let result: Siteverify = serde_json::from_slice(&bytes).map_err(|_| AdminError)?;
        let time = chrono::DateTime::parse_from_rfc3339(&result.challenge_ts)
            .map_err(|_| AdminError)?
            .timestamp();
        if !result.success
            || result.hostname != self.config.hostname
            || result.action != self.config.action
            || time > now
            || now.checked_sub(time).is_none_or(|age| age >= 300)
        {
            return Err(AdminError);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    #[tokio::test]
    async fn http_transport_rejects_plaintext_redirect_failure_and_oversize_on_local_simulator() {
        let secure = CloudflareHttp {
            client: client_builder().build().unwrap(),
        };
        assert!(secure
            .request("http://127.0.0.1:1/".into(), None)
            .await
            .is_err());
        for (status, body) in [
            ("302 Found", String::new()),
            ("503 Service Unavailable", String::new()),
            ("200 OK", "x".repeat(65537)),
            ("200 OK", "{}".into()),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let expected = status == "200 OK" && body.len() <= 65536;
            let task = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"secret=synthetic&response=synthetic") {
                    let mut chunk = [0; 4096];
                    let n = stream.read(&mut chunk).await.unwrap();
                    assert!(n > 0 && request.len() + n <= 8192);
                    request.extend_from_slice(&chunk[..n]);
                }
                let request = std::str::from_utf8(&request).unwrap();
                assert!(request.starts_with("POST /siteverify"));
                assert!(request.contains("secret=synthetic&response=synthetic"));
                let response=format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nLocation: http://127.0.0.1:1/should-not-follow\r\nConnection: close\r\n\r\n{body}",body.len());
                let _ = stream.write_all(response.as_bytes()).await;
            });
            // Only this private local test overrides HTTPS; every other client option is shared.
            let http = CloudflareHttp {
                client: client_builder().https_only(false).build().unwrap(),
            };
            assert_eq!(
                http.request(
                    format!("http://{address}/siteverify"),
                    Some(vec![
                        ("secret".into(), "synthetic".into()),
                        ("response".into(), "synthetic".into())
                    ])
                )
                .await
                .is_ok(),
                expected
            );
            task.await.unwrap();
        }
    }
}
