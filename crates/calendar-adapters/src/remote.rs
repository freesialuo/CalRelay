//! HTTPS fetch with per-hop DNS validation and pinned connections. No database writes.
use base64::{engine::general_purpose::STANDARD, Engine};
use calendar_application::{
    CredentialProvider, RemoteAuthentication, RemoteDocument, RemoteFetchError as E,
    RemoteFetchLimits, RemoteFetchOutcome, RemoteFetchRequest, RemoteFetcher, RemoteValidators,
};
use reqwest::{
    header::{
        HeaderMap, HeaderValue, AUTHORIZATION, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH,
        LAST_MODIFIED, LOCATION, RETRY_AFTER,
    },
    Url,
};
use std::{
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};
use zeroize::Zeroizing;

type DnsFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<SocketAddr>, E>> + Send + 'a>>;
trait Resolver: Send + Sync {
    fn lookup(&self, host: String, port: u16) -> DnsFuture<'_>;
}
struct SystemResolver;
impl Resolver for SystemResolver {
    fn lookup(&self, host: String, port: u16) -> DnsFuture<'_> {
        Box::pin(async move {
            let addresses: Vec<_> = tokio::net::lookup_host((host.as_str(), port))
                .await
                .map_err(|_| E::Dns)?
                .take(65)
                .collect();
            if addresses.is_empty() || addresses.len() > 64 {
                return Err(E::Dns);
            }
            Ok(addresses)
        })
    }
}

