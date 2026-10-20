use std::path::PathBuf;

use anyhow::{Context, bail};
use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

/// axiond, the Darkmatter chat server.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Settings file (default: axiond.toml here, if there is one).
    #[arg(long, global = true, env = "AXIOND_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the server (what plain `axiond` does).
    Serve,
    /// Set up a new server: the first admin and their team.
    Setup {
        /// The admin's username.
        #[arg(long)]
        admin: String,
        #[arg(long)]
        email: String,
        /// The team's URL name, e.g. "home".
        #[arg(long)]
        team: String,
        /// The team's name as shown (default: the URL name).
        #[arg(long)]
        team_name: Option<String>,
    },
    /// Manage people.
    #[command(subcommand)]
    User(UserCommand),
    /// Manage channels.
    #[command(subcommand)]
    Channel(ChannelCommand),
    /// Manage custom emoji.
    #[command(subcommand)]
    Emoji(EmojiCommand),
    /// Make an invite code someone can sign up with.
    Invite {
        /// How many people can use it.
        #[arg(long, default_value_t = 1)]
        uses: i64,
        /// How many days it works for.
        #[arg(long, default_value_t = 7)]
        days: i64,
        /// A note for yourself, e.g. who it's for.
        #[arg(long, default_value = "")]
        note: String,
    },
    /// List reported messages and people that no one has dealt with yet.
    Reports,
    /// Show what admins did, newest first.
    Audit {
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Check the running server answers (for health checks).
    Ping,
    /// Back up the database and stored files to a .tar.gz (safe while running).
    Backup {
        /// Where to write it (default: the data folder's `backups`).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Restore a backup into an empty data folder (with axiond stopped).
    Restore { file: PathBuf },
}

#[derive(Subcommand)]
enum UserCommand {
    /// Add someone to the server and every team.
    Add {
        username: String,
        #[arg(long)]
        email: String,
        /// Make them an admin.
        #[arg(long)]
        admin: bool,
    },
    /// Set someone's password (they're signed out everywhere).
    Password { username: String },
    /// Set someone's profile picture (PNG, JPEG, GIF or WebP).
    Picture { username: String, file: PathBuf },
    /// Turn off someone's two-factor codes, if they lost their phone (they're signed out everywhere).
    MfaOff { username: String },
    /// Stop someone signing in (they're signed out everywhere). Undo with `activate`.
    Deactivate { username: String },
    /// Let a deactivated person sign in again.
    Activate { username: String },
    /// Delete someone's account for good: profile, picture and memberships.
    Delete {
        username: String,
        /// Delete their messages too, not just their name on them.
        #[arg(long)]
        delete_messages: bool,
    },
}

#[derive(Subcommand)]
enum EmojiCommand {
    /// Add a custom emoji, used as :name: (a picture of at most 1 MB).
    Add { name: String, file: PathBuf },
}

#[derive(Subcommand)]
enum ChannelCommand {
    /// Add a public channel to a team, with everyone in the team in it.
    Add {
        /// The team's URL name.
        team: String,
        /// The channel's URL name, e.g. "random".
        name: String,
        /// The name as shown (default: the URL name).
        #[arg(long)]
        display_name: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_env("AXIOND_LOG").unwrap_or_else(|_| EnvFilter::new("axiond=info")))
        // Plain text in `docker logs` and journald.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();
    let cli = Cli::parse();
    let config = Config::load(cli.config.as_deref())?;

    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            config.check_transport()?;
            let listener = tokio::net::TcpListener::bind(config.listen).await.with_context(|| format!("listening on {}", config.listen))?;
            let app = App::open(config).await?;
            let users: i64 = sqlx_count(&app).await?;
            if users == 0 {
                tracing::warn!("no one can log in yet: run `axiond setup` to make the first admin");
            }
            tracing::info!("axiond {} listening on http://{}", axiond::VERSION, listener.local_addr()?);
            app.serve(listener).await
        }
        Command::Setup { admin, email, team, team_name } => {
            let app = App::open(config).await?;
            let password = new_password(&admin)?;
            let display = team_name.unwrap_or_else(|| team.clone());
            admin::setup(&app, NewUser { username: admin.clone(), email, password }, &team, &display).await?;
            println!("Set up team {display} with admin {admin}. Start the server with `axiond`.");
            Ok(())
        }
        Command::User(UserCommand::Add { username, email, admin }) => {
            let app = App::open(config).await?;
            let password = new_password(&username)?;
            admin::add_user(&app, NewUser { username: username.clone(), email, password }, admin).await?;
            println!("Added {username}.");
            Ok(())
        }
        Command::User(UserCommand::Password { username }) => {
            let app = App::open(config).await?;
            let password = new_password(&username)?;
            admin::set_password(&app, &username, &password).await?;
            println!("Changed {username}'s password and signed them out everywhere.");
            Ok(())
        }
        Command::Ping => ping(&config).await,
        Command::Backup { out } => {
            let out = out.unwrap_or_else(|| config.data_dir.join("backups"));
            let app = App::open(config).await?;
            let path = axiond::backup::create(&app.db, &app.config.data_dir, &out).await?;
            println!("Backed up to {}", path.display());
            Ok(())
        }
        Command::Restore { file } => {
            axiond::backup::restore(&file, &config.data_dir)?;
            // Opening it checks it and brings an older backup up to date.
            App::open(config).await?;
            println!("Restored. Start the server with `axiond`.");
            Ok(())
        }
        Command::User(UserCommand::Picture { username, file }) => {
            let app = App::open(config).await?;
            let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            admin::set_picture(&app, &username, bytes).await?;
            println!("Changed {username}'s picture.");
            Ok(())
        }
        Command::User(UserCommand::MfaOff { username }) => {
            let app = App::open(config).await?;
            admin::mfa_off(&app, &username).await?;
            println!("Turned off {username}'s two-factor codes and signed them out everywhere.");
            Ok(())
        }
        Command::User(UserCommand::Deactivate { username }) => {
            admin::set_active(&App::open(config).await?, &username, false).await?;
            println!("Deactivated {username}.");
            Ok(())
        }
        Command::User(UserCommand::Activate { username }) => {
            admin::set_active(&App::open(config).await?, &username, true).await?;
            println!("{username} can sign in again.");
            Ok(())
        }
        Command::User(UserCommand::Delete { username, delete_messages }) => {
            admin::delete_user(&App::open(config).await?, &username, delete_messages).await?;
            println!("Deleted {username}'s account{}.", if delete_messages { " and messages" } else { "" });
            Ok(())
        }
        Command::Invite { uses, days, note } => {
            let (code, _) = admin::invite(&App::open(config).await?, uses, days, &note).await?;
            println!("Invite code (works {uses} time{}, for {days} day{}):\n\n    {code}\n", if uses == 1 { "" } else { "s" }, if days == 1 { "" } else { "s" });
            println!("It's shown only now. In Darkmatter, choose \"I have an invite\" on the sign-in screen.");
            Ok(())
        }
        Command::Reports => {
            let app = App::open(config).await?;
            let reports = admin::open_reports(&app).await?;
            if reports.is_empty() {
                println!("No open reports.");
            }
            for r in reports {
                println!("{r}");
            }
            Ok(())
        }
        Command::Audit { limit } => {
            let app = App::open(config).await?;
            for e in axiond::audit::recent(&app.db, limit).await? {
                let who = if e.actor_id.is_empty() { "(command line)".to_string() } else { format!("@{}", e.actor) };
                println!("{}  {who:<20} {:<18} {} {}", e.create_at, e.action, e.target_id, e.detail);
            }
            Ok(())
        }
        Command::Emoji(EmojiCommand::Add { name, file }) => {
            let app = App::open(config).await?;
            let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            admin::add_emoji(&app, &name, &bytes).await?;
            println!("Added :{name}:.");
            Ok(())
        }
        Command::Channel(ChannelCommand::Add { team, name, display_name }) => {
            let app = App::open(config).await?;
            admin::add_channel(&app, &team, &name, display_name.as_deref().unwrap_or(&name)).await?;
            println!("Added ~{name} to {team}.");
            Ok(())
        }
    }
}

