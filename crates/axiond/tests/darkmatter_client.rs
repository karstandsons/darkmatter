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
    app: App,
    _dir: tempfile::TempDir,
}

async fn server() -> Server {
    server_with(|_| {}).await
}

async fn server_with(change: impl FnOnce(&mut Config)) -> Server {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config { data_dir: dir.path().into(), ..Config::default() };
    change(&mut config);
    let app = App::open(config).await.unwrap();
    let user = |name: &str| NewUser { username: name.into(), email: format!("{name}@example.com"), password: PASSWORD.into() };
    admin::setup(&app, user("alice"), "home", "Home").await.unwrap();
    admin::add_user(&app, user("bob"), false).await.unwrap();
    admin::add_channel(&app, "home", "rust", "Rust").await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(app.clone().serve(listener));
    Server { url, app, _dir: dir }
}

async fn login(s: &Server, name: &str) -> Client {
    let mut c = Client::new(&s.url).unwrap().user_agent("Darkmatter/test (linux)").unwrap();
    c.login(name, PASSWORD, None).await.unwrap();
    c
}

/// The next event that isn't a connection notice or someone coming and going.
async fn next_event(stream: &mut ws::EventStream) -> Event {
    loop {
        match next_any(stream).await {
            Event::StatusChange { .. } => continue,
            e => return e,
        }
    }
}

/// The next event that isn't a connection notice.
async fn next_any(stream: &mut ws::EventStream) -> Event {
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
    // Replying follows the thread.
    assert!(matches!(next_event(&mut events).await, Event::ThreadUpdated(t) if t.id == post.id && t.unread_replies == 0));
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
    // DMs are end-to-end encrypted: plain text is refused (tests/encrypted_dms.rs).
    assert!(dm.encrypted);
    assert!(alice.create_post(&NewPost { channel_id: dm.id.clone(), message: "psst".into(), ..Default::default() }).await.is_err());
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

#[tokio::test]
async fn reactions_threads_and_unread() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let (alice_id, bob_id) = (alice.me().await.unwrap().id, bob.me().await.unwrap().id);
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let rust = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "rust").unwrap().id;
    let post = |c: &Client, message: &str, root: &str| {
        let new = NewPost { channel_id: rust.clone(), message: message.into(), root_id: root.into(), ..Default::default() };
        let c = c.clone();
        async move { c.create_post(&new).await.unwrap() }
    };

    // Reactions: added once, shown on the post, and sent live.
    let root = post(&alice, "lunch?", "").await;
    let mut alice_events = ws::connect(&alice).unwrap();
    assert!(matches!(alice_events.next().await, Some(Message::Connected { .. })));
    let r = bob.add_reaction(&bob_id, &root.id, "+1").await.unwrap();
    assert_eq!((r.emoji_name.as_str(), r.user_id.as_str()), ("+1", bob_id.as_str()));
    bob.add_reaction(&bob_id, &root.id, "+1").await.unwrap();
    match next_event(&mut alice_events).await {
        Event::ReactionAdded(r) => assert_eq!((r.post_id, r.emoji_name), (root.id.clone(), "+1".into())),
        e => panic!("expected a reaction, got {e:?}"),
    }
    let thread = alice.thread(&root.id).await.unwrap();
    assert_eq!(thread.posts[&root.id].metadata.reactions.len(), 1);
    assert!(bob.add_reaction(&alice_id, &root.id, "smile").await.is_err(), "can't react as someone else");
    assert!(bob.add_reaction(&bob_id, &root.id, "Not An Emoji").await.is_err());
    bob.remove_reaction(&bob_id, &root.id, "+1").await.unwrap();
    assert!(matches!(next_event(&mut alice_events).await, Event::ReactionRemoved(_)));
    let page = alice.posts(&rust, PostsQuery::Page { page: 0, per_page: 10 }, true).await.unwrap();
    assert!(page.posts[&root.id].metadata.reactions.is_empty());

    // Bob replies and mentions nobody: Alice follows her thread and has an unread reply.
    post(&bob, "yes, noon", &root.id).await;
    loop {
        match next_event(&mut alice_events).await {
            Event::ThreadUpdated(t) => {
                assert_eq!((t.id.as_str(), t.reply_count, t.unread_replies), (root.id.as_str(), 1, 1));
                assert_eq!(t.participants.iter().map(|u| u.username.as_str()).collect::<Vec<_>>(), ["alice", "bob"]);
                break;
            }
            Event::Posted { .. } => continue,
            e => panic!("expected the thread update, got {e:?}"),
        }
    }
    let threads = alice.user_threads(&team).await.unwrap();
    assert_eq!((threads.total, threads.total_unread_threads, threads.total_unread_mentions), (1, 1, 0));
    assert!(bob.user_threads(&team).await.unwrap().threads[0].unread_replies == 0, "your own reply is read");

    // A mention in the thread counts for Alice, and reading clears it.
    let mention = post(&bob, "@alice bring cake", &root.id).await;
    let t = &alice.user_threads(&team).await.unwrap().threads[0];
    assert_eq!((t.unread_replies, t.unread_mentions), (2, 1));
    alice.mark_thread_read(&team, &root.id, mention.create_at).await.unwrap();
    let t = &alice.user_threads(&team).await.unwrap().threads[0];
    assert_eq!((t.unread_replies, t.unread_mentions, t.last_viewed_at), (0, 0, mention.create_at));

    // "Mark as unread" from the mention: two messages unread, one a mention.
    alice.view_channel(&rust, None).await.unwrap();
    let unread = alice.set_unread(&alice_id, &mention.id).await.unwrap();
    let ch = alice.channel(&rust).await.unwrap();
    assert_eq!((ch.total_msg_count - unread.msg_count, unread.mention_count), (1, 1));
    let member = alice.my_channel_members(&team).await.unwrap().into_iter().find(|m| m.channel_id == rust).unwrap();
    assert_eq!((member.unread_count(&ch), member.mention_count), (1, 1));
    let from_root = alice.set_unread(&alice_id, &root.id).await.unwrap();
    assert_eq!((ch.total_msg_count - from_root.msg_count, ch.total_msg_count_root - from_root.msg_count_root), (3, 1));
    assert!(bob.set_unread(&alice_id, &root.id).await.is_err(), "only for yourself");
}

