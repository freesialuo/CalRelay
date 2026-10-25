//! Explicit isolated system-harness support. Never selected by the production executable.
use crate::{
    credentials_bootstrap::RuntimeCredentialProvider, production_runtime::supervision_budgets,
    source_worker::IcsSourceRunner, ConfigError, DeploymentConfig, Server,
};
use calendar_application::{
    configuration::ReminderConfigurationRepository, Clock, CredentialProvider, DeliveryEnvelope,
    DeliveryOutcome, PushOutput, RemoteAuthentication, RemoteDocument, RemoteFetchError,
    RemoteFetchOutcome, RemoteFetchRequest, RemoteFetcher, RemoteValidators, SyncLimits,
};
use calendar_storage::security::read_secret_file;
use serde::Deserialize;
use std::{
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const ERROR: ConfigError = ConfigError("invalid isolated system harness configuration");
const SOURCE_A: &str = "https://ics-a.system.invalid/calendar.ics";
const SOURCE_B: &str = "https://ics-b.system.invalid/calendar.ics";

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum DatabaseMode {
    #[default]
    Fresh,
    Reopen,
    Restored,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemHarnessConfig {
    version: u8,
    root: PathBuf,
    deployment_config: PathBuf,
    #[serde(default)]
    mode: DatabaseMode,
    clock_file: PathBuf,
    targets: Targets,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Targets {
    a: String,
    b: String,
    bark: String,
}
impl SystemHarnessConfig {
    pub fn load(path: &Path, database: &Path) -> Result<Self, ConfigError> {
        let bytes = read_secret_file(path, 16_384).map_err(|_| ERROR)?;
        let config: Self = serde_json::from_slice(bytes.expose()).map_err(|_| ERROR)?;
        if config.version != 1 {
            return Err(ERROR);
        }
        validate_root(&config.root)?;
        for file in [path, &config.deployment_config, &config.clock_file] {
            controlled_path(&config.root, file)?;
            read_secret_file(file, 65_536).map_err(|_| ERROR)?;
        }
        controlled_path(&config.root, database)?;
        for target in [&config.targets.a, &config.targets.b, &config.targets.bark] {
            loopback_target(target)?;
        }
        FileClock::load(&config.root, &config.clock_file)?;
        Ok(config)
    }
    fn prepare_database(&self, path: &Path) -> Result<(), ConfigError> {
        controlled_path(&self.root, path)?;
        match self.mode {
            DatabaseMode::Fresh => {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(|_| ERROR)?;
            }
            DatabaseMode::Reopen | DatabaseMode::Restored => {
                let metadata = std::fs::symlink_metadata(path).map_err(|_| ERROR)?;
                if !metadata.is_file() || metadata.len() == 0 {
                    return Err(ERROR);
                }
            }
        }
        Ok(())
    }
    pub async fn initialize(&self, path: &Path) -> Result<Server, Box<dyn std::error::Error>> {
        let bytes = read_secret_file(&self.deployment_config, 65_536)?;
        let config = DeploymentConfig::from_deployment(bytes.expose(), |key| {
            (key == "CALRELAY_DATABASE").then(|| path.to_string_lossy().into_owned())
        })?;
        if !config.server.bind.ip().is_loopback()
            || config.server.bind.port() != 0
            || config.admin.is_some()
        {
            return Err(ERROR.into());
        }
        let keys = config.credentials.as_ref().ok_or(ERROR)?;
        for key in keys.key_files.values() {
            controlled_path(&self.root, key)?;
        }
        // Load actual keys before touching the database. No default key or provider substitution.
        let keys = keys.load()?;
        self.prepare_database(path)?;
        let server = Server::initialize_with_supervision(config.server, supervision_budgets())
            .await?
            .with_credential_keys(keys)?;
        if self.mode == DatabaseMode::Restored {
            let control: (bool, Option<i64>) =
                sqlx::query_as("SELECT paused,cutoff FROM recovery_control WHERE id=1")
                    .fetch_one(server.services.storage.pool())
                    .await?;
            if !control.0 || control.1.is_none() {
                server.services.drain().await;
                return Err(ERROR.into());
            }
        }
        Ok(server)
    }
    pub fn install(&self, server: Server) -> Result<Server, Box<dyn std::error::Error>> {
        let clock = Arc::new(FileClock::load(&self.root, &self.clock_file)?);
        let fetcher = Arc::new(LoopbackFetcher {
            targets: self.targets.clone(),
        });
        let runner = IcsSourceRunner {
            fetcher,
            ics_limits: server.services.ics_limits,
            sync_limits: SyncLimits::default(),
        };
        let output = LoopbackBark {
            services: server.services.clone(),
            provider: server.services.credential_provider()?,
            target: self.targets.bark.clone(),
        };
        Ok(server.with_test_runtime_ports_and_clock(runner, output, clock)?)
    }
}

/// The explicit system mode only accepts private, caller-owned fixture roots. Default legacy
/// harness invocation never reaches this code and continues exclusive create_new behavior.
fn validate_root(root: &Path) -> Result<(), ConfigError> {
    let metadata = std::fs::symlink_metadata(root).map_err(|_| ERROR)?;
    if !root.is_absolute()
        || !metadata.is_dir()
        || std::fs::canonicalize(root).map_err(|_| ERROR)? != root
        || !root
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("calrelay-system-"))
    {
        return Err(ERROR);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no arguments and no caller preconditions.
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(ERROR);
        }
    }
    #[cfg(not(unix))]
    return Err(ERROR);
    Ok(())
}
fn controlled_path(root: &Path, path: &Path) -> Result<(), ConfigError> {
    validate_root(root)?;
    let relative = path.strip_prefix(root).map_err(|_| ERROR)?;
    if relative.as_os_str().is_empty() {
        return Err(ERROR);
    }
    let mut current = root.to_path_buf();
    for part in relative.components() {
        let Component::Normal(part) = part else {
            return Err(ERROR);
        };
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(m) if m.file_type().is_symlink() || (!m.is_file() && !m.is_dir()) => {
                return Err(ERROR)
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ERROR),
        }
    }
    Ok(())
}
pub async fn offline_snapshot(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let [mode, root_flag, root, source_flag, source, destination_flag, destination, rest @ ..] =
        args
    else {
        return Err(ERROR.into());
    };
    if root_flag != "--test-root"
        || source_flag != "--source"
        || destination_flag != "--destination"
    {
        return Err(ERROR.into());
    }
    let root = Path::new(root);
    validate_root(root)?;
    let source = Path::new(source);
    let destination = Path::new(destination);
    controlled_path(root, source)?;
    controlled_path(root, destination)?;
    match (mode.as_str(), rest) {
        ("backup", []) => calendar_storage::backup::backup(source, destination).await?,
        ("restore", [flag, cutoff]) if flag == "--cutoff-ms" => {
            calendar_storage::backup::restore(source, destination, cutoff.parse()?).await?
        }
        _ => return Err(ERROR.into()),
    }
    Ok(())
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClockFrame {
    utc_now_ms: i64,
    monotonic_offset_ms: u64,
}
struct ClockCache {
    frame: ClockFrame,
    read_at: Instant,
}
struct FileClock {
    root: PathBuf,
    path: PathBuf,
    started: Instant,
    cache: Mutex<ClockCache>,
}
impl FileClock {
    fn read(root: &Path, path: &Path) -> Result<ClockFrame, ConfigError> {
        controlled_path(root, path)?;
        let bytes = read_secret_file(path, 1024).map_err(|_| ERROR)?;
        let frame: ClockFrame = serde_json::from_slice(bytes.expose()).map_err(|_| ERROR)?;
        if !(0..=253_399_708_799_999).contains(&frame.utc_now_ms)
            || frame.monotonic_offset_ms > 31_536_000_000
        {
            return Err(ERROR);
        }
        Ok(frame)
    }
    fn load(root: &Path, path: &Path) -> Result<Self, ConfigError> {
        let started = Instant::now();
        Ok(Self {
            root: root.into(),
            path: path.into(),
            started,
            cache: Mutex::new(ClockCache {
                frame: Self::read(root, path)?,
                read_at: started,
            }),
        })
    }
    fn frame(&self) -> ClockFrame {
        let mut cache = self.cache.lock().expect("test clock lock failed");
        if cache.read_at.elapsed() >= Duration::from_millis(10) {
            let frame = Self::read(&self.root, &self.path).expect("invalid test clock control");
            assert!(
                frame.monotonic_offset_ms >= cache.frame.monotonic_offset_ms,
                "test monotonic offset cannot decrease"
            );
            cache.frame = frame;
            cache.read_at = Instant::now();
        }
        cache.frame
    }
}
impl Clock for FileClock {
    fn utc_now_ms(&self) -> i64 {
        self.frame().utc_now_ms
    }
    fn monotonic_now_ms(&self) -> u64 {
        self.frame()
            .monotonic_offset_ms
            .saturating_add(self.started.elapsed().as_millis() as u64)
    }
}
fn loopback_target(target: &str) -> Result<(), ConfigError> {
    if target.len() > 4096 {
        return Err(ERROR);
    }
    let url = reqwest::Url::parse(target).map_err(|_| ERROR)?;
    let ip = url
        .host_str()
        .and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
        .ok_or(ERROR)?;
    if url.scheme() != "http"
        || !ip.is_loopback()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ERROR);
    }
    Ok(())
}
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(8))
        .build()
        .expect("test HTTP client")
}
async fn bounded(mut response: reqwest::Response) -> Result<Vec<u8>, ()> {
    let mut bytes = vec![];
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if chunk.len() > 65_536 - bytes.len() {
            return Err(());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
struct LoopbackFetcher {
    targets: Targets,
}
impl RemoteFetcher for LoopbackFetcher {
    async fn fetch(
        &self,
        request: RemoteFetchRequest,
    ) -> Result<RemoteFetchOutcome, RemoteFetchError> {
        if !matches!(request.authentication, RemoteAuthentication::None) {
            return Err(RemoteFetchError::Credential);
        }
        let target = match request.url.as_str() {
            SOURCE_A => &self.targets.a,
            SOURCE_B => &self.targets.b,
            _ => return Err(RemoteFetchError::InvalidConfiguration),
        };
        let mut get = client().get(target);
        if let Some(tag) = request.validators.etag {
            get = get.header("if-none-match", tag);
        }
        let response = get.send().await.map_err(|_| RemoteFetchError::Transport)?;
        let status = response.status().as_u16();
        let validators = RemoteValidators {
            etag: response
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .filter(|v| v.len() <= 1024)
                .map(str::to_owned),
            last_modified: None,
        };
        match status {
            304 => Ok(RemoteFetchOutcome::NotModified(validators)),
            200 => Ok(RemoteFetchOutcome::Modified(RemoteDocument {
                credential_revision: None,
                final_url: request.url,
                body: bounded(response)
                    .await
                    .map_err(|_| RemoteFetchError::TooLarge)?,
                validators,
            })),
            _ => Err(RemoteFetchError::Server {
                status,
                retry_after_ms: Some(1000),
            }),
        }
    }
}
struct LoopbackBark {
    services: Arc<crate::Services>,
    provider: RuntimeCredentialProvider,
    target: String,
}
impl PushOutput for LoopbackBark {
    async fn deliver(&self, envelope: DeliveryEnvelope) -> DeliveryOutcome {
        let config = match self
            .services
            .storage
            .output_for_delivery(&envelope.job_id, &envelope.output_id)
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
        if !matches!(
            config.origin.as_str(),
            "https://bark.system.invalid"
                | "https://bark.system.invalid/"
                | "https://bark.system.invalid/push"
        ) {
            return DeliveryOutcome::PermanentFailure;
        }
        let secret = match self
            .provider
            .credential(&config.credential_id, "bark")
            .await
        {
            Ok(s) => s,
            Err(_) => return DeliveryOutcome::PermanentFailure,
        };
        let Ok(key) = std::str::from_utf8(secret.expose()) else {
            return DeliveryOutcome::PermanentFailure;
        };
        let body = envelope
            .event
            .description
            .as_deref()
            .unwrap_or(&envelope.event.title);
        if !key.starts_with("synthetic_")
            || key.len() > 4096
            || !key.bytes().all(|b| b.is_ascii_graphic())
            || envelope.event.title.len() + body.len() > 65_536
        {
            return DeliveryOutcome::PermanentFailure;
        }
        let payload =
            serde_json::json!({"device_key":key,"title":envelope.event.title,"body":body});
        let response = match client()
            .post(&self.target)
            .timeout(Duration::from_millis(config.request_timeout_ms.min(8000)))
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&payload).expect("test payload"))
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
        let status = response.status().as_u16();
        if status == 503 || status == 429 || status >= 500 {
            return DeliveryOutcome::Retryable {
                retry_after_ms: Some(1000),
            };
        }
        if status != 200 {
            return DeliveryOutcome::PermanentFailure;
        }
        match bounded(response)
            .await
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v.get("code").and_then(|n| n.as_u64()))
        {
            Some(200) => DeliveryOutcome::Accepted,
            _ => DeliveryOutcome::Unknown {
                retry_after_ms: Some(1000),
            },
        }
    }
}
