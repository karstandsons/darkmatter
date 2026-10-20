//! HTTPS: a Let's Encrypt certificate fetched and renewed in the background
//! (TLS-ALPN-01), or certificate files.

use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use rustls_acme::AcmeConfig;
use rustls_acme::axum::AxumAcceptor;
use rustls_acme::caches::DirCache;

/// rustls needs to be told which crypto to use, once per process.
pub fn install_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Accepts TLS with a certificate for `domain`, kept in `cache` (private to
/// the server, like the database) and renewed before it expires.
pub fn acme(domain: &str, email: Option<&str>, staging: bool, cache: &Path) -> AxumAcceptor {
    if let Err(e) = std::fs::create_dir_all(cache) {
        tracing::error!("can't make {}: {e}", cache.display());
    }
    let mut state = AcmeConfig::new([domain])
        .contact(email.map(|e| format!("mailto:{e}")))
        .cache(DirCache::new(cache.to_path_buf()))
        .directory_lets_encrypt(!staging)
        .state();
    let acceptor = state.axum_acceptor(state.default_rustls_config());
    tokio::spawn(async move {
        while let Some(event) = state.next().await {
            match event {
                Ok(ok) => tracing::info!("certificate: {ok:?}"),
                Err(err) => tracing::error!("certificate: {err:?}"),
            }
        }
    });
    acceptor
}

/// Stops accepting on Ctrl+C or SIGTERM and gives open requests 10 s.
pub fn shutdown_handle() -> axum_server::Handle<std::net::SocketAddr> {
    let handle = axum_server::Handle::new();
    let on_signal = handle.clone();
    tokio::spawn(async move {
        crate::shutdown().await;
        on_signal.graceful_shutdown(Some(Duration::from_secs(10)));
    });
    handle
}
