//! Darkmatter's client against a real Mattermost, so a Mattermost release
//! that would break Darkmatter shows up in CI rather than for its users.
//!
//! Runs only with `MM_TEST_URL` set to a throwaway server (it makes users,
//! a team and posts), e.g.:
//!
//!   docker run -d -p 127.0.0.1:8067:8065 mattermost/mattermost-preview:11.7.12
//!   MM_TEST_URL=http://127.0.0.1:8067 cargo test -p mm-api --test mattermost
//!
//! Never point it at a real server.

use std::time::Duration;

use mm_api::models::NewPost;
use mm_api::ws::{self, Event, Message};
use mm_api::{Client, PostsQuery};
use serde_json::{Value, json};

const PASSWORD: &str = "Darkmatter-test-1!";

fn server() -> Option<String> {
    let url = std::env::var("MM_TEST_URL").ok()?;
    let host = url::Url::parse(&url).ok()?.host_str()?.to_string();
    // A guard against pointing it at a real server by mistake.
    assert!(matches!(host.as_str(), "127.0.0.1" | "localhost" | "mattermost"), "MM_TEST_URL must be a local throwaway server, not {host}");
    Some(url)
}

/// Raw API calls the client doesn't need (setting up users and teams).
async fn raw(url: &str, token: Option<&str>, method: reqwest::Method, path: &str, body: Value) -> Value {
    let mut req = reqwest::Client::new().request(method, format!("{url}/api/v4/{path}")).json(&body);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.unwrap();
    let status = resp.status();
    let value: Value = resp.json().await.unwrap_or(Value::Null);
    assert!(status.is_success() || status.as_u16() == 400, "{path}: {status} {value}");
    value
}

async fn login(url: &str, name: &str) -> Client {
    let mut c = Client::new(url).unwrap();
    c.login(name, PASSWORD, None).await.unwrap();
    c
}

/// The next event that's about chat, skipping presence and connection notices.
async fn next_event(stream: &mut ws::EventStream, want: impl Fn(&Event) -> bool) -> Event {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, stream.next()).await.expect("the event in time").expect("stream open") {
            Message::Event(e) if want(&e) => return e,
            _ => continue,
        }
    }
}

