//! The admin socket: `axiond user deactivate` and the like, run by the
//! running server rather than the command's own process, so they take
//! effect at once. Doing them in the command's process would change the
//! database but not the server's memory: open sockets, calls, and the
//! events everyone else's apps get.
//!
//! It's `admin.sock` in the data folder (which only axiond's user can
//! open), and it only answers that user or root. One line of JSON in, one
//! out. With no server running, the command does the job itself.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{App, admin};

/// What the command asks the server to do.
#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Request {
    SetActive { username: String, active: bool },
    Delete { username: String, delete_messages: bool },
    Password { username: String, password: String },
    MfaOff { username: String },
}

impl Request {
    /// For the log (never the password).
    fn describe(&self) -> String {
        match self {
            Request::SetActive { username, active: true } => format!("activate {username}"),
            Request::SetActive { username, active: false } => format!("deactivate {username}"),
            Request::Delete { username, .. } => format!("delete {username}"),
            Request::Password { username, .. } => format!("password {username}"),
            Request::MfaOff { username } => format!("mfa-off {username}"),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Reply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join("admin.sock")
}

/// A name the socket can be bound or reached by. Socket paths are limited
/// to about 100 bytes, so a deeper data folder is reached through its open
/// directory on Linux (`/proc/self/fd/N/admin.sock`); elsewhere it gets no
/// socket.
#[cfg(unix)]
struct Address {
    path: PathBuf,
    _dir: Option<std::fs::File>,
}

#[cfg(unix)]
impl Address {
    fn of(data_dir: &Path) -> Address {
        let path = socket_path(data_dir);
        if path.as_os_str().len() < 100 {
            return Address { path, _dir: None };
        }
        #[cfg(target_os = "linux")]
        if let Ok(dir) = std::fs::File::open(data_dir) {
            use std::os::fd::AsRawFd;
            return Address { path: format!("/proc/self/fd/{}/admin.sock", dir.as_raw_fd()).into(), _dir: Some(dir) };
        }
        Address { path, _dir: None }
    }
}

/// Does `request`: through the running server if there is one, here if not.
pub async fn apply(config: crate::Config, request: Request) -> anyhow::Result<()> {
    if ask(&config.data_dir, &request).await? {
        return Ok(());
    }
    run(&App::open(config).await?, request).await
}

async fn run(app: &App, request: Request) -> anyhow::Result<()> {
    match request {
        Request::SetActive { username, active } => admin::set_active(app, &username, active).await,
        Request::Delete { username, delete_messages } => admin::delete_user(app, &username, delete_messages).await,
        Request::Password { username, password } => admin::set_password(app, &username, &password).await,
        Request::MfaOff { username } => admin::mfa_off(app, &username).await,
    }
}

/// Asks the running server. False: none is running.
#[cfg(unix)]
async fn ask(data_dir: &Path, request: &Request) -> anyhow::Result<bool> {
    use anyhow::Context;
    use std::io::ErrorKind;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let path = socket_path(data_dir);
    let mut stream = match tokio::net::UnixStream::connect(&Address::of(data_dir).path).await {
        Ok(s) => s,
        // No socket, or one a stopped server left behind, or a path too
        // long for the server to have made one.
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::InvalidInput) => return Ok(false),
        // Doing it here would miss the running server's open connections.
        Err(e) => {
            return Err(e).with_context(|| format!("reaching the running server at {} (run this as the user axiond runs as)", path.display()));
        }
    };
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    stream.write_all(&line).await?;
    stream.shutdown().await?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).await?;
    let reply: Reply = serde_json::from_slice(&answer).context("the running server didn't answer (is it stopping?)")?;
    match reply.error {
        Some(e) => anyhow::bail!("{e}"),
        None => Ok(true),
    }
}

#[cfg(not(unix))]
async fn ask(_data_dir: &Path, _request: &Request) -> anyhow::Result<bool> {
    Ok(false)
}

/// Listens on the admin socket until the server stops. Without one (an
/// unusable path, say) admin commands still work, and the session watch
/// (`housekeeping::watch_sessions`) closes what they end within seconds.
#[cfg(unix)]
pub fn listen(app: &App) -> anyhow::Result<Listening> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let path = socket_path(&app.config.data_dir);
    let address = Address::of(&app.config.data_dir);
    if std::os::unix::net::UnixStream::connect(&address.path).is_ok() {
        anyhow::bail!("another axiond is already running on {}", app.config.data_dir.display());
    }
    let _ = std::fs::remove_file(&path);
    let listener = match tokio::net::UnixListener::bind(&address.path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("no admin socket at {}: {e}; admin commands take a few seconds to close connections", path.display());
            return Ok(Listening(None));
        }
    };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    // The socket is ours, so its owner is who we run as.
    let owner = std::fs::metadata(&path)?.uid();
    let app = app.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                // Out of file handles, say: don't spin.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            };
            match stream.peer_cred() {
                Ok(c) if c.uid() == owner || c.uid() == 0 => {}
                _ => continue,
            }
            let app = app.clone();
            tokio::spawn(async move { answer(&app, stream).await });
        }
    });
    Ok(Listening(Some(path)))
}

#[cfg(not(unix))]
pub fn listen(_app: &App) -> anyhow::Result<Listening> {
    Ok(Listening(None))
}

/// The socket's file, removed when the server stops.
pub struct Listening(Option<PathBuf>);

impl Drop for Listening {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(unix)]
async fn answer(app: &App, stream: tokio::net::UnixStream) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    let mut read = BufReader::new(read.take(64 * 1024));
    let read = read.read_line(&mut line);
    let reply = match tokio::time::timeout(std::time::Duration::from_secs(10), read).await {
        Ok(Ok(_)) => match serde_json::from_str::<Request>(&line) {
            Ok(request) => {
                let what = request.describe();
                match run(app, request).await {
                    Ok(()) => {
                        tracing::info!("admin command: {what}");
                        Reply { error: None }
                    }
                    Err(e) => Reply { error: Some(format!("{e:#}")) },
                }
            }
            Err(e) => Reply { error: Some(format!("not a request this server knows ({e}); is it older than this command?")) },
        },
        _ => return,
    };
    let _ = write.write_all(&serde_json::to_vec(&reply).unwrap_or_default()).await;
    let _ = write.shutdown().await;
}
