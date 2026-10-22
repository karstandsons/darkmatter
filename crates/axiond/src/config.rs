//! `axiond.toml`, with `AXIOND_*` environment variables on top.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where to listen. Anything but localhost needs TLS: `domain`, the
    /// `tls_cert` and `tls_key` files, or `behind_proxy`.
    pub listen: SocketAddr,
    /// The database and uploaded files.
    pub data_dir: PathBuf,
    /// A TLS proxy (Caddy, Traefik, nginx) is in front.
    pub behind_proxy: bool,
    /// The name apps show for this server.
    pub site_name: String,
    /// The largest file someone can upload, in megabytes.
    pub max_file_mb: u64,
    /// Serve HTTPS for this domain with a certificate from Let's Encrypt,
    /// fetched and renewed by itself (TLS-ALPN-01, so `listen` must be
    /// reachable on port 443).
    pub domain: Option<String>,
    /// Where Let's Encrypt sends notices about the certificate.
    pub acme_email: Option<String>,
    /// Use Let's Encrypt's staging service (for trying things out).
    pub acme_staging: bool,
    /// Another ACME server instead of Let's Encrypt, and a CA certificate
    /// (PEM) to trust when talking to it. For testing against Pebble.
    pub acme_directory: Option<String>,
    pub acme_ca: Option<PathBuf>,
    /// Serve HTTPS with these PEM files instead of Let's Encrypt's (for
    /// `domain`, if set).
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    /// Take a backup into `data_dir/backups` this often (0 = never).
    pub backup_hours: u64,
    /// How many of those backups to keep.
    pub backup_keep: usize,
    /// The UDP address call media comes in on: one port for every call.
    /// Unset, a free port on `listen`'s address (fine on localhost; open a
    /// fixed one, e.g. `0.0.0.0:443`, on a server).
    pub calls_udp: Option<SocketAddr>,
    /// The address devices send call media to, when `calls_udp` is
    /// `0.0.0.0` or behind NAT: this server's public IP.
    pub calls_public_ip: Option<IpAddr>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: SocketAddr::from(([127, 0, 0, 1], 8065)),
            data_dir: PathBuf::from("data"),
            behind_proxy: false,
            site_name: "axiond".into(),
            max_file_mb: 100,
            domain: None,
            acme_email: None,
            acme_staging: false,
            acme_directory: None,
            acme_ca: None,
            tls_cert: None,
            tls_key: None,
            backup_hours: 24,
            backup_keep: 7,
            calls_udp: None,
            calls_public_ip: None,
        }
    }
}

