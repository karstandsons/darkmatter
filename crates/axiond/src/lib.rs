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
mod calls;
pub mod config;
mod db;
pub mod e2ee;
mod error;
pub mod housekeeping;
mod hub;
mod ids;
mod limits;
mod secrets;
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
    e2ee: e2ee::State,
    calls: calls::Calls,
    passwords: auth::Passwords,
    secrets: secrets::SecretKey,
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
        let e2ee = e2ee::init(&db).await?;
        let secrets = secrets::SecretKey::load_or_create(&config.secret_key_path())?;
        let sealed = secrets::seal_old_secrets(&db, &secrets).await?;
        if sealed > 0 {
            tracing::info!("encrypted {sealed} two-factor secret(s) that were stored in plaintext");
        }
        let passwords = auth::Passwords::load(&db).await?;
        Ok(App(Arc::new(State { config, db, hub: Default::default(), e2ee, calls: Default::default(), passwords, secrets, limiter: Default::default() })))
    }

    /// Measures this machine and sets the password hashing cost (argon2id)
    /// so one password takes about a quarter of a second, never below
    /// OWASP's minimum. Returns the settings chosen. Passwords hashed before
    /// are hashed again at the new cost the next time they're used.
    pub async fn tune_passwords(&self) -> anyhow::Result<String> {
        let params = self.passwords.tune(&self.db, std::time::Duration::from_millis(250)).await?;
        Ok(auth::show_params(&params))
    }

    /// Whether `tune_passwords` has been run on this server.
    pub async fn passwords_tuned(&self) -> anyhow::Result<bool> {
        Ok(auth::Passwords::tuned(&self.db).await?)
    }

    /// People whose two-factor secrets the secret key can't open: it isn't
    /// the key they were encrypted with (a restore without `secret.key`).
    pub async fn unreadable_two_factor(&self) -> anyhow::Result<Vec<String>> {
        Ok(secrets::unreadable(&self.db, &self.secrets).await?)
    }

    /// Serves the API on `listener` until Ctrl+C or SIGTERM: HTTPS if the
    /// config has a domain or certificate files, plain HTTP otherwise.
    pub async fn serve(self, listener: TcpListener) -> anyhow::Result<()> {
        let db = self.db.clone();
        let tls = self.config.tls()?;
        self.schedule_backups();
        housekeeping::start(&self);
        calls::start(&self, listener.local_addr()?).await?;
        let config = self.config.clone();
        let app = self.clone();
        let service = api::router(self).into_make_service_with_connect_info::<std::net::SocketAddr>();
        // Each connection to the port is HTTP(S), or call media over TCP.
        let server = axum_server::from_tcp(listener.into_std()?)?.handle(tls::shutdown_handle());
        match tls {
            config::Tls::None => server.acceptor(calls::Split::new(axum_server::accept::DefaultAcceptor, &app)).serve(service).await?,
            config::Tls::Files(cert, key) => {
                tls::install_provider();
                let rustls = axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert, &key)
                    .await
                    .map_err(|e| anyhow::anyhow!("reading {} and {}: {e}", cert.display(), key.display()))?;
                let acceptor = axum_server::tls_rustls::RustlsAcceptor::new(rustls);
                server.acceptor(calls::Split::new(acceptor, &app)).serve(service).await?;
            }
            config::Tls::Acme(domain) => {
                tls::install_provider();
                let acceptor = tls::acme(&domain, &config)?;
                server.acceptor(calls::Split::new(acceptor, &app)).serve(service).await?;
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
