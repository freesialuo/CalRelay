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
        DeploymentConfig::from_args(args)?.server_only()
    }
    /// Parse only deployment addressing; never load credentials or initialize storage.
    pub fn deployment_bind_from_args(
        args: impl IntoIterator<Item = String>,
    ) -> Result<SocketAddr, ConfigError> {
        Ok(DeploymentConfig::document_from_args(args)?.bind)
    }
    pub fn from_deployment(
        bytes: &[u8],
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        DeploymentConfig::from_deployment(bytes, get)?.server_only()
    }
    fn with_bind(
        bind: SocketAddr,
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let config = Self {
            bind,
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

/// Parsed once; admin material is validated and loaded before database/listener startup.
pub struct DeploymentConfig {
    pub server: Config,
    pub admin: Option<crate::admin_bootstrap::AdminBootstrapConfig>,
    pub credentials: Option<crate::credentials_bootstrap::CredentialKeysConfig>,
}
impl DeploymentConfig {
    pub fn from_args(args: impl IntoIterator<Item = String>) -> Result<Self, ConfigError> {
        let document = Self::document_from_args(args)?;
        Self::from_document(document, |key| std::env::var(key).ok())
    }
    fn document_from_args(args: impl IntoIterator<Item = String>) -> Result<Document, ConfigError> {
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
        use std::io::Read;
        let file = std::fs::File::open(path)
            .map_err(|_| ConfigError("cannot read deployment configuration"))?;
        let mut bytes = Vec::new();
        file.take(65537)
            .read_to_end(&mut bytes)
            .map_err(|_| ConfigError("cannot read deployment configuration"))?;
        Self::parse_document(&bytes)
    }
    pub fn from_deployment(
        bytes: &[u8],
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        if get("CALRELAY_BIND").is_some() {
            return Err(ConfigError("CALRELAY_BIND was removed; remove it and use --deployment-config FILE with explicit bind"));
        }
        Self::from_document(Self::parse_document(bytes)?, get)
    }
    fn parse_document(bytes: &[u8]) -> Result<Document, ConfigError> {
        if bytes.len() > 65536 {
            return Err(ConfigError("deployment configuration exceeds limit"));
        }
        serde_json::from_slice(bytes).map_err(|_| {
            ConfigError(
                "invalid deployment configuration; expected explicit bind and optional admin/credentials",
            )
        })
    }
    fn from_document(
        document: Document,
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        if let Some(admin) = &document.admin {
            admin.validate()?;
        }
        if let Some(credentials) = &document.credentials {
            credentials.validate()?;
        }
        Ok(Self {
            server: Config::with_bind(document.bind, get)?,
            admin: document.admin,
            credentials: document.credentials,
        })
    }
    fn server_only(self) -> Result<Config, ConfigError> {
        if self.admin.is_some() {
            return Err(ConfigError(
                "admin configuration requires DeploymentConfig bootstrap",
            ));
        }
        if self.credentials.is_some() {
            return Err(ConfigError(
                "credentials configuration requires DeploymentConfig bootstrap",
            ));
        }
        Ok(self.server)
    }
    pub async fn initialize(self) -> Result<crate::Server, crate::StartError> {
        self.initialize_with_supervision(crate::supervision::SupervisionConfig::default())
            .await
    }
    pub async fn initialize_with_supervision(
        self,
        budgets: crate::supervision::SupervisionConfig,
    ) -> Result<crate::Server, crate::StartError> {
        // Bad secrets fail before migrations create or modify a database.
        let verifier = if let Some(admin) = &self.admin {
            Some(std::sync::Arc::new(admin.load_verifier().await?))
        } else {
            None
        };
        let keys = self.credentials.as_ref().map(|c| c.load()).transpose()?;
        let mut server = crate::Server::initialize_with_supervision(self.server, budgets).await?;
        if let Some(keys) = keys {
            server = server.with_credential_keys(keys)?;
        }
        if let (Some(admin), Some(verifier)) = (self.admin, verifier) {
            Ok(server.with_admin_site_key(verifier, admin.site_key)?)
        } else {
            Ok(server)
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    bind: SocketAddr,
    #[serde(default, deserialize_with = "crate::admin_bootstrap::configured_admin")]
    admin: Option<crate::admin_bootstrap::AdminBootstrapConfig>,
    #[serde(
        default,
        deserialize_with = "crate::credentials_bootstrap::configured_credentials"
    )]
    credentials: Option<crate::credentials_bootstrap::CredentialKeysConfig>,
}