pub struct HttpFetcher<P> {
    credentials: Arc<P>,
    limits: RemoteFetchLimits,
    resolver: Arc<dyn Resolver>,
    #[cfg(test)]
    root: Option<reqwest::Certificate>,
    #[cfg(test)]
    allow_test_loopback: bool,
}
impl<P: CredentialProvider> HttpFetcher<P> {
    pub fn new(credentials: Arc<P>, limits: RemoteFetchLimits) -> Result<Self, E> {
        if limits.max_bytes == 0
            || limits.max_bytes > 64 * 1024 * 1024
            || limits.max_redirects > 10
            || limits.timeout.is_zero()
            || limits.timeout > Duration::from_secs(120)
        {
            return Err(E::InvalidConfiguration);
        }
        Ok(Self {
            credentials,
            limits,
            resolver: Arc::new(SystemResolver),
            #[cfg(test)]
            root: None,
            #[cfg(test)]
            allow_test_loopback: false,
        })
    }
    fn address_allowed(&self, ip: IpAddr) -> bool {
        #[cfg(test)]
        if self.allow_test_loopback && ip.is_loopback() {
            return true;
        }
        public_address(ip)
    }
    async fn pinned_addresses(&self, url: &Url) -> Result<Vec<SocketAddr>, E> {
        let port = url.port_or_known_default().ok_or(E::InvalidConfiguration)?;
        let host = url.host_str().ok_or(E::InvalidConfiguration)?;
        let literal = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>();
        let addresses = match literal {
            Ok(ip) => vec![SocketAddr::new(ip, port)],
            Err(_) => self.resolver.lookup(host.to_owned(), port).await?,
        };
        if addresses.is_empty()
            || addresses.len() > 64
            || addresses.iter().any(|a| !self.address_allowed(a.ip()))
        {
            return Err(E::AddressDenied);
        }
        Ok(addresses
            .into_iter()
            .map(|a| SocketAddr::new(a.ip(), port))
            .collect())
    }
    async fn authorization(&self, auth: &RemoteAuthentication) -> Result<Option<HeaderValue>, E> {
        let mut value = match auth {
            RemoteAuthentication::None => return Ok(None),
            RemoteAuthentication::Bearer { credential_id } => {
                let secret = self
                    .credentials
                    .credential(credential_id, "bearer")
                    .await
                    .map_err(|_| E::Credential)?;
                if secret.expose().is_empty()
                    || secret.expose().len() > 4096
                    || !secret.expose().iter().all(|b| b.is_ascii_graphic())
                {
                    return Err(E::Credential);
                }
                let mut bytes = Zeroizing::new(b"Bearer ".to_vec());
                bytes.extend_from_slice(secret.expose());
                HeaderValue::from_bytes(&bytes).map_err(|_| E::Credential)?
            }
            RemoteAuthentication::Basic { credential_id } => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Basic {
                    username: Zeroizing<String>,
                    password: Zeroizing<String>,
                }
                let secret = self
                    .credentials
                    .credential(credential_id, "basic")
                    .await
                    .map_err(|_| E::Credential)?;
                if secret.expose().len() > 8192 {
                    return Err(E::Credential);
                }
                let basic: Basic =
                    serde_json::from_slice(secret.expose()).map_err(|_| E::Credential)?;
                if basic.username.contains(':')
                    || basic
                        .username
                        .chars()
                        .chain(basic.password.chars())
                        .any(char::is_control)
                {
                    return Err(E::Credential);
                }
                let mut raw = Zeroizing::new(basic.username.as_bytes().to_vec());
                raw.push(b':');
                raw.extend_from_slice(basic.password.as_bytes());
                let encoded = Zeroizing::new(STANDARD.encode(&raw));
                let mut value = Zeroizing::new(b"Basic ".to_vec());
                value.extend_from_slice(encoded.as_bytes());
                HeaderValue::from_bytes(&value).map_err(|_| E::Credential)?
            }
        };
        value.set_sensitive(true);
        Ok(Some(value))
    }
    async fn fetch_inner(&self, request: RemoteFetchRequest) -> Result<RemoteFetchOutcome, E> {
        let mut url = parse_url(&request.url)?;
        // Validate and resolve before exposing credentials to a transport.
        let mut addresses = self.pinned_addresses(&url).await?;
        let auth = self.authorization(&request.authentication).await?;
        let mut conditional = HeaderMap::new();
        if let Some(etag) = request.validators.etag {
            conditional.insert(IF_NONE_MATCH, validator_header(&etag)?);
        }
        if let Some(modified) = request.validators.last_modified {
            conditional.insert(IF_MODIFIED_SINCE, validator_header(&modified)?);
        }
        for hop in 0..=self.limits.max_redirects {
            let builder = reqwest::Client::builder()
                .https_only(true)
                // Application transport intentionally requires a direct connection: a proxy
                // would resolve the hostname again and invalidate this client's SSRF check.
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .referer(false)
                .no_gzip()
                .no_brotli()
                .no_deflate()
                .no_zstd()
                .timeout(self.limits.timeout)
                .pool_max_idle_per_host(0)
                .resolve_to_addrs(url.host_str().ok_or(E::InvalidConfiguration)?, &addresses);
            #[cfg(test)]
            let builder = if let Some(root) = &self.root {
                builder
                    .tls_built_in_root_certs(false)
                    .add_root_certificate(root.clone())
            } else {
                builder
            };
            let client = builder.build().map_err(|_| E::Transport)?;
            let mut builder = client
                .get(url.clone())
                .headers(conditional.clone())
                .header("Accept-Encoding", "identity");
            if let Some(auth) = &auth {
                builder = builder.header(AUTHORIZATION, auth.clone());
            }
            let mut response = builder.send().await.map_err(transport_error)?;
            if !response
                .remote_addr()
                .is_some_and(|peer| addresses.contains(&peer))
            {
                return Err(E::Transport);
            }
            let status = response.status().as_u16();
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                if hop == self.limits.max_redirects {
                    return Err(E::Redirect);
                }
                let location = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .filter(|s| s.len() <= 8192)
                    .ok_or(E::Redirect)?;
                let next = url.join(location).map_err(|_| E::Redirect)?;
                parse_url(next.as_str())?;
                // Deliberately restrict redirects to this origin; no credential/validator
                // forwarding across hosts or ports, even to another public endpoint.
                if next.origin() != url.origin() {
                    return Err(E::Redirect);
                }
                addresses = self.pinned_addresses(&next).await?;
                url = next;
                continue;
            }
            let retry = retry_after(response.headers(), request.now_ms);
            classify_status(status, retry, request.has_committed_snapshot)?;
            let validators = RemoteValidators {
                etag: response_validator(response.headers(), ETAG)?,
                last_modified: response_validator(response.headers(), LAST_MODIFIED)?,
            };
            if status == 304 {
                return Ok(RemoteFetchOutcome::NotModified(validators));
            }
            if response
                .headers()
                .get("content-encoding")
                .is_some_and(|v| v != "identity")
            {
                return Err(E::Protocol);
            }
            if response
                .content_length()
                .is_some_and(|n| n > self.limits.max_bytes as u64)
            {
                return Err(E::TooLarge);
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
                if chunk.len() > self.limits.max_bytes - body.len() {
                    return Err(E::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(RemoteFetchOutcome::Modified(RemoteDocument {
                body,
                validators,
            }));
        }
        Err(E::Redirect)
    }
}
impl<P: CredentialProvider> RemoteFetcher for HttpFetcher<P> {
    async fn fetch(&self, request: RemoteFetchRequest) -> Result<RemoteFetchOutcome, E> {
        tokio::time::timeout(self.limits.timeout, self.fetch_inner(request))
            .await
            .map_err(|_| E::Timeout)?
    }
}
fn parse_url(raw: &str) -> Result<Url, E> {
    if raw.len() > 8192 {
        return Err(E::InvalidConfiguration);
    }
    let url = Url::parse(raw).map_err(|_| E::InvalidConfiguration)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(E::InvalidConfiguration);
    }
    Ok(url)
}
fn validator_header(raw: &str) -> Result<HeaderValue, E> {
    if raw.len() > 4096 {
        return Err(E::InvalidConfiguration);
    }
    HeaderValue::from_str(raw).map_err(|_| E::InvalidConfiguration)
}
fn response_validator(
    headers: &HeaderMap,
    name: reqwest::header::HeaderName,
) -> Result<Option<String>, E> {
    headers
        .get(name)
        .map(|v| {
            v.to_str()
                .ok()
                .filter(|v| v.len() <= 4096)
                .map(str::to_owned)
                .ok_or(E::Protocol)
        })
        .transpose()
}
fn transport_error(error: reqwest::Error) -> E {
    if error.is_timeout() {
        E::Timeout
    } else {
        E::Transport
    }
}
fn retry_after(headers: &HeaderMap, now_ms: i64) -> Option<i64> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?;
    if let Ok(seconds) = value.parse::<i64>() {
        return seconds.checked_mul(1000).filter(|v| *v >= 0);
    }
    let date = httpdate::parse_http_date(value)
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis();
    i64::try_from(date)
        .ok()?
        .checked_sub(now_ms)
        .map(|n| n.max(0))
}
fn classify_status(status: u16, retry_after_ms: Option<i64>, snapshot: bool) -> Result<(), E> {
    match status {
        200 => Ok(()),
        304 if snapshot => Ok(()),
        304 => Err(E::UnexpectedNotModified),
        401 | 403 => Err(E::Unauthorized),
        429 => Err(E::RateLimited { retry_after_ms }),
        500..=599 => Err(E::Server {
            status,
            retry_after_ms,
        }),
        _ => Err(E::Status { status }),
    }
}
fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let n = u32::from(ip);
            ![
                (0x00000000, 8),
                (0x0a000000, 8),
                (0x64400000, 10),
                (0x7f000000, 8),
                (0xa9fe0000, 16),
                (0xac100000, 12),
                (0xc0000000, 24),
                (0xc0000200, 24),
                (0xc0586300, 24),
                (0xc0a80000, 16),
                (0xc6120000, 15),
                (0xc6336400, 24),
                (0xcb007100, 24),
                (0xe0000000, 4),
                (0xf0000000, 4),
            ]
            .iter()
            .any(|(network, prefix)| n >> (32 - prefix) == network >> (32 - prefix))
        }
        IpAddr::V6(ip) => {
            let n = u128::from(ip);
            // Only native global unicast. Exclude IETF special assignments, documentation
            // and transition mechanisms (mapped IPv4, NAT64, Teredo and 6to4).
            n >> 125 == 1
                && n >> 105
                    != u128::from("2001::".parse::<std::net::Ipv6Addr>().expect("constant")) >> 105
                && n >> 96
                    != u128::from(
                        "2001:db8::"
                            .parse::<std::net::Ipv6Addr>()
                            .expect("constant"),
                    ) >> 96
                && n >> 112 != 0x2002
                && n >> 108 != 0x3fff0
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use calendar_application::RemoteFetchError as E;
    #[test]
    fn ac2_public_addresses_only_and_transition_formats_are_denied() {
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(public_address(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.168.1.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:8.8.8.8",
            "64:ff9b::a00:1",
            "2001:db8::1",
            "2002:0808:0808::1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
        ] {
            assert!(!public_address(ip.parse().unwrap()), "{ip}");
        }
    }
    #[test]
    fn ac3_status_and_first_304_are_typed() {
        assert_eq!(classify_status(200, None, false), Ok(()));
        assert_eq!(
            classify_status(304, None, false),
            Err(E::UnexpectedNotModified)
        );
        assert_eq!(classify_status(304, None, true), Ok(()));
        assert_eq!(classify_status(401, None, false), Err(E::Unauthorized));
        assert_eq!(
            classify_status(429, Some(2000), false),
            Err(E::RateLimited {
                retry_after_ms: Some(2000)
            })
        );
        assert_eq!(
            classify_status(503, Some(3000), false),
            Err(E::Server {
                status: 503,
                retry_after_ms: Some(3000)
            })
        );
        assert_eq!(
            classify_status(404, None, false),
            Err(E::Status { status: 404 })
        );
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;
    use calendar_application::{SecretBytes, SecurityError};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::mpsc,
    };
    use tokio_rustls::{
        rustls::{self, pki_types::PrivatePkcs8KeyDer},
        TlsAcceptor,
    };

    struct TestCredentials;
    impl CredentialProvider for TestCredentials {
        async fn credential(&self, id: &str, kind: &str) -> Result<SecretBytes, SecurityError> {
            match (id, kind) {
                ("b", "bearer") => Ok(SecretBytes::new(b"synthetic-bearer".to_vec())),
                ("u", "basic") => Ok(SecretBytes::new(
                    br#"{"username":"user","password":"p\"ass"}"#.to_vec(),
                )),
                _ => Err(SecurityError::Denied),
            }
        }
    }
    struct ScriptedDns {
        answers: Vec<Vec<IpAddr>>,
        calls: AtomicUsize,
    }
    impl Resolver for ScriptedDns {
        fn lookup(&self, _host: String, port: u16) -> DnsFuture<'_> {
            Box::pin(async move {
                let index = self.calls.fetch_add(1, Ordering::SeqCst);
                let ips = self
                    .answers
                    .get(index)
                    .or_else(|| self.answers.last())
                    .ok_or(E::Dns)?;
                Ok(ips.iter().map(|ip| SocketAddr::new(*ip, port)).collect())
            })
        }
    }
    fn request(url: String) -> RemoteFetchRequest {
        RemoteFetchRequest {
            url,
            authentication: RemoteAuthentication::None,
            validators: RemoteValidators::default(),
            has_committed_snapshot: false,
            now_ms: 0,
        }
    }
    fn response(status: u16, headers: &str, body: &str) -> Vec<u8> {
        format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",body.len()).into_bytes()
    }
    struct Service {
        url: String,
        root: reqwest::Certificate,
        received: mpsc::Receiver<String>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Service {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn service(replies: Vec<(Vec<u8>, bool)>) -> Service {
        let signed = rcgen::generate_simple_self_signed(vec!["calendar.example".into()]).unwrap();
        let root = reqwest::Certificate::from_der(signed.cert.der()).unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![signed.cert.der().clone()],
            PrivatePkcs8KeyDer::from(signed.signing_key.serialize_der()).into(),
        )
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "https://calendar.example:{}/calendar",
            listener.local_addr().unwrap().port()
        );
        let (tx, received) = mpsc::channel(16);
        let task = tokio::spawn(async move {
            for (reply, stall) in replies {
                let (socket, _) = listener.accept().await.unwrap();
                let Ok(mut tls) = acceptor.accept(socket).await else {
                    continue;
                };
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") && bytes.len() < 16384 {
                    let mut byte = [0];
                    if tls.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    bytes.extend(byte);
                }
                let _ = tx.send(String::from_utf8(bytes).unwrap()).await;
                if !reply.is_empty() {
                    let _ = tls.write_all(&reply).await;
                    let _ = tls.flush().await;
                }
                if stall {
                    std::future::pending::<()>().await;
                }
                let _ = tls.shutdown().await;
            }
        });
        Service {
            url,
            root,
            received,
            task,
        }
    }
    fn fetcher(
        service: &Service,
        answers: Vec<Vec<IpAddr>>,
        max_bytes: usize,
    ) -> (HttpFetcher<TestCredentials>, Arc<ScriptedDns>) {
        let mut client = HttpFetcher::new(
            Arc::new(TestCredentials),
            RemoteFetchLimits {
                max_bytes,
                timeout: Duration::from_secs(3),
                max_redirects: 2,
            },
        )
        .unwrap();
        let resolver = Arc::new(ScriptedDns {
            answers,
            calls: AtomicUsize::new(0),
        });
        client.resolver = resolver.clone();
        client.root = Some(service.root.clone());
        client.allow_test_loopback = true;
        (client, resolver)
    }
    fn local() -> Vec<Vec<IpAddr>> {
        vec![vec!["127.0.0.1".parse().unwrap()]]
    }
    #[tokio::test]
    async fn ac1_https_basic_bearer_none_conditional_and_bounded_body() {
        let mut server = service(vec![
            (
                response(
                    200,
                    "ETag: \"rev1\"\r\nLast-Modified: Wed, 21 Oct 2026 00:00:00 GMT\r\n",
                    "BEGIN:VCALENDAR"
                ),
                false
            );
            3
        ])
        .await;
        let (client, dns) = fetcher(&server, local(), 64);
        for (auth, expected) in [
            (RemoteAuthentication::None, None),
            (
                RemoteAuthentication::Basic {
                    credential_id: "u".into(),
                },
                Some(format!(
                    "authorization: basic {}",
                    STANDARD.encode(b"user:p\"ass").to_lowercase()
                )),
            ),
            (
                RemoteAuthentication::Bearer {
                    credential_id: "b".into(),
                },
                Some("authorization: bearer synthetic-bearer".into()),
            ),
        ] {
            let mut req = request(server.url.clone());
            req.authentication = auth;
            req.validators.etag = Some("\"old\"".into());
            req.validators.last_modified = Some("Wed, 14 Oct 2026 00:00:00 GMT".into());
            let outcome = client.fetch(req).await.unwrap();
            let RemoteFetchOutcome::Modified(doc) = outcome else {
                panic!("expected modified")
            };
            assert_eq!(doc.body, b"BEGIN:VCALENDAR");
            assert_eq!(doc.validators.etag.as_deref(), Some("\"rev1\""));
            let wire = server.received.recv().await.unwrap().to_lowercase();
            assert!(wire.starts_with("get /calendar http/1.1"));
            assert!(wire.contains("if-none-match: \"old\""));
            assert!(wire.contains("if-modified-since:"));
            assert!(!wire.lines().next().unwrap().contains("synthetic"));
            match expected {
                Some(expected) => assert!(wire.contains(&expected)),
                None => assert!(!wire.contains("authorization:")),
            }
        }
        assert_eq!(dns.calls.load(Ordering::SeqCst), 3);
    }
    #[tokio::test]
    async fn ac1_chunked_and_declared_size_limits_and_truncation() {
        for (reply,expected) in [
            (response(200,"","12345"),E::TooLarge),
            (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\n123\r\n2\r\n45\r\n0\r\n\r\n".to_vec(),E::TooLarge),
            (b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\n12".to_vec(),E::Transport),
            (response(200,"Content-Encoding: gzip\r\n","123"),E::Protocol),
        ] {
            let server=service(vec![(reply,false)]).await;let(client,_)=fetcher(&server,local(),4);
            assert_eq!(client.fetch(request(server.url.clone())).await.err(),Some(expected));
        }
    }
    #[tokio::test]
    async fn ac3_http_status_304_retry_after_and_redaction() {
        for (status, headers, snapshot, expected) in [
            (304, "", false, Some(E::UnexpectedNotModified)),
            (304, "ETag: \"new\"\r\n", true, None),
            (401, "", false, Some(E::Unauthorized)),
            (
                429,
                "Retry-After: 2\r\n",
                false,
                Some(E::RateLimited {
                    retry_after_ms: Some(2000),
                }),
            ),
            (
                503,
                "Retry-After: Thu, 01 Jan 1970 00:00:03 GMT\r\n",
                false,
                Some(E::Server {
                    status: 503,
                    retry_after_ms: Some(3000),
                }),
            ),
            (404, "", false, Some(E::Status { status: 404 })),
            (206, "", false, Some(E::Status { status: 206 })),
        ] {
            let server = service(vec![(response(status, headers, ""), false)]).await;
            let (client, _) = fetcher(&server, local(), 64);
            let mut req = request(server.url.clone() + "?secret=do-not-log");
            req.has_committed_snapshot = snapshot;
            let result = client.fetch(req).await;
            if let Some(error) = expected {
                assert_eq!(result.err(), Some(error));
                assert!(!format!("{error:?} {error}").contains("do-not-log"));
            } else {
                assert!(matches!(result, Ok(RemoteFetchOutcome::NotModified(_))));
            }
        }
    }
    #[tokio::test]
    async fn ac2_redirect_dns_rebinding_and_mixed_answers_are_rejected() {
        let server = service(vec![(response(302, "Location: /next\r\n", ""), false)]).await;
        let (client, dns) = fetcher(
            &server,
            vec![
                vec!["127.0.0.1".parse().unwrap()],
                vec!["10.0.0.1".parse().unwrap()],
            ],
            64,
        );
        assert_eq!(
            client.fetch(request(server.url.clone())).await.err(),
            Some(E::AddressDenied)
        );
        assert_eq!(dns.calls.load(Ordering::SeqCst), 2);
        let (client, _) = fetcher(
            &server,
            vec![vec![
                "8.8.8.8".parse().unwrap(),
                "10.0.0.1".parse().unwrap(),
            ]],
            64,
        );
        assert_eq!(
            client.fetch(request(server.url.clone())).await.err(),
            Some(E::AddressDenied)
        );
    }
    #[tokio::test]
    async fn ac2_connection_uses_checked_dns_once_and_checks_every_redirect() {
        let server = service(vec![
            (response(302, "Location: /next\r\n", ""), false),
            (response(200, "", "ok"), false),
        ])
        .await;
        let (client, dns) = fetcher(&server, local(), 64);
        assert!(matches!(
            client.fetch(request(server.url.clone())).await,
            Ok(RemoteFetchOutcome::Modified(_))
        ));
        assert_eq!(dns.calls.load(Ordering::SeqCst), 2);
        // A second resolver lookup within the first connection would rebind to private IP.
        let server = service(vec![(response(200, "", "ok"), false)]).await;
        let (client, dns) = fetcher(
            &server,
            vec![
                vec!["127.0.0.1".parse().unwrap()],
                vec!["10.0.0.1".parse().unwrap()],
            ],
            64,
        );
        assert!(client.fetch(request(server.url.clone())).await.is_ok());
        assert_eq!(dns.calls.load(Ordering::SeqCst), 1);
        for location in [
            "http://calendar.example/plain",
            "https://127.0.0.1/private",
            "https://other.example/leak",
        ] {
            let server = service(vec![(
                response(302, &format!("Location: {location}\r\n"), ""),
                false,
            )])
            .await;
            let (client, _) = fetcher(&server, local(), 64);
            assert!(client.fetch(request(server.url.clone())).await.is_err());
        }
    }
    #[tokio::test]
    async fn ac1_total_timeout_covers_stalled_body_and_dns() {
        let server = service(vec![(
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n".to_vec(),
            true,
        )])
        .await;
        let (mut client, _) = fetcher(&server, local(), 64);
        client.limits.timeout = Duration::from_millis(100);
        assert_eq!(
            client.fetch(request(server.url.clone())).await.err(),
            Some(E::Timeout)
        );
        struct StalledDns;
        impl Resolver for StalledDns {
            fn lookup(&self, _host: String, _port: u16) -> DnsFuture<'_> {
                Box::pin(std::future::pending())
            }
        }
        client.resolver = Arc::new(StalledDns);
        assert_eq!(
            client.fetch(request(server.url.clone())).await.err(),
            Some(E::Timeout)
        );
    }
    #[tokio::test]
    async fn ac2_production_policy_rejects_local_literals_and_malformed_config() {
        let client =
            HttpFetcher::new(Arc::new(TestCredentials), RemoteFetchLimits::default()).unwrap();
        for url in [
            "https://127.0.0.1/",
            "https://2130706433/",
            "https://0x7f000001/",
            "https://[::ffff:127.0.0.1]/",
            "https://169.254.169.254/",
        ] {
            assert_eq!(
                client.fetch(request(url.into())).await.err(),
                Some(E::AddressDenied)
            );
        }
        for url in [
            "http://public.example/",
            "https://user:pass@public.example/",
            "https://public.example/#fragment",
            "file:///data/db",
        ] {
            assert_eq!(
                client.fetch(request(url.into())).await.err(),
                Some(E::InvalidConfiguration)
            );
        }
    }
    #[tokio::test]
    async fn ac2_tls_requires_trusted_certificate_and_matching_hostname() {
        for trusted in [false, true] {
            let server = service(vec![(response(200, "", "ok"), false)]).await;
            let (mut client, _) = fetcher(&server, local(), 64);
            let mut url = server.url.clone();
            if trusted {
                url = url.replace("calendar.example", "other.example");
            } else {
                client.root = None;
            }
            assert_eq!(client.fetch(request(url)).await.err(), Some(E::Transport));
        }
    }
    #[tokio::test]
    async fn ac2_redirect_budget_is_finite() {
        let server = service(vec![(response(302, "Location: /again\r\n", ""), false); 3]).await;
        let (client, dns) = fetcher(&server, local(), 64);
        assert_eq!(
            client.fetch(request(server.url.clone())).await.err(),
            Some(E::Redirect)
        );
        assert_eq!(dns.calls.load(Ordering::SeqCst), 3);
    }
}

#[cfg(test)]
mod tls_policy_tests {
    use super::*;
    #[test]
    fn retry_after_overflow_and_malformed_headers_are_rejected() {
        let mut headers = HeaderMap::new();
        for raw in ["-1", "9223372036854775807", "not-a-date"] {
            headers.insert(RETRY_AFTER, HeaderValue::from_str(raw).unwrap());
            assert_eq!(retry_after(&headers, 0), None);
        }
        assert!(validator_header("valid\r\nAuthorization: leaked").is_err());
        assert!(validator_header(&"x".repeat(4097)).is_err());
    }
}
