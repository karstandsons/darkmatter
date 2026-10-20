//! axiond, the Darkmatter chat server.
//!
//! It speaks the part of the Mattermost API v4 that Darkmatter uses, so
//! Darkmatter (and anything else built on `mm-api`) logs in to it unchanged.
#![forbid(unsafe_code)]

pub mod admin;
mod api;
pub mod audit;
mod auth;
pub mod config;
mod db;
mod error;
mod hub;
mod ids;
mod limits;

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

    /// Serves the API on `listener` until Ctrl+C or SIGTERM.
    pub async fn serve(self, listener: TcpListener) -> anyhow::Result<()> {
        let db = self.db.clone();
        let service = api::router(self).into_make_service_with_connect_info::<std::net::SocketAddr>();
        axum::serve(listener, service).with_graceful_shutdown(shutdown()).await?;
        db.close().await;
        Ok(())
    }
}

async fn shutdown() {
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