/// A small PNG, `w` by `h`.
fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([x as u8, y as u8, 128]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[tokio::test]
async fn files_and_pictures() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let rust = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "rust").unwrap().id;
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;

    // A picture gets its size, a thumbnail and a preview.
    let picture = png(300, 200);
    let up = alice.upload_file(&rust, "../cat.png", picture.clone()).await.unwrap();
    let info = &up.file_infos[0];
    assert_eq!((info.name.as_str(), info.extension.as_str(), info.mime_type.as_str()), ("cat.png", "png", "image/png"));
    assert_eq!((info.width, info.height, info.size, info.has_preview_image), (300, 200, picture.len() as i64, true));
    assert!(bob.file(&info.id).await.is_err(), "nobody else sees it before it's posted");

    // Posted with no text, it shows in the post for everyone in the channel.
    let post = alice
        .create_post(&NewPost { channel_id: rust.clone(), file_ids: vec![info.id.clone()], ..Default::default() })
        .await
        .unwrap();
    assert_eq!(post.file_ids, std::slice::from_ref(&info.id));
    let page = bob.posts(&rust, PostsQuery::Page { page: 0, per_page: 10 }, true).await.unwrap();
    assert_eq!(page.posts[&post.id].metadata.files[0].id, info.id);
    assert_eq!(bob.file(&info.id).await.unwrap(), picture);
    let thumb = image::load_from_memory(&bob.file_thumbnail(&info.id).await.unwrap()).unwrap();
    assert!(thumb.width() <= 120 && thumb.height() <= 100);
    assert_eq!(image::load_from_memory(&bob.file_preview(&info.id).await.unwrap()).unwrap().width(), 300);

    // Downloads are attachments a browser won't run.
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v4/files/{}", s.url, info.id))
        .bearer_auth(bob.token().unwrap())
        .send()
        .await
        .unwrap();
    let h = resp.headers();
    assert_eq!(h["content-disposition"], "attachment; filename*=UTF-8''cat%2Epng");
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert!(h["content-security-policy"].to_str().unwrap().contains("sandbox"));

    // An upload goes in one post, in its own channel, by whoever uploaded it.
    assert!(alice.create_post(&NewPost { channel_id: rust.clone(), file_ids: vec![info.id.clone()], ..Default::default() }).await.is_err());
    let notes = alice.upload_file(&rust, "notes.txt", b"plain text".to_vec()).await.unwrap().file_infos[0].clone();
    assert!(!notes.has_preview_image && notes.mime_type == "text/plain");
    assert!(bob.file_thumbnail(&notes.id).await.is_err());
    assert!(bob.create_post(&NewPost { channel_id: rust.clone(), file_ids: vec![notes.id.clone()], ..Default::default() }).await.is_err());
    assert!(alice.create_post(&NewPost { channel_id: town, file_ids: vec![notes.id.clone()], ..Default::default() }).await.is_err());

    // Deleting the post takes its files with it.
    alice.delete_post(&post.id).await.unwrap();
    assert!(bob.file(&info.id).await.is_err());

    // Something that only looks like a picture is just a file.
    let fake = alice.upload_file(&rust, "fake.png", b"not really a png".to_vec()).await.unwrap().file_infos[0].clone();
    assert!(!fake.has_preview_image);
}

#[tokio::test]
async fn uploads_stop_at_the_size_limit() {
    let s = server_with(|c| c.max_file_mb = 1).await;
    let alice = login(&s, "alice").await;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let rust = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "rust").unwrap().id;
    alice.upload_file(&rust, "ok.bin", vec![7; 1024 * 1024]).await.unwrap();
    let err = alice.upload_file(&rust, "big.bin", vec![7; 1024 * 1024 + 1]).await.unwrap_err();
    assert!(format!("{err:?}").contains("413") || format!("{err}").contains("at most 1 MB"), "{err:?}");
}

#[tokio::test]
async fn search() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let channels = alice.my_channels(&team).await.unwrap();
    let id = |name: &str| channels.iter().find(|c| c.name == name).unwrap().id.clone();
    let (rust, town, off) = (id("rust"), id("town-square"), id("off-topic"));
    let say = |c: &Client, channel: &str, message: &str| {
        let new = NewPost { channel_id: channel.into(), message: message.into(), ..Default::default() };
        let c = c.clone();
        async move { c.create_post(&new).await.unwrap() }
    };
    let borrow = say(&alice, &rust, "The borrow checker says no").await;
    let lunch = say(&bob, &town, "Lunch at noon? Café downstairs").await;
    let cake = say(&bob, &town, "lunch cake later").await;
    say(&alice, &off, "a secret plan").await;
    let found = |terms: &'static str| {
        let alice = alice.clone();
        let team = team.clone();
        async move {
            let mut ids = alice.search_posts(&team, terms).await.unwrap().order;
            ids.sort();
            ids
        }
    };
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };

    assert_eq!(found("borrow").await, std::slice::from_ref(&borrow.id));
    assert_eq!(found("BORR*").await, std::slice::from_ref(&borrow.id));
    assert_eq!(found("lunch").await, sorted(vec![lunch.id.clone(), cake.id.clone()]));
    assert_eq!(found("cafe").await, std::slice::from_ref(&lunch.id), "accents don't matter");
    assert_eq!(found("lunch -cake").await, std::slice::from_ref(&lunch.id));
    assert_eq!(found("\"lunch cake\"").await, std::slice::from_ref(&cake.id));
    assert_eq!(found("from:alice").await.len(), 2);
    assert_eq!(found("lunch from:@bob in:~town-square").await.len(), 2);
    assert!(found("lunch in:rust").await.is_empty());
    assert!(found("before:2000-01-01").await.is_empty());
    assert!(found("\" AND OR NOT ( * :").await.is_empty(), "odd input is no results, not an error");

    // Only channels you're in.
    bob.leave_channel(&off, &bob.me().await.unwrap().id).await.ok();
    assert!(bob.search_posts(&team, "secret").await.unwrap().order.is_empty());
    assert_eq!(found("secret").await.len(), 1);

    // Edits and deletes keep the index right.
    alice.edit_post(&borrow.id, "The compiler says yes").await.unwrap();
    assert!(found("borrow").await.is_empty());
    assert_eq!(found("compiler").await, std::slice::from_ref(&borrow.id));
    bob.delete_post(&cake.id).await.unwrap();
    assert!(found("cake").await.is_empty());

    // Never encrypted channels, whatever is stored there.
    sqlx::query("UPDATE channels SET encrypted = 1 WHERE id = ?").bind(&town).execute(&s.app.db).await.unwrap();
    assert!(found("lunch").await.is_empty());
}

