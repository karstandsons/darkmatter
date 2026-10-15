//! Live check against a real server:
//!
//!   MM_URL=http://localhost:8065 MM_TOKEN=... cargo run -p mm-api --example smoke
//!   MM_URL=... MM_LOGIN=alice MM_PASSWORD=... [MM_MFA=123456] cargo run -p mm-api --example smoke
//!
//! Prints the user, teams and channels, then streams events until Ctrl+C.
//! With MM_UPLOAD=<path>, also posts that file to ~town-square of the first team.

use mm_api::models::ChannelType;
use mm_api::ws::{self, Message};
use mm_api::{Client, PostsQuery};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let env = |k: &str| std::env::var(k).ok();
    let url = env("MM_URL").ok_or("set MM_URL")?;
    let mut client = Client::new(&url)?;
    if let Some(token) = env("MM_TOKEN") {
        client = client.with_token(token);
    } else {
        let login = env("MM_LOGIN").ok_or("set MM_TOKEN, or MM_LOGIN and MM_PASSWORD")?;
        let password = env("MM_PASSWORD").ok_or("set MM_PASSWORD")?;
        client.login(&login, &password, env("MM_MFA").as_deref()).await?;
    }

    let me = client.me().await?;
    println!("logged in as @{} ({})", me.username, me.display_name());

    for team in client.my_teams().await? {
        println!("\nteam {} ({})", team.display_name, team.name);
        let channels = client.my_channels(&team.id).await?;
        let members = client.my_channel_members(&team.id).await?;
        for ch in &channels {
            let unread = members.iter().find(|m| m.channel_id == ch.id).map_or(0, |m| m.unread_count(ch));
            let name = match ch.kind {
                ChannelType::Direct => format!("@{}", ch.dm_partner(&me.id).unwrap_or("?")),
                _ => format!("~{}", ch.name),
            };
            println!("  {name:<40} {:>4} unread", unread);
        }
        if let (Some(path), Some(ch)) = (env("MM_UPLOAD"), channels.iter().find(|c| c.name == "town-square")) {
            let name = std::path::Path::new(&path).file_name().unwrap().to_string_lossy().into_owned();
            let up = client.upload_file(&ch.id, &name, std::fs::read(&path)?).await?;
            let file_ids: Vec<_> = up.file_infos.iter().map(|f| f.id.clone()).collect();
            let post = mm_api::models::NewPost {
                channel_id: ch.id.clone(),
                message: format!("uploaded {name}"),
                file_ids,
                ..Default::default()
            };
            let p = client.create_post(&post).await?;
            println!("  uploaded {name} -> post {} with {} file(s)", p.id, p.file_ids.len());
        }
        if let Some(ch) = channels.iter().find(|c| c.name == "town-square") {
            let posts = client.posts(&ch.id, PostsQuery::Page { page: 0, per_page: 5 }, true).await?;
            println!("  last posts in ~town-square:");
            for p in posts.chronological() {
                println!("    {}", p.message.lines().next().unwrap_or_default());
            }
        }
    }

    println!("\nstreaming events (Ctrl+C to stop)…");
    let mut stream = ws::connect(&client)?;
    while let Some(msg) = stream.next().await {
        match msg {
            Message::Event(ev) => println!("{ev:?}"),
            other => println!("-- {other:?}"),
        }
    }
    Ok(())
}