/// `GET /api/v4/system/ping` on the server's own port (over HTTPS if it
/// serves HTTPS, checking the certificate); fails unless it says 200.
async fn ping(config: &Config) -> anyhow::Result<()> {
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
    async fn ask<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, host: &str) -> anyhow::Result<String> {
        stream.write_all(format!("GET /api/v4/system/ping HTTP/1.0\r\nHost: {host}\r\n\r\n").as_bytes()).await?;
        let mut reply = Vec::new();
        // A TLS server may close without a goodbye; what arrived counts.
        let _ = stream.read_to_end(&mut reply).await;
        Ok(String::from_utf8_lossy(&reply).into_owned())
    }
    let mut addr = config.listen;
    if addr.ip().is_unspecified() {
        addr.set_ip(std::net::Ipv4Addr::LOCALHOST.into());
    }
    let tls = config.tls()?;
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let stream = tokio::net::TcpStream::connect(addr).await?;
        let Some(host) = (match &tls {
            axiond::config::Tls::None => None,
            _ => Some(config.domain.clone().unwrap_or_else(|| "localhost".into())),
        }) else {
            return ask(stream, "localhost").await;
        };
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        // Our own certificate files are trusted as given (they may be self-signed).
        if let axiond::config::Tls::Files(cert, _) = &tls {
            use rustls_pki_types::pem::PemObject;
            for c in rustls_pki_types::CertificateDer::pem_file_iter(cert)? {
                roots.add(c?)?;
            }
        }
        let client = rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
        let name = rustls_pki_types::ServerName::try_from(host.clone())?;
        let stream = tokio_rustls::TlsConnector::from(std::sync::Arc::new(client)).connect(name, stream).await?;
        ask(stream, &host).await
    })
    .await??;
    if !reply.starts_with("HTTP/1.0 200") && !reply.starts_with("HTTP/1.1 200") {
        bail!("{addr} answered: {}", reply.lines().next().unwrap_or_default());
    }
    println!("ok");
    Ok(())
}

async fn sqlx_count(app: &App) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&app.db).await?)
}

/// From `AXIOND_PASSWORD` (for scripts), or asked for twice.
fn new_password(username: &str) -> anyhow::Result<String> {
    if let Ok(p) = std::env::var("AXIOND_PASSWORD") {
        return Ok(p);
    }
    let first = rpassword::prompt_password(format!("Password for {username}: "))?;
    let again = rpassword::prompt_password("Again: ")?;
    if first != again {
        bail!("the passwords didn't match");
    }
    Ok(first)
}