/// A multipart upload the way Mattermost's web app sends it.
async fn upload(s: &Server, c: &Client, path: &str, parts: Vec<(&'static str, reqwest::multipart::Part)>) -> reqwest::Response {
    let mut form = reqwest::multipart::Form::new();
    for (name, part) in parts {
        form = form.part(name, part);
    }
    reqwest::Client::new()
        .post(format!("{}/api/v4/{path}", s.url))
        .bearer_auth(c.token().unwrap())
        .multipart(form)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn emoji_and_pictures() {
    use reqwest::multipart::Part;
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let bob_id = bob.me().await.unwrap().id;
    let emoji = |name: &str, bytes: Vec<u8>| {
        vec![("image", Part::bytes(bytes).file_name("e.png")), ("emoji", Part::text(format!(r#"{{"name":"{name}"}}"#)))]
    };

    // Anyone can add an emoji; everyone sees it.
    let resp = upload(&s, &bob, "emoji", emoji("party", png(32, 32))).await;
    assert_eq!(resp.status(), 201);
    let list = alice.custom_emoji(0, 200).await.unwrap();
    assert_eq!(list.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["party"]);
    let picture = image::load_from_memory(&alice.emoji_image(&list[0].id).await.unwrap()).unwrap();
    assert_eq!(picture.width(), 32);

    // Names are unique and tidy, and only small real pictures pass.
    assert_eq!(upload(&s, &alice, "emoji", emoji("party", png(8, 8))).await.status(), 400);
    assert_eq!(upload(&s, &alice, "emoji", emoji("Party Time", png(8, 8))).await.status(), 400);
    assert_eq!(upload(&s, &alice, "emoji", emoji("svg", b"<svg onload=alert(1)/>".to_vec())).await.status(), 400);
    assert_eq!(upload(&s, &alice, "emoji", emoji("huge", png(2000, 10))).await.status(), 400);

    // Only its creator or an admin removes it.
    let id = list[0].id.clone();
    let del = |c: &Client| reqwest::Client::new().delete(format!("{}/api/v4/emoji/{id}", s.url)).bearer_auth(c.token().unwrap().to_string()).send();
    let carol = {
        admin::add_user(&s.app, NewUser { username: "carol".into(), email: "carol@example.com".into(), password: PASSWORD.into() }, false).await.unwrap();
        login(&s, "carol").await
    };
    assert_eq!(del(&carol).await.unwrap().status(), 403);
    assert_eq!(del(&alice).await.unwrap().status(), 200);
    assert!(alice.custom_emoji(0, 200).await.unwrap().is_empty());
    assert!(alice.emoji_image(&id).await.is_err());

    // Profile pictures become a 256-pixel square; until then, 404 for initials.
    assert!(alice.profile_image(&bob_id).await.is_err());
    let path = format!("users/{bob_id}/image");
    assert_eq!(upload(&s, &bob, &path, vec![("image", Part::bytes(png(400, 300)).file_name("me.png"))]).await.status(), 200);
    let pic = image::load_from_memory(&alice.profile_image(&bob_id).await.unwrap()).unwrap();
    assert_eq!((pic.width(), pic.height()), (256, 256));
    assert!(alice.users_by_ids(std::slice::from_ref(&bob_id)).await.unwrap()[0].last_picture_update > 0);
    assert_eq!(upload(&s, &carol, &path, vec![("image", Part::bytes(png(10, 10)).file_name("x.png"))]).await.status(), 403);
}

#[tokio::test]
async fn pictures_stay_in_their_folders() {
    use reqwest::multipart::Part;
    let s = server().await;
    let alice = login(&s, "alice").await; // an admin, so may change anyone's picture
    let bob = login(&s, "bob").await;
    let bob_id = bob.me().await.unwrap().id;
    let path = format!("users/{bob_id}/image");
    assert_eq!(upload(&s, &bob, &path, vec![("image", Part::bytes(png(40, 40)).file_name("me.png"))]).await.status(), 200);

    // A picture outside `pictures/` and `emoji/`, where `..%2F` would lead.
    let data = s._dir.path();
    let probe = data.join("probe.png");
    std::fs::write(&probe, png(8, 8)).unwrap();
    let http = reqwest::Client::new();
    let send = |method: reqwest::Method, path: &str| {
        http.request(method, format!("{}/api/v4/{path}", s.url)).bearer_auth(alice.token().unwrap().to_string()).send()
    };
    use reqwest::Method;
    for sneaky in ["..%2Fprobe", "..%2F..%2Fpictures%2Fprobe", "%2E%2E%2Fprobe", "..%5Cprobe", "..%2F", "..%2F..%2F..%2F..%2F..%2Fetc%2Fpasswd"] {
        let users = format!("users/{sneaky}/image");
        let emoji = format!("emoji/{sneaky}");
        let get = send(Method::GET, &users).await.unwrap();
        assert!(get.status().is_client_error(), "GET {users}: {}", get.status());
        assert_eq!(send(Method::GET, &format!("{emoji}/image")).await.unwrap().status().as_u16() / 100, 4, "GET {emoji}/image");
        let form = || reqwest::multipart::Form::new().part("image", Part::bytes(png(10, 10)).file_name("x.png"));
        for method in [Method::POST, Method::PUT] {
            let up = http
                .request(method.clone(), format!("{}/api/v4/{users}", s.url))
                .bearer_auth(alice.token().unwrap().to_string())
                .multipart(form())
                .send()
                .await
                .unwrap();
            assert!(up.status().is_client_error(), "{method} {users}: {}", up.status());
        }
        assert!(send(Method::DELETE, &users).await.unwrap().status().is_client_error(), "DELETE {users}");
        assert!(send(Method::DELETE, &emoji).await.unwrap().status().is_client_error(), "DELETE {emoji}");
    }
    // Nothing was read, written or removed outside the folders.
    assert_eq!(std::fs::read(&probe).unwrap(), png(8, 8));
    let mut outside: Vec<_> =
        std::fs::read_dir(data).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).filter(|n| n.ends_with(".png")).collect();
    outside.sort();
    assert_eq!(outside, ["probe.png"]);
    // Real ids still work.
    assert!(alice.profile_image(&bob_id).await.is_ok());
}

#[tokio::test]
async fn statuses_and_server_commands() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let (alice_id, bob_id) = (alice.me().await.unwrap().id, bob.me().await.unwrap().id);
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let channels = alice.my_channels(&team).await.unwrap();
    let id = |name: &str| channels.iter().find(|c| c.name == name).unwrap().id.clone();
    let (town, rust) = (id("town-square"), id("rust"));
    let status = |c: &Client, who: &str| {
        let (c, who) = (c.clone(), who.to_string());
        async move { c.statuses_by_ids(&[who]).await.unwrap().pop().unwrap().status }
    };

    // Online while an app is connected; others hear about it.
    let mut alice_events = ws::connect(&alice).unwrap();
    assert!(matches!(next_any(&mut alice_events).await, Event::StatusChange { user_id, status } if user_id == alice_id && status == "online"));
    assert_eq!(status(&bob, &alice_id).await, "online");
    assert_eq!(status(&alice, &bob_id).await, "offline");
    let bob_events = ws::connect(&bob).unwrap();
    assert!(matches!(next_any(&mut alice_events).await, Event::StatusChange { user_id, status } if user_id == bob_id && status == "online"));

    // /away sticks while connected, and /online goes back to automatic.
    let run = |c: &Client, channel: &str, command: &str| {
        let (c, channel, command, team) = (c.clone(), channel.to_string(), command.to_string(), team.clone());
        async move { c.execute_command(&team, &channel, "", &command).await }
    };
    let said = run(&bob, &town, "/away").await.unwrap();
    assert_eq!(said.text, "You are now away.");
    assert!(matches!(next_any(&mut alice_events).await, Event::StatusChange { status, .. } if status == "away"));
    assert_eq!(status(&alice, &bob_id).await, "away");
    run(&bob, &town, "/online").await.unwrap();
    assert_eq!(status(&alice, &bob_id).await, "online");
    drop(bob_events);
    loop {
        if let Event::StatusChange { user_id, status } = next_any(&mut alice_events).await
            && user_id == bob_id
            && status == "offline"
        {
            break;
        }
    }

    // Replies only you see arrive as ephemeral messages.
    run(&alice, &town, "/dnd").await.unwrap();
    loop {
        if let Event::Ephemeral(p) = next_event(&mut alice_events).await {
            assert!(p.message.starts_with("Do not disturb") && p.channel_id == town);
            break;
        }
    }

    // /me and /shrug post as you.
    run(&alice, &town, "/me waves").await.unwrap();
    run(&alice, &town, "/shrug fine").await.unwrap();
    let page = alice.posts(&town, PostsQuery::Page { page: 0, per_page: 2 }, false).await.unwrap();
    let texts: Vec<_> = page.chronological().iter().map(|p| p.message.clone()).collect();
    assert_eq!(texts, ["*waves*", "fine ¯\\\\\\_(ツ)\\_/¯"]);

    // /leave and /join answer with where to go.
    let left = run(&bob, &rust, "/leave").await.unwrap();
    assert_eq!(left.goto_location, "/home/channels/town-square");
    let joined = run(&bob, &town, "/join ~rust").await.unwrap();
    assert_eq!(joined.goto_location, "/home/channels/rust");
    assert!(bob.my_channels(&team).await.unwrap().iter().any(|c| c.id == rust));

    // /msg opens a DM. It's encrypted, so the server won't write the text
    // for you, and says so.
    let dm = run(&alice, &town, "/msg @bob psst").await.unwrap();
    let dm_name = dm.goto_location.rsplit('/').next().unwrap().to_string();
    let dm_channel = bob.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == dm_name).unwrap();
    assert!(dm_channel.encrypted);
    assert!(bob.posts(&dm_channel.id, PostsQuery::Page { page: 0, per_page: 5 }, false).await.unwrap().order.is_empty());

    // /header changes the channel for everyone, with a note in the channel.
    run(&bob, &rust, "/header Rust talk").await.unwrap();
    assert_eq!(alice.channel(&rust).await.unwrap().header, "Rust talk");

    // /invite and /kick (the second only for admins and the channel's creator).
    admin::add_user(&s.app, NewUser { username: "carol".into(), email: "carol@example.com".into(), password: PASSWORD.into() }, false).await.unwrap();
    let carol = login(&s, "carol").await;
    let carol_id = carol.me().await.unwrap().id;
    run(&carol, &town, "/leave").await.unwrap_err();
    carol.leave_channel(&rust, &carol_id).await.unwrap();
    run(&bob, &rust, "/invite @carol").await.unwrap();
    assert!(carol.my_channels(&team).await.unwrap().iter().any(|c| c.id == rust));
    assert!(run(&bob, &rust, "/kick @carol").await.is_err());
    run(&alice, &rust, "/kick @carol").await.unwrap();
    assert!(!carol.my_channels(&team).await.unwrap().iter().any(|c| c.id == rust));

    // /mute toggles the channel's notifications.
    run(&alice, &rust, "/mute").await.unwrap();
    let member = alice.my_channel_members(&team).await.unwrap().into_iter().find(|m| m.channel_id == rust).unwrap();
    assert!(member.is_muted());

    // Unknown commands are an error; completions list matches.
    assert!(run(&alice, &town, "/nope").await.is_err());
    let s2 = alice.command_suggestions(&team, &town, "/m").await.unwrap();
    assert_eq!(s2.iter().map(|x| x.complete.as_str()).collect::<Vec<_>>(), ["me", "msg", "mute"]);
}

#[tokio::test]
async fn sessions_and_two_factor_codes() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let laptop = login(&s, "alice").await;
    let alice_id = alice.me().await.unwrap().id;
    let http = reqwest::Client::new();
    let api = |path: &str| format!("{}/api/v4/{path}", s.url);
    let get = |c: &Client, path: String| http.get(api(&path)).bearer_auth(c.token().unwrap().to_string()).send();

    // Two apps signed in; the list never shows tokens.
    let sessions: serde_json::Value = get(&alice, format!("users/{alice_id}/sessions")).await.unwrap().json().await.unwrap();
    let sessions = sessions.as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().all(|s| s["device_id"].as_str().unwrap().len() == 26 && s.get("token").is_none()));
    let current = sessions.iter().filter(|s| s["is_current"] == true).count();
    assert_eq!(current, 1);

    // Signing the laptop out ends it at once.
    let laptop_session = sessions.iter().find(|s| s["is_current"] == false).unwrap()["id"].as_str().unwrap().to_string();
    let revoke = http
        .post(api(&format!("users/{alice_id}/sessions/revoke")))
        .bearer_auth(alice.token().unwrap())
        .json(&serde_json::json!({ "session_id": laptop_session }))
        .send()
        .await
        .unwrap();
    assert_eq!(revoke.status(), 200);
    assert!(laptop.me().await.unwrap_err().is_unauthorized());

    // Turning on two-factor codes: a secret, then a code from it.
    let generated: serde_json::Value = http.post(api(&format!("users/{alice_id}/mfa/generate"))).bearer_auth(alice.token().unwrap()).send().await.unwrap().json().await.unwrap();
    assert!(generated["url"].as_str().unwrap().starts_with("otpauth://totp/axiond:alice?secret="));
    let secret = totp_rs::Secret::try_from_base32(generated["secret"].as_str().unwrap()).unwrap();
    let totp = totp_rs::Builder::new().with_secret(secret).build().unwrap();
    let activate = |code: String, on: bool| {
        http.put(api(&format!("users/{alice_id}/mfa")))
            .bearer_auth(alice.token().unwrap().to_string())
            .json(&serde_json::json!({ "activate": on, "code": code }))
            .send()
    };
    assert_eq!(activate("000000".into(), true).await.unwrap().status(), 400);
    let code = totp.generate_current().to_string();
    assert_eq!(activate(code.clone(), true).await.unwrap().status(), 200);

    // Logging in now needs a code, and each code works once.
    let mut c = Client::new(&s.url).unwrap();
    assert!(c.login("alice", PASSWORD, None).await.unwrap_err().needs_mfa());
    assert!(c.login("alice", "wrong", None).await.unwrap_err().is_unauthorized(), "a wrong password doesn't reveal codes are on");
    assert!(!c.login("alice", "wrong", None).await.unwrap_err().needs_mfa());
    assert!(c.login("alice", PASSWORD, Some(&code)).await.is_err(), "the code used to turn it on is spent");
    // The next step's code is accepted (clocks drift), and then it's spent too.
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let next = totp.generate(unix + 30).to_string();
    c.login("alice", PASSWORD, Some(&next)).await.unwrap();
    assert!(Client::new(&s.url).unwrap().login("alice", PASSWORD, Some(&next)).await.is_err(), "no replays");

    // An admin can turn them off for a lost phone.
    admin::mfa_off(&s.app, "alice").await.unwrap();
    assert!(c.me().await.unwrap_err().is_unauthorized(), "and that signs them out");
    Client::new(&s.url).unwrap().login("alice", PASSWORD, None).await.unwrap();

    // Darkmatter can tell axiond from plain Mattermost.
    let e2ee: serde_json::Value = http.get(api("e2ee/config")).send().await.unwrap().json().await.unwrap();
    assert_eq!((e2ee["server"].as_str(), e2ee["enabled"].as_bool(), e2ee["ciphersuite"].as_u64()), (Some("axiond"), Some(true), Some(1)));
    assert_eq!(e2ee["external_sender"].as_str().map(str::len), Some(44), "the server's key, base64");
}

#[tokio::test]
async fn invites_blocking_reports_and_deleting() {
    use serde_json::{Value, json};
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let bob_id = bob.me().await.unwrap().id;
    let http = reqwest::Client::new();
    let api = |path: &str| format!("{}/api/v4/{path}", s.url);
    let as_user = |c: &Client, method: reqwest::Method, path: &str, body: Value| {
        http.request(method, api(path)).bearer_auth(c.token().unwrap().to_string()).json(&body).send()
    };
    use reqwest::Method;

    // Only admins make invites; a code works as many times as it says.
    assert_eq!(as_user(&bob, Method::POST, "invites", json!({})).await.unwrap().status(), 403);
    let made: Value = as_user(&alice, Method::POST, "invites", json!({ "max_uses": 1, "note": "for carol" })).await.unwrap().json().await.unwrap();
    let code = made["code"].as_str().unwrap().to_string();
    let signup = |code: &str, name: &str| {
        http.post(api(&format!("users?t={code}")))
            .json(&json!({ "username": name, "email": format!("{name}@example.com"), "password": PASSWORD }))
            .send()
    };
    assert_eq!(signup("made-up", "mallory").await.unwrap().status(), 400);
    assert_eq!(signup(&code, "Bad Name").await.unwrap().status(), 400, "a bad username doesn't spend the invite");
    assert_eq!(signup(&code, "carol").await.unwrap().status(), 201);
    assert_eq!(signup(&code, "dave").await.unwrap().status(), 400, "used up");
    let carol = login(&s, "carol").await;
    let carol_id = carol.me().await.unwrap().id;
    let team = carol.my_teams().await.unwrap()[0].id.clone();
    assert!(carol.my_channels(&team).await.unwrap().iter().any(|c| c.name == "town-square"));
    assert!(http.post(api("users")).json(&json!({ "username": "eve", "email": "eve@example.com", "password": PASSWORD })).send().await.unwrap().status().is_client_error());

    // Blocking: no DMs either way, and the blocked person's mentions don't count.
    let dm = alice.create_direct(&alice.me().await.unwrap().id, &bob_id).await.unwrap();
    assert_eq!(as_user(&bob, Method::PUT, &format!("users/me/blocked/{carol_id}"), json!({})).await.unwrap().status(), 200);
    assert!(carol.create_direct(&carol_id, &bob_id).await.is_err());
    assert!(bob.create_direct(&bob_id, &carol_id).await.is_err());
    let town = carol.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    carol.create_post(&NewPost { channel_id: town.clone(), message: "@bob hello?".into(), ..Default::default() }).await.unwrap();
    let member = bob.my_channel_members(&team).await.unwrap().into_iter().find(|m| m.channel_id == town).unwrap();
    assert_eq!(member.mention_count, 0);
    let list: Vec<String> = as_user(&bob, Method::GET, "users/me/blocked", json!({})).await.unwrap().json().await.unwrap();
    assert_eq!(list, std::slice::from_ref(&carol_id));

    // Reports go to admins, who see and resolve them (in the audit log).
    let post = carol.create_post(&NewPost { channel_id: town.clone(), message: "something nasty".into(), ..Default::default() }).await.unwrap();
    assert_eq!(as_user(&bob, Method::POST, "reports", json!({ "post_id": post.id, "reason": "rude" })).await.unwrap().status(), 201);
    assert_eq!(as_user(&bob, Method::GET, "reports", json!({})).await.unwrap().status(), 403);
    let open: Vec<Value> = as_user(&alice, Method::GET, "reports", json!({})).await.unwrap().json().await.unwrap();
    assert_eq!((open.len(), open[0]["user_id"].as_str()), (1, Some(carol_id.as_str())));
    let id = open[0]["id"].as_str().unwrap();
    assert_eq!(as_user(&alice, Method::PUT, &format!("reports/{id}"), json!({ "resolution": "talked to carol" })).await.unwrap().status(), 200);
    assert!(admin::open_reports(&s.app).await.unwrap().is_empty());

    // Deactivating signs someone out; they can come back.
    assert_eq!(as_user(&alice, Method::PUT, &format!("users/{carol_id}/active"), json!({ "active": false })).await.unwrap().status(), 200);
    assert!(carol.me().await.unwrap_err().is_unauthorized());
    assert!(Client::new(&s.url).unwrap().login("carol", PASSWORD, None).await.is_err());
    admin::set_active(&s.app, "carol", true).await.unwrap();
    let carol = login(&s, "carol").await;

    // Deleting your own account needs your password, and leaves only "deleted-…".
    assert_eq!(as_user(&carol, Method::DELETE, "users/me", json!({ "password": "nope" })).await.unwrap().status(), 400);
    assert_eq!(as_user(&carol, Method::DELETE, "users/me", json!({ "password": PASSWORD })).await.unwrap().status(), 200);
    assert!(carol.me().await.unwrap_err().is_unauthorized());
    let gone = alice.users_by_ids(std::slice::from_ref(&carol_id)).await.unwrap().pop().unwrap();
    assert!(gone.username.starts_with("deleted-") && gone.delete_at > 0);
    assert_eq!(alice.thread(&post.id).await.unwrap().posts.len(), 1, "messages stay unless asked");
    assert!(admin::set_active(&s.app, &gone.username, true).await.is_err(), "a deleted account stays deleted");

    // The last admin can't be removed by accident.
    assert_eq!(as_user(&alice, Method::DELETE, "users/me", json!({ "password": PASSWORD })).await.unwrap().status(), 400);
    assert_eq!(as_user(&bob, Method::PUT, &format!("users/{bob_id}/active"), json!({ "active": false })).await.unwrap().status(), 403);

    // The audit log shows who did what.
    let log = axiond::audit::recent(&s.app.db, 50).await.unwrap();
    let actions: Vec<&str> = log.iter().rev().map(|e| e.action.as_str()).collect();
    assert_eq!(actions, ["server.setup", "invite.create", "user.signup", "report.resolve", "user.deactivate", "user.activate", "user.delete"]);
    assert_eq!(log[log.len() - 2].actor, "alice");
    assert_eq!(log.last().unwrap().target, "alice", "the first admin");
    let _ = dm;
}

#[tokio::test]
async fn rate_limits() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let http = reqwest::Client::new();
    let try_login = |who: String, forwarded: &'static str| {
        http.post(format!("{}/api/v4/users/login", s.url))
            .header("x-forwarded-for", forwarded)
            .json(&serde_json::json!({ "login_id": who, "password": "x" }))
            .send()
    };
    // Ten tries from one address (one was Alice's login), then it has to
    // wait. Not behind a proxy, a made-up X-Forwarded-For changes nothing.
    // All at once: checking each password is slow on purpose, and one try
    // comes back every 6 s, so tries one after another could earn a new one.
    let forwarded = ["1.2.3.4", "5.6.7.8", "9.9.9.9"];
    let tries = futures_util::future::join_all((0..12).map(|i| try_login(format!("nobody{i}"), forwarded[i % 3]))).await;
    let statuses: Vec<u16> = tries.iter().map(|r| r.as_ref().unwrap().status().as_u16()).collect();
    assert_eq!(statuses.iter().filter(|s| **s == 401).count(), 9, "{statuses:?}");
    assert_eq!(statuses.iter().filter(|s| **s == 429).count(), 3, "{statuses:?}");
    let blocked = tries.iter().map(|r| r.as_ref().unwrap()).find(|r| r.status() == 429).unwrap();
    assert!(blocked.headers()["retry-after"].to_str().unwrap().parse::<u64>().unwrap() >= 1);

    // Posting too fast is refused until it slows down.
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let mut refused = 0;
    for i in 0..40 {
        let new = NewPost { channel_id: town.clone(), message: format!("spam {i}"), ..Default::default() };
        if let Err(e) = alice.create_post(&new).await {
            assert!(format!("{e:?}").contains("429"), "{e:?}");
            refused += 1;
        }
    }
    assert!(refused > 0);
}

