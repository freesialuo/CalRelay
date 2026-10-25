//! Typed helpers for explicit isolated harnesses; no binary flags, routes or transport maps.
use crate::{
    production_runtime::supervision_budgets, ConfigError, DeploymentConfig, Server, StartError,
};
use calendar_application::Clock;
use calendar_storage::security::read_secret_file;
use serde::Deserialize;
use std::{
    path::{Component, Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
const ERROR: ConfigError = ConfigError("invalid controlled test runtime input");

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DatabaseMode {
    #[default]
    Fresh,
    Reopen,
    Restored,
}
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error(transparent)]
    Invalid(#[from] ConfigError),
    #[error("test runtime startup failed")]
    Startup(#[from] StartError),
    #[error("test recovery control unavailable")]
    Database(#[from] sqlx::Error),
    #[error("test snapshot operation failed")]
    Snapshot(#[from] calendar_storage::backup::Error),
}
#[derive(Clone)]
pub struct HarnessRoot {
    path: PathBuf,
}
impl HarnessRoot {
    pub fn new(path: PathBuf) -> Result<Self, ConfigError> {
        validate_root(&path)?;
        Ok(Self { path })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Credentials-only deployment file; the caller retains the existing real guard assembly.
    /// Fresh exclusively creates. Reopen/restored require an existing controlled regular file.
    pub async fn initialize(
        &self,
        database: &Path,
        deployment: &Path,
        mode: DatabaseMode,
    ) -> Result<Server, HarnessError> {
        controlled_path(&self.path, database)?;
        controlled_path(&self.path, deployment)?;
        let bytes = read_secret_file(deployment, 65_536).map_err(|_| ERROR)?;
        let config = DeploymentConfig::from_deployment(bytes.expose(), |key| {
            (key == "CALRELAY_DATABASE").then(|| database.to_string_lossy().into_owned())
        })?;
        if !config.server.bind.ip().is_loopback()
            || config.server.bind.port() != 0
            || config.admin.is_some()
        {
            return Err(ERROR.into());
        }
        let keys = config.credentials.as_ref().ok_or(ERROR)?;
        for key in keys.key_files.values() {
            controlled_path(&self.path, key)?;
        }
        let keys = keys.load()?;
        match mode {
            DatabaseMode::Fresh => {
                std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(database)
                    .map_err(|_| ConfigError("fresh test database must not exist"))?;
            }
            DatabaseMode::Reopen | DatabaseMode::Restored => {
                let m = std::fs::symlink_metadata(database).map_err(|_| ERROR)?;
                if !m.is_file() || m.len() == 0 {
                    return Err(ERROR.into());
                }
            }
        }
        let server = Server::initialize_with_supervision(config.server, supervision_budgets())
            .await?
            .with_credential_keys(keys)?;
        if matches!(mode, DatabaseMode::Restored) {
            let control: (bool, Option<i64>) =
                sqlx::query_as("SELECT paused,cutoff FROM recovery_control WHERE id=1")
                    .fetch_one(server.services.storage.pool())
                    .await?;
            if !control.0 || control.1.is_none() {
                server.services.drain().await;
                return Err(
                    ConfigError("restored test database must remain paused with a cutoff").into(),
                );
            }
        }
        Ok(server)
    }
    pub async fn backup(&self, database: &Path, destination: &Path) -> Result<(), HarnessError> {
        controlled_path(&self.path, database)?;
        controlled_path(&self.path, destination)?;
        calendar_storage::backup::backup(database, destination).await?;
        Ok(())
    }
    pub async fn restore(
        &self,
        snapshot: &Path,
        destination: &Path,
        cutoff: i64,
    ) -> Result<PathBuf, HarnessError> {
        for path in [
            snapshot.to_path_buf(),
            snapshot.join("manifest.json"),
            snapshot.join("snapshot.sqlite3"),
            destination.to_path_buf(),
        ] {
            controlled_path(&self.path, &path)?;
        }
        calendar_storage::backup::restore(snapshot, destination, cutoff).await?;
        Ok(destination.join("database.sqlite3"))
    }
}
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
#[derive(Clone, Copy, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClockFrame {
    pub utc_now_ms: i64,
    pub monotonic_offset_ms: u64,
}
struct ClockCache {
    frame: ClockFrame,
    read_at: Instant,
}
pub struct FileBusinessClock {
    root: PathBuf,
    path: PathBuf,
    started: Instant,
    cache: Mutex<ClockCache>,
}
impl FileBusinessClock {
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
    pub fn load(root: &HarnessRoot, path: &Path) -> Result<Self, ConfigError> {
        let root = root.path();
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
impl Clock for FileBusinessClock {
    fn utc_now_ms(&self) -> i64 {
        self.frame().utc_now_ms
    }
    fn monotonic_now_ms(&self) -> u64 {
        self.frame()
            .monotonic_offset_ms
            .saturating_add(self.started.elapsed().as_millis() as u64)
    }
}
