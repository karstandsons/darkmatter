//! A long-running test: several people post as fast as the rate limits allow
//! while the server is killed mid-write and restarted, over and over. Every
//! client retries like Darkmatter's outbox (same pending id until the server
//! answers). At the end, every message must be there exactly once.
//!
//!   SOAK_SECS=600 cargo test -p axiond --release --test soak -- --ignored --nocapture

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use mm_api::models::NewPost;
use mm_api::{Client, PostsQuery};
use tokio::sync::Mutex;

const PASSWORD: &str = "correct horse battery";
const PEOPLE: usize = 5;

/// The real binary, so a restart is a real crash: SIGKILL, mid-write.
async fn start(data: &std::path::Path, addr: std::net::SocketAddr) -> tokio::process::Child {
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_axiond"))
        .env("AXIOND_DATA_DIR", data)
        .env("AXIOND_LISTEN", addr.to_string())
        .env("AXIOND_BACKUP_HOURS", "0")
        .env("AXIOND_LOG", "axiond=warn")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    // Up once it answers.
    let ping = format!("http://{addr}/api/v4/system/ping");
    for _ in 0..200 {
        if reqwest::get(&ping).await.is_ok_and(|r| r.status().is_success()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    child
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "long; run by hand or nightly with SOAK_SECS"]
async fn messages_survive_crashes() {
    let secs: u64 = std::env::var("SOAK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().to_path_buf();
    {
        let app = App::open(Config { data_dir: data.clone(), ..Config::default() }).await.unwrap();
        let user = |name: String| NewUser { email: format!("{name}@example.com"), username: name, password: PASSWORD.into() };
        admin::setup(&app, user("person0".into()), "home", "Home").await.unwrap();
        for i in 1..PEOPLE {
            admin::add_user(&app, user(format!("person{i}")), false).await.unwrap();
        }
        app.db.close().await;
    }
    let addr: std::net::SocketAddr = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let url = format!("http://{addr}");
    let server = Arc::new(Mutex::new(start(&data, addr).await));
    let done = Arc::new(AtomicBool::new(false));

    // Each person posts numbered messages, retrying each until it's in.
    let mut people = Vec::new();
    for i in 0..PEOPLE {
        let (url, done) = (url.clone(), done.clone());
        people.push(tokio::spawn(async move {
            let client = loop {
                let mut c = Client::new(&url).unwrap();
                if c.login(&format!("person{i}"), PASSWORD, None).await.is_ok() {
                    break c;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            };
            let team = client.my_teams().await.unwrap()[0].id.clone();
            let town = client.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
            let mut sent = 0u32;
            let mut retries = 0u32;
            while !done.load(Ordering::Relaxed) {
                let post = NewPost { channel_id: town.clone(), message: format!("p{i} #{sent}"), pending_post_id: format!("p{i}:{sent}"), ..Default::default() };
                loop {
                    match client.create_post(&post).await {
                        Ok(_) => break,
                        Err(_) => {
                            retries += 1;
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
                sent += 1;
                // About the posting rate limit: 5 a second.
                tokio::time::sleep(Duration::from_millis(210)).await;
            }
            (sent, retries, town)
        }));
    }

    // Kill and restart the server every few seconds, mid-write.
    let start_at = Instant::now();
    let mut restarts = 0;
    while start_at.elapsed() < Duration::from_secs(secs) {
        tokio::time::sleep(Duration::from_millis(1500 + (restarts % 7) * 300)).await;
        let mut s = server.lock().await;
        s.kill().await.unwrap();
        *s = start(&data, addr).await;
        restarts += 1;
    }
    done.store(true, Ordering::Relaxed);
    let mut sent = HashMap::new();
    let mut town = String::new();
    let mut retries = 0;
    for (i, p) in people.into_iter().enumerate() {
        let (n, r, t) = p.await.unwrap();
        sent.insert(i, n);
        retries += r;
        town = t;
    }

    // Every message exactly once, in each person's order.
    let mut reader = Client::new(&url).unwrap();
    reader.login("person0", PASSWORD, None).await.unwrap();
    let mut all = Vec::new();
    let mut page = 0;
    loop {
        let list = reader.posts(&town, PostsQuery::Page { page, per_page: 200 }, false).await.unwrap();
        if list.order.is_empty() {
            break;
        }
        all.extend(list.posts.into_values().filter(|p| !p.is_system()).map(|p| (p.create_at, p.id, p.message)));
        page += 1;
    }
    all.sort();
    let all: Vec<String> = all.into_iter().map(|(_, _, m)| m).collect();
    let total: u32 = sent.values().sum();
    println!("{secs} s: {restarts} crashes, {total} messages, {retries} retries");
    assert_eq!(all.len(), total as usize, "no message lost or doubled");
    for (i, n) in sent {
        let mine: Vec<&String> = all.iter().filter(|m| m.starts_with(&format!("p{i} #"))).collect();
        assert_eq!(mine.len(), n as usize, "p{i}");
        for (k, m) in mine.iter().enumerate() {
            assert_eq!(**m, format!("p{i} #{k}"), "p{i}'s messages in order");
        }
    }
    server.lock().await.kill().await.unwrap();
}
