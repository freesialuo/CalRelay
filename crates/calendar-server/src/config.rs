use std::{net::SocketAddr, path::PathBuf, time::Duration};

/// No implicit database or bind address: a missing configuration must fail closed.
#[derive(Clone, Debug)]
pub struct Config {
    pub bind: SocketAddr,
    pub database: PathBuf,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub request_timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid or missing server configuration: {0}")]
pub struct ConfigError(pub &'static str);

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let config = Self {
            bind: get("CALRELAY_BIND")
                .ok_or(ConfigError("CALRELAY_BIND"))?
                .parse()
                .map_err(|_| ConfigError("CALRELAY_BIND"))?,
            database: get("CALRELAY_DATABASE")
                .ok_or(ConfigError("CALRELAY_DATABASE"))?
                .into(),
            max_request_bytes: number(&get, "CALRELAY_MAX_REQUEST_BYTES", 1_048_576)?,
            max_response_bytes: number(&get, "CALRELAY_MAX_RESPONSE_BYTES", 1_048_576)?,
            request_timeout: Duration::from_millis(number(
                &get,
                "CALRELAY_REQUEST_TIMEOUT_MS",
                30_000,
            )? as u64),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.database.as_os_str().is_empty() || self.database.to_str() == Some(":memory:") {
            return Err(ConfigError("CALRELAY_DATABASE must name a persistent file"));
        }
        if !(1..=64 * 1024 * 1024).contains(&self.max_request_bytes) {
            return Err(ConfigError("CALRELAY_MAX_REQUEST_BYTES"));
        }
        // Leave space for every bounded error envelope (including its generated request id).
        if !(512..=64 * 1024 * 1024).contains(&self.max_response_bytes) {
            return Err(ConfigError("CALRELAY_MAX_RESPONSE_BYTES"));
        }
        if self.request_timeout.is_zero() || self.request_timeout > Duration::from_secs(120) {
            return Err(ConfigError("CALRELAY_REQUEST_TIMEOUT_MS"));
        }
        Ok(())
    }
}
fn number(
    get: &impl Fn(&str) -> Option<String>,
    key: &'static str,
    default: usize,
) -> Result<usize, ConfigError> {
    get(key)
        .map(|s| s.parse().map_err(|_| ConfigError(key)))
        .unwrap_or(Ok(default))
}