#[tokio::test]
async fn darkmatter_against_mattermost() {
    let Some(url) = server() else {
        eprintln!("skipped: set MM_TEST_URL to a throwaway Mattermost to run");
        return;
    };
    // Names unique to this run, so the same server can be reused.
    let run = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() % 0xffffff);
    let (alice_name, bob_name, team_name) = (format!("alice{run}"), format!("bob{run}"), format!("dm{run}"));

    // The first account on a fresh server is its admin; it makes this run's
    // people (later runs reuse it).
    let mut admin = Client::new(&url).unwrap();
    if admin.login("dmadmin", PASSWORD, None).await.is_err() {
        raw(&url, None, reqwest::Method::POST, "users", json!({ "username": "dmadmin", "email": "dmadmin@example.com", "password": PASSWORD })).await;
        admin.login("dmadmin", PASSWORD, None).await.expect("dmadmin is the server's first account");
    }
    let admin_token = admin.token().unwrap().to_string();
    for name in [&alice_name, &bob_name] {
        raw(&url, Some(&admin_token), reqwest::Method::POST, "users", json!({ "username": name, "email": format!("{name}@example.com"), "password": PASSWORD })).await;
    }
    let alice = login(&url, &alice_name).await;
    let bob = login(&url, &bob_name).await;
    let (me, bob_me) = (alice.me().await.unwrap(), bob.me().await.unwrap());
    let token = alice.token().unwrap().to_string();
    // Alice makes the team, so she's its admin.
    let team = raw(&url, Some(&token), reqwest::Method::POST, "teams", json!({ "name": team_name, "display_name": "Darkmatter test", "type": "O" })).await;
    let team_id = team["id"].as_str().unwrap().to_string();
    raw(&url, Some(&token), reqwest::Method::POST, &format!("teams/{team_id}/members"), json!({ "team_id": team_id, "user_id": bob_me.id })).await;
    let channel = raw(
        &url,
        Some(&token),
        reqwest::Method::POST,
        "channels",
        json!({ "team_id": team_id, "name": "rust", "display_name": "Rust", "type": "O" }),
    )
    .await;
    let rust = channel["id"].as_str().unwrap().to_string();
    bob.join_channel(&rust, &bob_me.id).await.unwrap();

    // What Darkmatter loads on start.
    assert!(alice.axiond().await.unwrap().is_none(), "plain Mattermost isn't axiond");
    assert!(alice.client_config().await.unwrap().contains_key("Version"));
    assert!(alice.my_teams().await.unwrap().iter().any(|t| t.id == team_id));
    assert!(alice.my_channels(&team_id).await.unwrap().iter().any(|c| c.id == rust));
    assert!(!alice.my_channel_members(&team_id).await.unwrap().is_empty());
    alice.user_threads(&team_id).await.unwrap();
    alice.custom_emoji(0, 200).await.unwrap();
    assert!(!alice.sessions(&me.id).await.unwrap().is_empty());

    // Bob listens; Alice mentions him.
    let mut events = ws::connect(&bob).unwrap();
    next_event(&mut events, |e| matches!(e, Event::Hello { .. })).await;
    let post = alice
        .create_post(&NewPost { channel_id: rust.clone(), message: format!("hi @{bob_name}"), pending_post_id: format!("{}:1", me.id), ..Default::default() })
        .await
        .unwrap();
    match next_event(&mut events, |e| matches!(e, Event::Posted { .. })).await {
        Event::Posted { post: p, mentions, .. } => {
            assert_eq!(p.id, post.id);
            assert_eq!(mentions, std::slice::from_ref(&bob_me.id));
        }
        _ => unreachable!(),
    }

    // Replies, threads, edits, reactions.
    let reply = bob.create_post(&NewPost { channel_id: rust.clone(), root_id: post.id.clone(), message: "hello".into(), ..Default::default() }).await.unwrap();
    let thread = alice.thread(&reply.id).await.unwrap();
    assert_eq!(thread.posts.len(), 2);
    let roots = alice.posts(&rust, PostsQuery::Page { page: 0, per_page: 30 }, true).await.unwrap();
    assert_eq!(roots.posts[&post.id].reply_count, 1);
    alice.edit_post(&post.id, "hi, edited").await.unwrap();
    next_event(&mut events, |e| matches!(e, Event::PostEdited(p) if p.message == "hi, edited")).await;
    bob.add_reaction(&bob_me.id, &post.id, "+1").await.unwrap();
    let thread = alice.thread(&post.id).await.unwrap();
    assert_eq!(thread.posts[&post.id].metadata.reactions.len(), 1);
    bob.remove_reaction(&bob_me.id, &post.id, "+1").await.unwrap();

    // Files and pictures.
    let png = {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8 * 4, y as u8 * 5, 128])).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    };
    let up = alice.upload_file(&rust, "pic.png", png.clone()).await.unwrap();
    let file = &up.file_infos[0];
    alice.create_post(&NewPost { channel_id: rust.clone(), message: "a picture".into(), file_ids: vec![file.id.clone()], ..Default::default() }).await.unwrap();
    assert_eq!(bob.file(&file.id).await.unwrap(), png);
    assert!(!bob.file_preview(&file.id).await.unwrap().is_empty());

    // Search, unread, statuses, people, commands, direct messages.
    let found = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let hits = bob.search_posts(&team_id, "edited").await.unwrap();
            if hits.posts.contains_key(&post.id) {
                return hits;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .expect("search finds the post");
    assert!(!found.order.is_empty());
    bob.view_channel(&rust, None).await.unwrap();
    let unread = bob.set_unread(&bob_me.id, &post.id).await.unwrap();
    let total = bob.channel(&rust).await.unwrap().total_msg_count;
    assert!(unread.msg_count < total, "unread from the post on");
    assert_eq!(alice.statuses_by_ids(std::slice::from_ref(&bob_me.id)).await.unwrap().len(), 1);
    assert!(alice.autocomplete_users(&team_id, &bob_name[..4]).await.unwrap().iter().any(|u| u.id == bob_me.id));
    alice.execute_command(&team_id, &rust, "", "/shrug").await.unwrap();
    assert!(!alice.command_suggestions(&team_id, &rust, "/shr").await.unwrap().is_empty());
    let dm = alice.create_direct(&me.id, &bob_me.id).await.unwrap();
    alice.create_post(&NewPost { channel_id: dm.id.clone(), message: "psst".into(), ..Default::default() }).await.unwrap();
    assert!(bob.search_channels(&team_id, "rus").await.unwrap().iter().any(|c| c.id == rust));

    // Leaving and deleting.
    bob.leave_channel(&rust, &bob_me.id).await.unwrap();
    alice.delete_post(&reply.id).await.unwrap();
}
