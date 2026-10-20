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
    /// Check the running server answers (for health checks).
    Ping,
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
        Command::Ping => ping(config.listen).await,
        Command::User(UserCommand::Picture { username, file }) => {
            let app = App::open(config).await?;
            let bytes = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            admin::set_picture(&app, &username, bytes).await?;
            println!("Changed {username}'s picture.");
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

/// `GET /api/v4/system/ping` on the server's own port; fails unless it says 200.
async fn ping(mut addr: std::net::SocketAddr) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if addr.ip().is_unspecified() {
        addr.set_ip(std::net::Ipv4Addr::LOCALHOST.into());
    }
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut stream = tokio::net::TcpStream::connect(addr).await?;
        stream.write_all(b"GET /api/v4/system/ping HTTP/1.0\r\nHost: localhost\r\n\r\n").await?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await?;
        anyhow::Ok(reply)
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
