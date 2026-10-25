//! Service assembly and lifecycle. Business routes and production outputs are intentionally absent.
pub mod admin;
pub mod admin_bootstrap;
mod aggregate_configuration;
mod aggregate_feed;
mod config;
mod feed;
mod http;
mod ingest;
mod reminder_configuration;
mod source_configuration;
pub mod source_worker;
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
        Arc,
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
}

/// Every future task claimant must enter through `run_task`. The read permit spans the
/// entire bounded application operation, including its final short transaction.
pub struct Services {
    pub storage: Storage,
    pub ingest_policy: calendar_application::IngestPolicy,
    pub delivery_policy: DeliveryPolicy,
    pub ics_limits: IcsLimits,
    stopping: AtomicBool,
    tasks: RwLock<()>,
    shutdown: watch::Sender<bool>,
}
impl Services {
    pub fn ready(&self) -> bool {
        !self.stopping.load(Ordering::SeqCst)
    }
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.shutdown.send_replace(true);
    }
    pub async fn run_task<T>(&self, operation: impl AsyncFnOnce(&Storage) -> T) -> Option<T> {
        if !self.ready() {
            return None;
        }
        let _permit = self.tasks.read().await;
        if !self.ready() {
            return None;
        }
        Some(operation(&self.storage).await)
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
}
impl Server {
    /// Validate and migrate before constructing a ready service or opening the listener.
    pub async fn initialize(config: Config) -> Result<Self, StartError> {
        config.validate()?;
        let ingest_policy = ingest::policy()?;
        let storage = Storage::open(&config.database).await?;
        let (shutdown, _) = watch::channel(false);
        Ok(Self {
            config,
            admin: None,
            services: Arc::new(Services {
                storage,
                ingest_policy,
                delivery_policy: DeliveryPolicy::default(),
                ics_limits: IcsLimits::default(),
                stopping: AtomicBool::new(false),
                tasks: RwLock::new(()),
                shutdown,
            }),
        })
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
        let services = self.services.clone();
        let mut shutdown = services.shutdown.subscribe();
        let result = axum::serve(listener, self.router()).with_graceful_shutdown(async move {
            tokio::select! {
                () = signal => {},
                _ = async { while !*shutdown.borrow_and_update() { if shutdown.changed().await.is_err() { break; } } } => {},
            }
            services.stop();
        }).await;
        self.services.drain().await;
        result.map_err(StartError::Io)
    }
}
