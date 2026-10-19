//! `axiond.toml`, with `AXIOND_*` environment variables on top.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Where to listen. Anything but localhost needs `behind_proxy` until
    /// axiond serves TLS itself.
    pub listen: SocketAddr,
    /// The database and, later, uploaded files.
    pub data_dir: PathBuf,
    /// A TLS proxy (Caddy, Traefik, nginx) is in front.
    pub behind_proxy: bool,
    /// The name apps show for this server.
    pub site_name: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: SocketAddr::from(([127, 0, 0, 1], 8065)),
            data_dir: PathBuf::from("data"),
            behind_proxy: false,
            site_name: "axiond".into(),
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
        Ok(())
    }

    /// Plain HTTP is only for this machine: passwords and messages would
    /// cross the network readable.
    pub fn check_transport(&self) -> anyhow::Result<()> {
        if !self.listen.ip().is_loopback() && !self.behind_proxy {
            bail!(
                "refusing to serve plain HTTP on {}: axiond doesn't do TLS yet. Put a TLS proxy \
                 (Caddy, Traefik, nginx) in front and set behind_proxy = true \
                 (AXIOND_BEHIND_PROXY=true), or listen on 127.0.0.1.",
                self.listen
            );
        }
        Ok(())
    }
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
    }

    #[test]
    fn unknown_keys_are_an_error() {
        assert!(toml::from_str::<Config>("listn = \"127.0.0.1:1\"").is_err());
        let c: Config = toml::from_str("site_name = \"Home\"").unwrap();
        assert_eq!((c.site_name.as_str(), c.listen.port()), ("Home", 8065));
    }
}
