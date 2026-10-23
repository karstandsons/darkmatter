//! A server with a lot of history: pages and searches must cost what they
//! return, not what the server holds. Every post carries its thread's
//! `reply_count`, and that once read the whole posts table per post returned
//! (a search over 27k matches took 162 s of CPU).

use std::time::{Duration, Instant};

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use mm_api::{Client, PostsQuery};

const PASSWORD: &str = "correct horse battery";
const POSTS: i64 = 50_000;

/// Far above what these take now (milliseconds) and far below what they took
/// with the scan, even in a debug build.
const FAST: Duration = Duration::from_secs(2);

async fn timed<T>(what: &str, f: impl Future<Output = T>) -> T {
    let start = Instant::now();
    let out = tokio::time::timeout(FAST * 5, f).await.unwrap_or_else(|_| panic!("{what} took over {:?}", FAST * 5));
    let took = start.elapsed();
    eprintln!("{what}: {took:?}");
    assert!(took < FAST, "{what} took {took:?}");
    out
}

#[tokio::test]
async fn pages_and_search_stay_fast_with_50k_posts() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::open(Config { data_dir: dir.path().into(), ..Config::default() }).await.unwrap();
    let user = |name: &str| NewUser { username: name.into(), email: format!("{name}@example.com"), password: PASSWORD.into() };
    admin::setup(&app, user("alice"), "home", "Home").await.unwrap();
    admin::add_channel(&app, "home", "rust", "Rust").await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(app.clone().serve(listener));

    let mut alice = Client::new(&url).unwrap();
    alice.login("alice", PASSWORD, None).await.unwrap();
    let me = alice.me().await.unwrap().id;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let rust = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "rust").unwrap().id;
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;

    // 50k posts in #rust, every fifth one a reply to the post before it.
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i + 1 < ?3) \
         INSERT INTO posts (id, channel_id, user_id, root_id, create_at, update_at, message) \
         SELECT printf('post%022d', i), ?1, ?2, \
                CASE WHEN i % 5 = 0 AND i > 0 THEN printf('post%022d', i - 1) ELSE '' END, \
                1000000 + i, 1000000 + i, 'the quick fox learns rust ' || i FROM n",
    )
    .bind(&rust)
    .bind(&me)
    .bind(POSTS)
    .execute(&app.db)
    .await
    .unwrap();
    alice.create_post(&mm_api::models::NewPost { channel_id: town.clone(), message: "hello".into(), ..Default::default() }).await.unwrap();

    let newest = timed("a page of 60", alice.posts(&rust, PostsQuery::Page { page: 0, per_page: 60 }, false)).await.unwrap();
    assert_eq!(newest.order.len(), 60);
    // 49995 replies to 49994; 49999 is on its own.
    let root = &newest.posts[&format!("post{:022}", 49_994)];
    assert_eq!(root.reply_count, 1);
    let reply = &newest.posts[&format!("post{:022}", 49_995)];
    assert_eq!(reply.reply_count, 1, "a reply carries its thread's count");
    assert_eq!(newest.posts[&format!("post{:022}", 49_999)].reply_count, 0);

    timed("a page in a small channel", alice.posts(&town, PostsQuery::Page { page: 0, per_page: 60 }, false)).await.unwrap();
    timed("an older page", alice.posts(&rust, PostsQuery::Page { page: 400, per_page: 60 }, true)).await.unwrap();

    // Every post matches; only a page of them comes back.
    let found = timed("searching 50k matches", alice.search_posts(&team, "fox rust")).await.unwrap();
    assert_eq!(found.order.len(), 40);
    assert_eq!(found.order[0], format!("post{:022}", 49_999));
    assert_eq!(found.posts[&format!("post{:022}", 49_995)].reply_count, 1);
}
