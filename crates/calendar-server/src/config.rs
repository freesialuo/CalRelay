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
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Result<Self, ConfigError> {
        if std::env::var_os("CALRELAY_BIND").is_some() {
            return Err(ConfigError("CALRELAY_BIND was removed; remove it and use --deployment-config FILE with explicit bind"));
        }
        let args: Vec<_> = args.into_iter().collect();
        let [flag, path] = args.as_slice() else {
            return Err(ConfigError("expected --deployment-config FILE"));
        };
        if flag != "--deployment-config" {
            return Err(ConfigError("expected --deployment-config FILE"));
        }
        let bytes =
            std::fs::read(path).map_err(|_| ConfigError("cannot read deployment configuration"))?;
        Self::from_deployment(&bytes, |key| std::env::var(key).ok())
    }

    pub fn from_deployment(
        bytes: &[u8],
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        if get("CALRELAY_BIND").is_some() {
            return Err(ConfigError("CALRELAY_BIND was removed; remove it and use --deployment-config FILE with explicit bind"));
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Deployment {
            bind: SocketAddr,
        }
        let deployment: Deployment = serde_json::from_slice(bytes).map_err(|_| {
            ConfigError("deployment configuration must contain only an explicit bind address")
        })?;
        let config = Self {
            bind: deployment.bind,
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