#[tokio::test]
async fn https_with_certificate_files() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let certs = tempfile::tempdir().unwrap();
    let made = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let (cert, key) = (certs.path().join("cert.pem"), certs.path().join("key.pem"));
    std::fs::write(&cert, made.cert.pem()).unwrap();
    std::fs::write(&key, made.signing_key.serialize_pem()).unwrap();
    let (c2, k2) = (cert.clone(), key.clone());
    let s = server_with(move |c| {
        c.tls_cert = Some(c2);
        c.tls_key = Some(k2);
    })
    .await;
    let addr = s.url.trim_start_matches("http://").to_string();

    // HTTPS answers, and says to stay on HTTPS.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(made.cert.der().clone()).unwrap();
    let client = rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
    let mut tls = tokio_rustls::TlsConnector::from(std::sync::Arc::new(client)).connect(name, tcp).await.unwrap();
    tls.write_all(b"GET /api/v4/system/ping HTTP/1.0\r\nHost: localhost\r\n\r\n").await.unwrap();
    let mut reply = Vec::new();
    let _ = tls.read_to_end(&mut reply).await;
    let reply = String::from_utf8_lossy(&reply).to_lowercase();
    assert!(reply.starts_with("http/1.0 200") || reply.starts_with("http/1.1 200"), "{reply}");
    assert!(reply.contains("strict-transport-security: max-age=31536000"));

    // Plain HTTP on the same port gets nothing useful.
    let mut plain = tokio::net::TcpStream::connect(&addr).await.unwrap();
    plain.write_all(b"GET /api/v4/system/ping HTTP/1.0\r\n\r\n").await.unwrap();
    let mut got = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(2), plain.read_to_end(&mut got)).await;
    assert!(!String::from_utf8_lossy(&got).contains("200 OK"));

    // `axiond ping` (the health check) speaks HTTPS and checks the certificate.
    let ping = tokio::process::Command::new(env!("CARGO_BIN_EXE_axiond"))
        .arg("ping")
        .env("AXIOND_LISTEN", &addr)
        .env("AXIOND_TLS_CERT", &cert)
        .env("AXIOND_TLS_KEY", &key)
        .output()
        .await
        .unwrap();
    assert!(ping.status.success(), "{}", String::from_utf8_lossy(&ping.stderr));
}

