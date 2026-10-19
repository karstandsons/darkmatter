//! Darkmatter's own API client (`mm-api`) against a real axiond: what the
//! app does from logging in to chatting, in the order it does it.

use std::time::Duration;

use axiond::admin::{self, NewUser};
use axiond::{App, Config};
use mm_api::models::{ChannelType, NewPost};
use mm_api::ws::{self, Event, Message};
use mm_api::{Client, PostsQuery};

const PASSWORD: &str = "correct horse battery";

struct Server {
    url: String,
    _dir: tempfile::TempDir,
}

async fn server() -> Server {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let dir = tempfile::tempdir().unwrap();
    let config = Config { data_dir: dir.path().into(), ..Config::default() };
    let app = App::open(config).await.unwrap();
    let user = |name: &str| NewUser { username: name.into(), email: format!("{name}@example.com"), password: PASSWORD.into() };
    admin::setup(&app, user("alice"), "home", "Home").await.unwrap();
    admin::add_user(&app, user("bob"), false).await.unwrap();
    admin::add_channel(&app, "home", "rust", "Rust").await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(app.serve(listener));
    Server { url, _dir: dir }
}

async fn login(s: &Server, name: &str) -> Client {
    let mut c = Client::new(&s.url).unwrap();
    c.login(name, PASSWORD, None).await.unwrap();
    c
}

