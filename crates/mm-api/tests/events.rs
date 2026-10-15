use mm_api::ws::{Event, RawEvent};

fn fixture(name: &str) -> Event {
    let path = format!("{}/../../fixtures/ws/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap();
    serde_json::from_str::<RawEvent>(&text).unwrap().into_event()
}

#[test]
fn posted_decodes_nested_post_and_mentions() {
    let Event::Posted { post, team_id, mentions } = fixture("posted") else { panic!() };
    assert_eq!(post.message, "hello @alice");
    assert_eq!(post.channel_id, "c1aaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(post.pending_post_id, "local-1");
    assert_eq!(team_id, "t1aaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(mentions, ["u1aaaaaaaaaaaaaaaaaaaaaaaa"]);
}

#[test]
fn reaction_added() {
    let Event::ReactionAdded(r) = fixture("reaction_added") else { panic!() };
    assert_eq!(r.emoji_name, "tada");
    assert_eq!(r.post_id, "p1aaaaaaaaaaaaaaaaaaaaaaaa");
}

#[test]
fn typing_takes_channel_from_broadcast() {
    let Event::Typing { channel_id, user_id, parent_id } = fixture("typing") else { panic!() };
    assert_eq!(channel_id, "c1aaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(user_id, "u2aaaaaaaaaaaaaaaaaaaaaaaa");
    assert!(parent_id.is_empty());
}

#[test]
fn status_change() {
    assert_eq!(
        fixture("status_change"),
        Event::StatusChange { user_id: "u2aaaaaaaaaaaaaaaaaaaaaaaa".into(), status: "away".into() }
    );
}

#[test]
fn unknown_events_are_kept_raw() {
    let Event::Other(raw) = fixture("unknown") else { panic!() };
    assert_eq!(raw.event, "some_future_event");
    assert_eq!(raw.seq, 7);
}

#[test]
fn nulls_from_the_server_become_defaults() {
    // Real shape of a post with a text attachment: image-only fields are null.
    let json = r#"{
        "id": "p1", "message": "notes", "file_ids": null, "props": null, "reply_count": null,
        "metadata": { "files": [ {
            "id": "f1", "name": "notes.txt", "mime_type": "text/plain; charset=utf-8",
            "has_preview_image": null, "width": null, "height": null, "mini_preview": null
        } ], "reactions": null, "embeds": null }
    }"#;
    let post: mm_api::models::Post = serde_json::from_str(json).unwrap();
    let f = &post.metadata.files[0];
    assert!(!f.has_preview_image);
    assert_eq!((f.width, f.height), (0, 0));
    assert!(post.file_ids.is_empty() && post.metadata.reactions.is_empty());

    let post: mm_api::models::Post = serde_json::from_str(r#"{"id":"p2","metadata":null}"#).unwrap();
    assert!(post.metadata.files.is_empty());
}

#[test]
fn thread_events() {
    let Event::ThreadUpdated(t) = fixture("thread_updated") else { panic!() };
    assert_eq!((t.id.as_str(), t.unread_replies, t.unread_mentions), ("r1aaaaaaaaaaaaaaaaaaaaaaaa", 2, 1));
    assert_eq!(t.participants[0].username, "bob");
    assert_eq!(t.post.message, "Thread test");
    assert_eq!(
        fixture("thread_read_changed"),
        Event::ThreadReadChanged { thread_id: "r1aaaaaaaaaaaaaaaaaaaaaaaa".into(), unread_replies: 0, unread_mentions: 0 }
    );
}