#[tokio::test]
async fn backup_and_restore() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let picture = png(64, 48);
    let file = alice.upload_file(&town, "cat.png", picture.clone()).await.unwrap().file_infos.remove(0);
    let post = alice
        .create_post(&NewPost { channel_id: town.clone(), message: "keep this".into(), file_ids: vec![file.id.clone()], ..Default::default() })
        .await
        .unwrap();

    // Backed up while the server runs.
    let out = tempfile::tempdir().unwrap();
    let archive = axiond::backup::create(&s.app.db, &s.app.config.data_dir, out.path()).await.unwrap();
    assert!(archive.file_name().unwrap().to_string_lossy().starts_with("axiond-"));
    #[cfg(unix)]
    assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&archive).unwrap().permissions()) & 0o777, 0o600);

    // Restored somewhere new, it serves the same messages and files.
    let restored = tempfile::tempdir().unwrap();
    let data = restored.path().join("data");
    axiond::backup::restore(&archive, &data).unwrap();
    assert!(axiond::backup::restore(&archive, &data).is_err(), "never over an existing database");
    let app = App::open(Config { data_dir: data.clone(), ..Config::default() }).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(app.serve(listener));
    let mut again = Client::new(&url).unwrap();
    again.login("alice", PASSWORD, None).await.unwrap();
    let thread = again.thread(&post.id).await.unwrap();
    assert_eq!(thread.posts[&post.id].message, "keep this");
    assert_eq!(again.file(&file.id).await.unwrap(), picture);
    assert!(again.file_thumbnail(&file.id).await.is_ok());

    // A backup that tries to write outside the data folder is refused.
    let evil = out.path().join("evil.tar.gz");
    {
        let gz = flate2::write::GzEncoder::new(std::fs::File::create(&evil).unwrap(), flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        let mut header = tar::Header::new_gnu();
        let name = b"files/../../escaped";
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_size(4);
        header.set_mode(0o600);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        tar.append(&header, &b"evil"[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
    }
    let target = out.path().join("target");
    assert!(axiond::backup::restore(&evil, &target).is_err());
    assert!(!out.path().join("escaped").exists());
}

/// The account calls Darkmatter's settings use, through its own client.
#[tokio::test]
async fn account_calls_from_darkmatter() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    assert_eq!(alice.axiond().await.unwrap().map(|i| i.server), Some("axiond".to_string()));
    let me = alice.me().await.unwrap();
    assert!(!me.mfa_active);

    let (code, _) = admin::invite(&s.app, 1, 7, "").await.unwrap();
    let anon = Client::new(&s.url).unwrap();
    let carol = anon.sign_up(&code, "carol", "carol@example.com", PASSWORD).await.unwrap();
    assert_eq!(carol.username, "carol");
    assert!(anon.sign_up(&code, "dave", "dave@example.com", PASSWORD).await.is_err());
    let carol_client = login(&s, "carol").await;

    let sessions = alice.sessions(&me.id).await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].is_current());
    assert_eq!(sessions[0].app(), "Darkmatter/test (linux)");

    let secret = alice.mfa_generate(&me.id).await.unwrap();
    assert!(secret.url.starts_with("otpauth://") && !secret.secret.is_empty());
    assert!(alice.mfa_activate(&me.id, true, "000000").await.is_err());

    alice.set_blocked(&carol.id, true).await.unwrap();
    assert_eq!(alice.blocked().await.unwrap(), std::slice::from_ref(&carol.id));
    alice.set_blocked(&carol.id, false).await.unwrap();
    assert!(alice.blocked().await.unwrap().is_empty());
    alice.report("", &carol.id, "testing").await.unwrap();
    assert_eq!(admin::open_reports(&s.app).await.unwrap().len(), 1);

    assert!(carol_client.delete_account("wrong", false).await.is_err());
    carol_client.delete_account(PASSWORD, false).await.unwrap();
    assert!(carol_client.me().await.unwrap_err().is_unauthorized());
    let _ = alice.revoke_session(&me.id, &sessions[0].id).await;
    assert!(alice.me().await.unwrap_err().is_unauthorized());
}

