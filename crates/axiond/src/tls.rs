//! HTTPS: a Let's Encrypt certificate fetched and renewed in the background
//! (TLS-ALPN-01), or certificate files.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rustls_acme::AcmeConfig;
use rustls_acme::axum::AxumAcceptor;
use rustls_acme::caches::DirCache;

/// rustls needs to be told which crypto to use, once per process.
pub fn install_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

/// Accepts TLS with a certificate for `domain`, kept in `data_dir/acme`
/// (private to the server, like the database) and renewed before it expires.
pub fn acme(domain: &str, config: &crate::Config) -> anyhow::Result<AxumAcceptor> {
    let cache = config.data_dir.join("acme");
    if let Err(e) = std::fs::create_dir_all(&cache) {
        tracing::error!("can't make {}: {e}", cache.display());
    }
    let mut acme = AcmeConfig::new([domain])
        .contact(config.acme_email.as_ref().map(|e| format!("mailto:{e}")))
        .cache(DirCache::new(cache))
        .directory_lets_encrypt(!config.acme_staging);
    if let Some(url) = &config.acme_directory {
        acme = acme.directory(url);
    }
    if let Some(ca) = &config.acme_ca {
        acme = acme.client_tls_config(Arc::new(trusting(ca)?));
    }
    let mut state = acme.state();
    // HTTP/2 when the app offers it.
    let mut rustls = (*state.default_rustls_config()).clone();
    rustls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = state.axum_acceptor(Arc::new(rustls));
    tokio::spawn(async move {
        while let Some(event) = state.next().await {
            match event {
                Ok(ok) => tracing::info!("certificate: {ok:?}"),
                Err(err) => tracing::error!("certificate: {err:?}"),
            }
        }
    });
    Ok(acceptor)
}

/// A TLS client that trusts only the CA in `pem`.
fn trusting(pem: &std::path::Path) -> anyhow::Result<rustls::ClientConfig> {
    use rustls_pki_types::CertificateDer;
    use rustls_pki_types::pem::PemObject;
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_file_iter(pem).map_err(|e| anyhow::anyhow!("{}: {e}", pem.display()))? {
        roots.add(cert.map_err(|e| anyhow::anyhow!("{}: {e}", pem.display()))?)?;
    }
    Ok(rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth())
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
