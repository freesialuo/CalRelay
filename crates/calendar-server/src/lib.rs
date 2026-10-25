//! Service assembly, health observation and lifecycle.
pub mod admin;
pub mod admin_bootstrap;
mod aggregate_configuration;
mod aggregate_feed;
mod config;
pub mod credentials_bootstrap;
mod feed;
mod http;
mod ingest;
mod reminder_configuration;
mod source_configuration;
pub mod source_worker;
pub mod supervision;
#[cfg(feature = "production-web")]
mod web;
use calendar_adapters::IcsLimits;
use calendar_application::DeliveryPolicy;
use calendar_storage::Storage;
pub use config::{Config, ConfigError, DeploymentConfig};
pub use http::{openapi, router, RequestId};
use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
};
use tokio::{
    net::TcpListener,
    sync::{watch, RwLock},
};

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Configuration(#[from] ConfigError),
    #[error("database initialization failed")]
    Storage(#[from] calendar_storage::OpenError),
    #[error("server I/O failed")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Supervision(#[from] supervision::CoreFault),
}

/// Every future task claimant must enter through `run_task`. The read permit spans the
/// entire bounded application operation, including its final short transaction.
pub struct Services {
    pub storage: Storage,
    credential_keys: OnceLock<Arc<calendar_storage::security::KeyRing>>,
    pub health: Arc<supervision::Health>,
    pub ingest_policy: calendar_application::IngestPolicy,
    pub delivery_policy: DeliveryPolicy,
    pub ics_limits: IcsLimits,
    stopping: AtomicBool,
    tasks: RwLock<()>,
    shutdown: watch::Sender<bool>,
    force_cancel: watch::Sender<bool>,
}
impl Services {
    pub fn credential_provider(
        self: &Arc<Self>,
    ) -> Result<credentials_bootstrap::RuntimeCredentialProvider, calendar_application::SecurityError>
    {
        self.credential_keys
            .get()
            .ok_or(calendar_application::SecurityError::MissingKey)?;
        Ok(credentials_bootstrap::RuntimeCredentialProvider {
            services: self.clone(),
        })
    }
    /// Business task admission; HTTP readiness is evaluated by `health.snapshot()`.
    pub fn ready(&self) -> bool {
        !self.stopping.load(Ordering::SeqCst)
    }
    pub fn stop(&self) {
        self.health.stop();
        self.stopping.store(true, Ordering::SeqCst);
        self.shutdown.send_replace(true);
    }
    /// None means admission was refused or force-cancelled after drain expiry.
    /// Keep the complete bounded operation and its transactions inside this future.
    pub async fn run_task<T>(&self, operation: impl AsyncFnOnce(&Storage) -> T) -> Option<T> {
        if !self.ready() {
            return None;
        }
        let _permit = self.tasks.read().await;
        if !self.ready() {
            return None;
        }
        let mut cancellation = self.force_cancel.subscribe();
        tokio::select! {
            biased;
            () = async {
                while !*cancellation.borrow_and_update() {
                    if cancellation.changed().await.is_err() { break; }
                }
            } => None,
            result = operation(&self.storage) => Some(result),
        }
    }
    fn cancel_admitted(&self) {
        self.force_cancel.send_replace(true);
    }
    async fn drain_cancelled(&self) {
        // Close the pool immediately while cancelled operations release their permits and
        // transactions; close waits for SQLx rollback/checked-out connection cleanup.
        tokio::join!(self.storage.close(), async {
            let _permit = self.tasks.write().await;
        });
    }
    pub async fn drain(&self) {
        self.stop();
        let _permit = self.tasks.write().await;
        self.storage.close().await;
    }
}

pub struct Server {
    pub services: Arc<Services>,
    config: Config,
    admin: Option<admin::AdminState>,
    workers: Vec<supervision::Worker>,
}
impl Server {
    /// Validate and migrate before constructing an initialized service or opening the listener.
    pub async fn initialize(config: Config) -> Result<Self, StartError> {
        Self::initialize_with_supervision(config, supervision::SupervisionConfig::default()).await
    }
    pub async fn initialize_with_supervision(
        config: Config,
        supervision: supervision::SupervisionConfig,
    ) -> Result<Self, StartError> {
        config.validate()?;
        supervision.validate()?;
        let ingest_policy = ingest::policy()?;
        let storage = Storage::open(&config.database).await?;
        let (shutdown, _) = watch::channel(false);
        let (force_cancel, _) = watch::channel(false);
        Ok(Self {
            config,
            admin: None,
            workers: Vec::new(),
            services: Arc::new(Services {
                storage,
                credential_keys: OnceLock::new(),
                health: Arc::new(supervision::Health::new(supervision)),
                ingest_policy,
                delivery_policy: DeliveryPolicy::default(),
                ics_limits: IcsLimits::default(),
                stopping: AtomicBool::new(false),
                tasks: RwLock::new(()),
                shutdown,
                force_cancel,
            }),
        })
    }
    pub fn with_credential_keys(
        self,
        keys: calendar_storage::security::KeyRing,
    ) -> Result<Self, ConfigError> {
        self.services
            .credential_keys
            .set(Arc::new(keys))
            .map_err(|_| ConfigError("credential keys already configured"))?;
        Ok(self)
    }
    pub async fn bind(&self) -> Result<TcpListener, StartError> {
        Ok(TcpListener::bind(self.config.bind).await?)
    }
    /// Explicit trusted assembly opt-in. The standard binary leaves management disabled.
    pub fn with_admin(
        mut self,
        verifier: Arc<calendar_adapters::admin_auth::CloudflareVerifier>,
    ) -> Self {
        self.admin = Some(admin::AdminState::new(self.services.clone(), verifier));
        self
    }
    pub fn with_admin_site_key(
        mut self,
        verifier: Arc<calendar_adapters::admin_auth::CloudflareVerifier>,
        site_key: String,
    ) -> Result<Self, ConfigError> {
        admin::validate_site_key(&site_key)?;
        let mut state = admin::AdminState::new(self.services.clone(), verifier);
        state.site_key = Some(site_key);
        self.admin = Some(state);
        Ok(self)
    }
    pub fn router(&self) -> axum::Router {
        http::router_with_admin(
            self.services.clone(),
            self.config.clone(),
            self.admin.clone(),
        )
    }
    pub async fn serve(
        self,
        listener: TcpListener,
        signal: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), StartError> {
        supervision::serve(self, listener, signal).await
    }
}