#[tokio::test]
async fn housekeeping_removes_deleted_files() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let posted = alice.upload_file(&town, "a.png", png(8, 8)).await.unwrap().file_infos.remove(0);
    let post = alice.create_post(&NewPost { channel_id: town.clone(), file_ids: vec![posted.id.clone()], ..Default::default() }).await.unwrap();
    let unposted = alice.upload_file(&town, "b.png", png(8, 8)).await.unwrap().file_infos.remove(0);
    let stored = |id: &str| s.app.config.data_dir.join("files").join(&id[..2]).join(id);
    assert!(stored(&posted.id).exists() && stored(&unposted.id).exists());

    // Fresh deletes and uploads are kept for now.
    alice.delete_post(&post.id).await.unwrap();
    assert_eq!(axiond::housekeeping::run(&s.app).await.unwrap(), 0);

    // Once old enough, their bytes and thumbnails go.
    sqlx::query("UPDATE files SET delete_at = delete_at - 8 * 86400000, create_at = create_at - 2 * 86400000").execute(&s.app.db).await.unwrap();
    assert_eq!(axiond::housekeeping::run(&s.app).await.unwrap(), 2);
    assert!(!stored(&posted.id).exists() && !stored(&unposted.id).exists());
    assert!(!s.app.config.data_dir.join("files").join(&posted.id[..2]).join(format!("{}_thumb.jpg", posted.id)).exists());
}