/// The next event that isn't a connection notice.
async fn next_event(stream: &mut ws::EventStream) -> Event {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), stream.next()).await.expect("an event").expect("stream open") {
            Message::Event(Event::Hello { .. }) | Message::Connected { .. } => continue,
            Message::Event(e) => return e,
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[tokio::test]
async fn login_bootstrap_and_chat() {
    let s = server().await;

    // Wrong passwords and unknown people get the same answer.
    let mut c = Client::new(&s.url).unwrap();
    let wrong = c.login("alice", "nope", None).await.unwrap_err();
    let nobody = c.login("carol", "nope", None).await.unwrap_err();
    assert!(wrong.is_unauthorized() && nobody.is_unauthorized());
    assert!(Client::new(&s.url).unwrap().with_token("made-up").me().await.unwrap_err().is_unauthorized());

    // What Darkmatter loads on start.
    let alice = login(&s, "ALICE@example.com").await;
    let me = alice.me().await.unwrap();
    assert_eq!((me.username.as_str(), me.email.as_str()), ("alice", "alice@example.com"));
    assert!(me.roles.contains("system_admin"));
    let teams = alice.my_teams().await.unwrap();
    assert_eq!(teams.len(), 1);
    let team = &teams[0].id;
    let channels = alice.my_channels(team).await.unwrap();
    let mut names: Vec<_> = channels.iter().map(|c| c.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["off-topic", "rust", "town-square"]);
    assert_eq!(alice.my_channel_members(team).await.unwrap().len(), 3);
    assert!(alice.client_config().await.unwrap()["Version"].starts_with("axiond"));
    assert!(alice.custom_emoji(0, 200).await.unwrap().is_empty());
    assert!(alice.user_threads(team).await.unwrap().threads.is_empty());
    let town = channels.iter().find(|c| c.name == "town-square").unwrap().id.clone();

    // Bob is listening when Alice posts.
    let bob = login(&s, "bob").await;
    let bob_id = bob.me().await.unwrap().id;
    let mut events = ws::connect(&bob).unwrap();
    assert!(matches!(events.next().await, Some(Message::Connected { resumed: false })));
    assert_eq!(alice.statuses_by_ids(std::slice::from_ref(&bob_id)).await.unwrap()[0].status, "online");

    let post = alice
        .create_post(&NewPost { channel_id: town.clone(), message: "hi @bob".into(), pending_post_id: "p1".into(), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(post.pending_post_id, "p1");
    match next_event(&mut events).await {
        Event::Posted { post: p, mentions, .. } => {
            assert_eq!((p.id.as_str(), p.message.as_str()), (post.id.as_str(), "hi @bob"));
            assert_eq!(mentions, std::slice::from_ref(&bob_id));
        }
        e => panic!("expected posted, got {e:?}"),
    }

    // Bob's unread and mention counts, then reading clears them.
    let unread = |members: Vec<mm_api::models::ChannelMember>, channels: &[mm_api::models::Channel]| {
        let m = members.into_iter().find(|m| m.channel_id == town).unwrap();
        (m.unread_count(channels.iter().find(|c| c.id == town).unwrap()), m.mention_count)
    };
    let bob_channels = bob.my_channels(team).await.unwrap();
    assert_eq!(unread(bob.my_channel_members(team).await.unwrap(), &bob_channels), (1, 1));
    bob.view_channel(&town, None).await.unwrap();
    assert_eq!(unread(bob.my_channel_members(team).await.unwrap(), &bob_channels), (0, 0));
    assert!(matches!(next_event(&mut events).await, Event::ChannelViewed { .. }));

    // A reply, an edit and a delete reach Bob; the thread and counts follow.
    let reply = bob
        .create_post(&NewPost { channel_id: town.clone(), root_id: post.id.clone(), message: "hello!".into(), ..Default::default() })
        .await
        .unwrap();
    assert!(matches!(next_event(&mut events).await, Event::Posted { post: p, .. } if p.root_id == post.id));
    let thread = alice.thread(&reply.id).await.unwrap();
    assert_eq!(thread.order, [reply.id.clone(), post.id.clone()]);
    assert_eq!(thread.posts[&post.id].reply_count, 1);
    let roots = alice.posts(&town, PostsQuery::Page { page: 0, per_page: 60 }, true).await.unwrap();
    assert_eq!(roots.order, std::slice::from_ref(&post.id));
    assert_eq!(roots.posts[&post.id].reply_count, 1);
    assert_eq!(alice.posts(&town, PostsQuery::Page { page: 0, per_page: 60 }, false).await.unwrap().order.len(), 2);

    alice.edit_post(&post.id, "hi @bob, edited").await.unwrap();
    assert!(matches!(next_event(&mut events).await, Event::PostEdited(p) if p.message == "hi @bob, edited" && p.edit_at > 0));
    assert!(bob.edit_post(&post.id, "not mine").await.is_err());
    alice.delete_post(&post.id).await.unwrap();
    let mut deleted = vec![];
    for _ in 0..2 {
        match next_event(&mut events).await {
            Event::PostDeleted(p) => deleted.push(p.id),
            e => panic!("expected post_deleted, got {e:?}"),
        }
    }
    deleted.sort();
    let mut expected = vec![post.id.clone(), reply.id.clone()];
    expected.sort();
    assert_eq!(deleted, expected);
    assert!(alice.posts(&town, PostsQuery::Page { page: 0, per_page: 60 }, false).await.unwrap().order.is_empty());

    // Typing reaches the other person.
    let mut alice_events = ws::connect(&alice).unwrap();
    assert!(matches!(alice_events.next().await, Some(Message::Connected { .. })));
    events.send(ws::Action::Typing { channel_id: town.clone(), parent_id: String::new() });
    assert!(matches!(next_event(&mut alice_events).await, Event::Typing { user_id, channel_id, .. } if user_id == bob_id && channel_id == town));
}

#[tokio::test]
async fn paging_direct_messages_and_channels() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let (alice_id, bob_id) = (alice.me().await.unwrap().id, bob.me().await.unwrap().id);
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let rust = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "rust").unwrap().id;

    let mut ids = vec![];
    for i in 0..7 {
        let p = alice.create_post(&NewPost { channel_id: rust.clone(), message: format!("m{i}"), ..Default::default() }).await.unwrap();
        ids.push(p.id);
    }
    // Newest page, then older, then newer again, as the timeline scrolls.
    let newest = alice.posts(&rust, PostsQuery::Page { page: 0, per_page: 3 }, true).await.unwrap();
    assert_eq!(newest.order, [ids[6].clone(), ids[5].clone(), ids[4].clone()]);
    assert_eq!((newest.prev_post_id.as_str(), newest.next_post_id.as_str()), (ids[3].as_str(), ""));
    let older = alice.posts(&rust, PostsQuery::Before { post_id: ids[4].clone(), per_page: 3 }, true).await.unwrap();
    assert_eq!(older.order, [ids[3].clone(), ids[2].clone(), ids[1].clone()]);
    let oldest = alice.posts(&rust, PostsQuery::Before { post_id: ids[1].clone(), per_page: 3 }, true).await.unwrap();
    assert_eq!((oldest.order.len(), oldest.prev_post_id.as_str()), (1, ""));
    let newer = alice.posts(&rust, PostsQuery::After { post_id: ids[1].clone(), per_page: 2 }, true).await.unwrap();
    assert_eq!(newer.order, [ids[3].clone(), ids[2].clone()]);
    assert_eq!(newer.next_post_id, ids[4]);
    let since = alice.posts(&rust, PostsQuery::Since(0), true).await.unwrap();
    assert_eq!(since.order.len(), 7);

    // A direct message is made once and shows up for both.
    let dm = alice.create_direct(&alice_id, &bob_id).await.unwrap();
    assert_eq!(dm.kind, ChannelType::Direct);
    assert_eq!(bob.create_direct(&bob_id, &alice_id).await.unwrap().id, dm.id);
    assert_eq!(dm.dm_partner(&alice_id), Some(bob_id.as_str()));
    assert!(bob.my_channels(&team).await.unwrap().iter().any(|c| c.id == dm.id));
    alice.create_post(&NewPost { channel_id: dm.id.clone(), message: "psst".into(), ..Default::default() }).await.unwrap();
    let members = bob.my_channel_members(&team).await.unwrap();
    assert_eq!(members.iter().find(|m| m.channel_id == dm.id).unwrap().mention_count, 1);
    assert_eq!(alice.users_by_ids(std::slice::from_ref(&bob_id)).await.unwrap()[0].email, "", "others' emails stay private");
    assert_eq!(alice.autocomplete_users(&team, "bo").await.unwrap()[0].id, bob_id);

    // Leaving and joining a public channel, found by search.
    bob.leave_channel(&rust, &bob_id).await.unwrap();
    assert!(!bob.my_channels(&team).await.unwrap().iter().any(|c| c.id == rust));
    assert!(bob.create_post(&NewPost { channel_id: rust.clone(), message: "x".into(), ..Default::default() }).await.is_err());
    let found = bob.search_channels(&team, "rus").await.unwrap();
    assert_eq!(found[0].id, rust);
    bob.join_channel(&rust, &bob_id).await.unwrap();
    let last = alice.posts(&rust, PostsQuery::Page { page: 0, per_page: 1 }, true).await.unwrap();
    assert_eq!(last.posts[&last.order[0]].kind, "system_join_channel");
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    assert!(alice.leave_channel(&town, &alice_id).await.is_err());

    // Logging out ends that session only.
    let mut bob = bob;
    let token = bob.token().unwrap().to_string();
    bob.logout().await.unwrap();
    assert!(Client::new(&s.url).unwrap().with_token(token).me().await.unwrap_err().is_unauthorized());
    assert!(alice.me().await.is_ok());
}