impl Config {
    /// Reads `path`, or `axiond.toml` in the working directory if there is
    /// one, then applies the environment.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let default = Path::new("axiond.toml");
        let path = path.or(default.exists().then_some(default));
        let mut config = match path {
            Some(p) => {
                let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
                toml::from_str(&text).with_context(|| format!("in {}", p.display()))?
            }
            None => Config::default(),
        };
        config.apply_env(|k| std::env::var(k).ok())?;
        Ok(config)
    }

    fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) -> anyhow::Result<()> {
        if let Some(v) = var("AXIOND_LISTEN") {
            self.listen = v.parse().with_context(|| format!("AXIOND_LISTEN={v}"))?;
        }
        if let Some(v) = var("AXIOND_DATA_DIR") {
            self.data_dir = v.into();
        }
        if let Some(v) = var("AXIOND_BEHIND_PROXY") {
            self.behind_proxy = matches!(v.as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = var("AXIOND_SITE_NAME") {
            self.site_name = v;
        }
        let some = |v: String| (!v.trim().is_empty()).then(|| v.trim().to_string());
        if let Some(v) = var("AXIOND_DOMAIN") {
            self.domain = some(v);
        }
        if let Some(v) = var("AXIOND_ACME_EMAIL") {
            self.acme_email = some(v);
        }
        if let Some(v) = var("AXIOND_ACME_STAGING") {
            self.acme_staging = matches!(v.as_str(), "1" | "true" | "yes");
        }
        if let Some(v) = var("AXIOND_ACME_DIRECTORY") {
            self.acme_directory = some(v);
        }
        if let Some(v) = var("AXIOND_ACME_CA") {
            self.acme_ca = some(v).map(Into::into);
        }
        if let Some(v) = var("AXIOND_TLS_CERT") {
            self.tls_cert = some(v).map(Into::into);
        }
        if let Some(v) = var("AXIOND_TLS_KEY") {
            self.tls_key = some(v).map(Into::into);
        }
        if let Some(v) = var("AXIOND_BACKUP_HOURS") {
            self.backup_hours = v.parse().with_context(|| format!("AXIOND_BACKUP_HOURS={v}"))?;
        }
        if let Some(v) = var("AXIOND_BACKUP_KEEP") {
            self.backup_keep = v.parse().with_context(|| format!("AXIOND_BACKUP_KEEP={v}"))?;
        }
        if let Some(v) = var("AXIOND_CALLS_UDP") {
            self.calls_udp = some(v).map(|v| v.parse().with_context(|| format!("AXIOND_CALLS_UDP={v}"))).transpose()?;
        }
        if let Some(v) = var("AXIOND_CALLS_PUBLIC_IP") {
            self.calls_public_ip = some(v).map(|v| v.parse().with_context(|| format!("AXIOND_CALLS_PUBLIC_IP={v}"))).transpose()?;
        }
        if let Some(v) = var("AXIOND_MAX_FILE_MB") {
            self.max_file_mb = v.parse().with_context(|| format!("AXIOND_MAX_FILE_MB={v}"))?;
        }
        Ok(())
    }

    /// How connections are secured.
    pub fn tls(&self) -> anyhow::Result<Tls> {
        match (&self.domain, &self.tls_cert, &self.tls_key) {
            (_, Some(cert), Some(key)) => Ok(Tls::Files(cert.clone(), key.clone())),
            (_, Some(_), None) | (_, None, Some(_)) => bail!("tls_cert and tls_key go together"),
            (Some(domain), None, None) => Ok(Tls::Acme(domain.clone())),
            (None, None, None) => Ok(Tls::None),
        }
    }

    /// Plain HTTP is only for this machine: passwords and messages would
    /// cross the network readable.
    pub fn check_transport(&self) -> anyhow::Result<()> {
        if matches!(self.tls()?, Tls::None) && !self.listen.ip().is_loopback() && !self.behind_proxy {
            bail!(
                "refusing to serve plain HTTP on {}. Either set domain (AXIOND_DOMAIN) for a \
                 Let's Encrypt certificate, give tls_cert and tls_key, or put a TLS proxy (Caddy, \
                 Traefik, nginx) in front and set behind_proxy = true (AXIOND_BEHIND_PROXY=true). \
                 Or listen on 127.0.0.1.",
                self.listen
            );
        }
        Ok(())
    }
}

/// How axiond secures connections itself.
#[derive(Clone, Debug, PartialEq)]
pub enum Tls {
    /// Plain HTTP: on localhost, or behind a TLS proxy.
    None,
    /// A Let's Encrypt certificate for this domain.
    Acme(String),
    /// Certificate and key PEM files.
    Files(PathBuf, PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_overrides_and_plain_http_stays_local() {
        let mut c = Config::default();
        assert!(c.check_transport().is_ok());
        c.apply_env(|k| (k == "AXIOND_LISTEN").then(|| "0.0.0.0:9000".into())).unwrap();
        assert_eq!(c.listen.port(), 9000);
        assert!(c.check_transport().is_err());
        c.apply_env(|k| (k == "AXIOND_BEHIND_PROXY").then(|| "true".into())).unwrap();
        assert!(c.check_transport().is_ok());
        c.behind_proxy = false;
        c.apply_env(|k| (k == "AXIOND_DOMAIN").then(|| "chat.example.com".into())).unwrap();
        assert!(c.check_transport().is_ok());
        assert_eq!(c.tls().unwrap(), Tls::Acme("chat.example.com".into()));
        c.tls_key = Some("key.pem".into());
        assert!(c.tls().is_err());
        c.tls_cert = Some("cert.pem".into());
        assert_eq!(c.tls().unwrap(), Tls::Files("cert.pem".into(), "key.pem".into()));
        c.apply_env(|k| match k {
            "AXIOND_CALLS_UDP" => Some("0.0.0.0:443".into()),
            "AXIOND_CALLS_PUBLIC_IP" => Some("203.0.113.7".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!((c.calls_udp.unwrap().port(), c.calls_public_ip.unwrap().to_string()), (443, "203.0.113.7".into()));
        assert!(c.apply_env(|k| (k == "AXIOND_CALLS_UDP").then(|| "443".into())).is_err());
    }

    #[test]
    fn unknown_keys_are_an_error() {
        assert!(toml::from_str::<Config>("listn = \"127.0.0.1:1\"").is_err());
        let c: Config = toml::from_str("site_name = \"Home\"").unwrap();
        assert_eq!((c.site_name.as_str(), c.listen.port()), ("Home", 8065));
    }
}