/// Databases made by released versions of axiond, oldest first. Each must
/// open with today's code (its migrations run) and still hold its messages.
/// Make one for a new release with
/// `AXIOND_FIXTURE=x.y.z cargo test -p axiond --test darkmatter_client make_upgrade_fixture -- --ignored`.
const UPGRADE_FIXTURES: &[&str] = &["0.1.0"];

#[tokio::test]
#[ignore = "writes tests/fixtures/axiond-<version>.db; run by hand for a release"]
async fn make_upgrade_fixture() {
    let version = std::env::var("AXIOND_FIXTURE").expect("set AXIOND_FIXTURE to the release, e.g. 0.1.0");
    let s = server().await;
    let alice = login(&s, "alice").await;
    let bob = login(&s, "bob").await;
    let bob_id = bob.me().await.unwrap().id;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let rust = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "rust").unwrap().id;
    let root = alice.create_post(&NewPost { channel_id: rust.clone(), message: "fixture: a thread @bob".into(), ..Default::default() }).await.unwrap();
    bob.create_post(&NewPost { channel_id: rust.clone(), root_id: root.id.clone(), message: "fixture: a reply".into(), ..Default::default() }).await.unwrap();
    bob.add_reaction(&bob_id, &root.id, "+1").await.unwrap();
    let file = alice.upload_file(&rust, "fixture.png", png(4, 4)).await.unwrap().file_infos.remove(0);
    alice.create_post(&NewPost { channel_id: rust.clone(), message: "fixture: a file".into(), file_ids: vec![file.id], ..Default::default() }).await.unwrap();
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/axiond-{version}.db"));
    axiond::backup::snapshot_db(&s.app.db, &out).await.unwrap();
    println!("wrote {}", out.display());
}

