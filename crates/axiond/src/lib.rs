//! axiond, the Darkmatter chat server.
//!
//! It speaks the part of the Mattermost API v4 that Darkmatter uses, so
//! Darkmatter (and anything else built on `mm-api`) logs in to it unchanged.
#![forbid(unsafe_code)]

pub mod admin;
mod api;
pub mod audit;
mod auth;
pub mod backup;
pub mod config;
mod db;
mod error;
pub mod housekeeping;
mod hub;
mod ids;
mod limits;
mod tls;

use std::ops::Deref;
use std::sync::Arc;

use tokio::net::TcpListener;

pub use config::Config;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Everything a request handler can reach. Cheap to clone.
#[derive(Clone)]
pub struct App(Arc<State>);

pub struct State {
    pub config: Config,
    pub db: db::Db,
    hub: hub::Hub,
    guard: auth::Guard,
    limiter: limits::Limiter,
}

impl Deref for App {
    type Target = State;

    fn deref(&self) -> &State {
        &self.0
    }
}

impl App {
    /// Opens the database in the configured data folder.
    pub async fn open(config: Config) -> anyhow::Result<Self> {
        let db = db::open(&config.data_dir).await?;
        Ok(App(Arc::new(State { config, db, hub: Default::default(), guard: Default::default(), limiter: Default::default() })))
    }

    /// Serves the API on `listener` until Ctrl+C or SIGTERM: HTTPS if the
    /// config has a domain or certificate files, plain HTTP otherwise.
    pub async fn serve(self, listener: TcpListener) -> anyhow::Result<()> {
        let db = self.db.clone();
        let tls = self.config.tls()?;
        self.schedule_backups();
        housekeeping::start(&self);
        let data_dir = self.config.data_dir.clone();
        let (email, staging) = (self.config.acme_email.clone(), self.config.acme_staging);
        let service = api::router(self).into_make_service_with_connect_info::<std::net::SocketAddr>();
        match tls {
            config::Tls::None => axum::serve(listener, service).with_graceful_shutdown(shutdown()).await?,
            config::Tls::Files(cert, key) => {
                tls::install_provider();
                let rustls = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
                    .await
                    .map_err(|e| anyhow::anyhow!("reading {} and {}: {e}", cert.display(), key.display()))?;
                let server = axum_server::from_tcp_rustls(listener.into_std()?, rustls)?;
                server.handle(tls::shutdown_handle()).serve(service).await?;
            }
            config::Tls::Acme(domain) => {
                tls::install_provider();
                let acceptor = tls::acme(&domain, email.as_deref(), staging, &data_dir.join("acme"));
                let server = axum_server::from_tcp(listener.into_std()?)?.acceptor(acceptor);
                server.handle(tls::shutdown_handle()).serve(service).await?;
            }
        }
        db.close().await;
        Ok(())
    }
}

impl App {
    /// Backs up into `data_dir/backups` every `backup_hours`, keeping the
    /// newest `backup_keep`.
    fn schedule_backups(&self) {
        let hours = self.config.backup_hours;
        if hours == 0 {
            return;
        }
        let (db, data, keep) = (self.db.clone(), self.config.data_dir.clone(), self.config.backup_keep.max(1));
        tokio::spawn(async move {
            let every = std::time::Duration::from_secs(hours * 3600);
            let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
            loop {
                tick.tick().await;
                let dir = data.join("backups");
                match backup::create(&db, &data, &dir).await {
                    Ok(path) => {
                        tracing::info!("backed up to {}", path.display());
                        if let Err(e) = backup::prune(&dir, keep) {
                            tracing::warn!("couldn't remove old backups: {e}");
                        }
                    }
                    Err(e) => tracing::error!("backup failed: {e:#}"),
                }
            }
        });
    }
}

pub(crate) async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
    tracing::info!("shutting down");
}