#[tokio::test]
async fn databases_from_older_versions_open() {
    for version in UPGRADE_FIXTURES {
        let dir = tempfile::tempdir().unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/axiond-{version}.db"));
        std::fs::copy(&fixture, dir.path().join("axiond.db")).unwrap();
        let app = App::open(Config { data_dir: dir.path().into(), ..Config::default() }).await.unwrap_or_else(|e| panic!("{version}: {e:#}"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(app.serve(listener));
        let mut alice = Client::new(&url).unwrap();
        alice.login("alice", PASSWORD, None).await.unwrap_or_else(|e| panic!("{version}: {e}"));
        let team = alice.my_teams().await.unwrap()[0].id.clone();
        let rust = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "rust").unwrap().id;
        let page = alice.posts(&rust, PostsQuery::Page { page: 0, per_page: 50 }, true).await.unwrap();
        let messages: Vec<&str> = page.chronological().iter().map(|p| p.message.as_str()).collect();
        assert_eq!(messages, ["fixture: a thread @bob", "fixture: a file"], "{version}");
        let root = page.chronological()[0].clone();
        assert_eq!((root.reply_count, root.metadata.reactions.len()), (1, 1), "{version}");
        assert_eq!(page.chronological()[1].metadata.files.len(), 1, "{version}");
        assert!(alice.search_posts(&team, "reply").await.unwrap().order.len() == 1, "{version}: search index");
    }
}

#[tokio::test]
async fn a_retried_message_is_written_once() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let new = NewPost { channel_id: town.clone(), message: "once".into(), pending_post_id: "me:123".into(), ..Default::default() };
    let first = alice.create_post(&new).await.unwrap();
    let again = alice.create_post(&new).await.unwrap();
    assert_eq!(first.id, again.id);
    assert_eq!(again.pending_post_id, "me:123");
    let page = alice.posts(&town, PostsQuery::Page { page: 0, per_page: 10 }, false).await.unwrap();
    assert_eq!(page.posts.values().filter(|p| p.message == "once").count(), 1);
    // Someone else's id never collides with yours.
    let bob = login(&s, "bob").await;
    let bobs = bob.create_post(&new).await.unwrap();
    assert_ne!(bobs.id, first.id);
}

/// In a plain channel the sender's link preview is kept, checked: its link
/// must be in the message, and its picture a JPEG. Nothing else is kept.
#[tokio::test]
async fn plain_link_previews_are_checked() {
    let s = server().await;
    let alice = login(&s, "alice").await;
    let team = alice.my_teams().await.unwrap()[0].id.clone();
    let town = alice.my_channels(&team).await.unwrap().into_iter().find(|c| c.name == "town-square").unwrap().id;
    let post = |message: &str, preview: serde_json::Value| NewPost { channel_id: town.clone(), message: message.into(), props: serde_json::json!({ "dm_preview": preview, "other": 1 }), ..Default::default() };
    let jpeg = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [0xff, 0xd8, 0xff, 0xe0, 0, 0]);
    let sent = alice
        .create_post(&post("see https://example.com/a", serde_json::json!({ "url": "https://example.com/a", "title": "A", "image": jpeg, "extra": "x" })))
        .await
        .unwrap();
    assert_eq!(sent.props["dm_preview"]["title"], "A");
    assert!(sent.props.get("other").is_none() && sent.props["dm_preview"].get("extra").is_none());
    // Not for a link in the message, or not a JPEG: refused.
    assert!(alice.create_post(&post("no link here", serde_json::json!({ "url": "https://example.com/a", "title": "A" }))).await.is_err());
    let png = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, b"\x89PNG....");
    assert!(alice.create_post(&post("see https://example.com/b", serde_json::json!({ "url": "https://example.com/b", "image": png }))).await.is_err());
}
