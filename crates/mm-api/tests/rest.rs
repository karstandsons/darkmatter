use mm_api::models::NewPost;
use mm_api::{Client, Error, PostsQuery};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn login_stores_token_and_authenticates_later_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v4/users/login"))
        .and(body_json(json!({ "login_id": "alice", "password": "pw" })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Token", "sess-123")
                .set_body_json(json!({ "id": "u1", "username": "alice" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/users/me"))
        .and(header("Authorization", "Bearer sess-123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "u1", "username": "alice", "first_name": "Alice" })))
        .mount(&server)
        .await;

    let mut client = Client::new(&server.uri()).unwrap();
    let user = client.login("alice", "pw", None).await.unwrap();
    assert_eq!(user.username, "alice");
    assert_eq!(client.token(), Some("sess-123"));
    assert_eq!(client.me().await.unwrap().display_name(), "Alice");
}

#[tokio::test]
async fn api_errors_carry_server_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v4/users/login"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "id": "mfa.validate_token.authenticate.app_error",
            "message": "Invalid MFA token.",
            "status_code": 401
        })))
        .mount(&server)
        .await;

    let err = Client::new(&server.uri()).unwrap().login("alice", "pw", None).await.unwrap_err();
    assert!(err.needs_mfa());
    assert!(err.is_unauthorized());
    let Error::Api(api) = err else { panic!() };
    assert_eq!(api.message, "Invalid MFA token.");
}

#[tokio::test]
async fn non_json_error_still_reports_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(502)).mount(&server).await;
    let err = Client::new(&server.uri()).unwrap().with_token("t").me().await.unwrap_err();
    let Error::Api(api) = err else { panic!() };
    assert_eq!(api.status_code, 502);
}

#[tokio::test]
async fn posts_are_returned_oldest_first() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/channels/c1/posts"))
        .and(query_param("page", "0"))
        .and(query_param("per_page", "2"))
        .and(query_param("collapsedThreads", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "order": ["p2", "p1"],
            "posts": {
                "p1": { "id": "p1", "message": "first", "create_at": 1 },
                "p2": { "id": "p2", "message": "second", "create_at": 2, "root_id": "p1" }
            },
            "next_post_id": "", "prev_post_id": "p0", "has_next": false
        })))
        .mount(&server)
        .await;

    let client = Client::new(&server.uri()).unwrap().with_token("t");
    let list = client.posts("c1", PostsQuery::Page { page: 0, per_page: 2 }, true).await.unwrap();
    let msgs: Vec<_> = list.chronological().iter().map(|p| p.message.as_str()).collect();
    assert_eq!(msgs, ["first", "second"]);
    assert!(list.posts["p2"].is_reply());
}

#[tokio::test]
async fn create_post_omits_empty_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v4/posts"))
        .and(body_json(json!({ "channel_id": "c1", "message": "hi", "pending_post_id": "local-1" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "p9", "message": "hi", "channel_id": "c1" })))
        .mount(&server)
        .await;

    let client = Client::new(&server.uri()).unwrap().with_token("t");
    let post = NewPost {
        channel_id: "c1".into(),
        message: "hi".into(),
        pending_post_id: "local-1".into(),
        ..Default::default()
    };
    assert_eq!(client.create_post(&post).await.unwrap().id, "p9");
}

#[tokio::test]
async fn subpath_servers_keep_their_prefix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/chat/api/v4/users/me/teams"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "id": "t1", "display_name": "Home" }])))
        .mount(&server)
        .await;

    let client = Client::new(&format!("{}/chat", server.uri())).unwrap().with_token("t");
    assert_eq!(client.my_teams().await.unwrap()[0].display_name, "Home");
}

#[tokio::test]
async fn read_only_allows_reads_and_refuses_writes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/users/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "u1" })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v4/users/ids"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(&server)
        .await;
    // Any write reaching the server fails this test.
    Mock::given(method("POST")).and(path("/api/v4/posts")).respond_with(ResponseTemplate::new(500)).expect(0).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/v4/channels/members/me/view"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let client = Client::new(&server.uri()).unwrap().with_token("t").read_only(true);
    assert_eq!(client.me().await.unwrap().id, "u1");
    assert!(client.users_by_ids(&["u1".into()]).await.is_ok());

    let post = NewPost { channel_id: "c1".into(), message: "hi".into(), ..Default::default() };
    assert!(matches!(client.create_post(&post).await, Err(Error::ReadOnly(_))));
    assert!(matches!(client.view_channel("c1", None).await, Err(Error::ReadOnly(_))));
    assert!(matches!(client.add_reaction("u1", "p1", "+1").await, Err(Error::ReadOnly(_))));
    assert!(matches!(client.delete_post("p1").await, Err(Error::ReadOnly(_))));
    assert!(matches!(client.upload_file("c1", "a.txt", vec![1]).await, Err(Error::ReadOnly(_))));
}

#[tokio::test]
async fn decode_errors_name_the_field() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/users/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "u1", "props": { "a": 5 } })))
        .mount(&server)
        .await;
    let err = Client::new(&server.uri()).unwrap().with_token("t").me().await.unwrap_err();
    let Error::Decode { endpoint, path, .. } = err else { panic!("{err:?}") };
    assert_eq!((endpoint.as_str(), path.as_str()), ("users/me", "props.a"));
}

#[tokio::test]
async fn edit_delete_and_mark_unread() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v4/posts/p1/patch"))
        .and(body_json(json!({ "message": "fixed" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "p1", "message": "fixed", "edit_at": 5 })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE")).and(path("/api/v4/posts/p1")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/v4/users/u1/posts/p1/set_unread"))
        .and(body_json(json!({ "collapsed_threads_supported": true })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "channel_id": "c1", "msg_count": 4, "msg_count_root": 3, "mention_count": 0, "last_viewed_at": 99
        })))
        .mount(&server)
        .await;

    let client = Client::new(&server.uri()).unwrap().with_token("t");
    assert_eq!(client.edit_post("p1", "fixed").await.unwrap().edit_at, 5);
    client.delete_post("p1").await.unwrap();
    let unread = client.set_unread("u1", "p1").await.unwrap();
    assert_eq!((unread.msg_count_root, unread.last_viewed_at), (3, 99));
}

#[tokio::test]
async fn people_and_channel_discovery() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/users/autocomplete"))
        .and(query_param("in_team", "t1"))
        .and(query_param("name", "bo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "users": [{ "id": "u2", "username": "bob" }] })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v4/teams/t1/channels/search"))
        .and(body_json(json!({ "term": "ru" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "id": "c9", "name": "rust", "type": "O" }])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v4/channels/direct"))
        .and(body_json(json!(["u1", "u2"])))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "d1", "type": "D", "name": "u1__u2" })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v4/channels/c9/members"))
        .and(body_json(json!({ "user_id": "u1" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "channel_id": "c9", "user_id": "u1" })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE")).and(path("/api/v4/channels/c9/members/u1")).respond_with(ResponseTemplate::new(200)).mount(&server).await;

    let client = Client::new(&server.uri()).unwrap().with_token("t");
    assert_eq!(client.autocomplete_users("t1", "bo").await.unwrap()[0].username, "bob");
    assert_eq!(client.search_channels("t1", "ru").await.unwrap()[0].name, "rust");
    assert_eq!(client.create_direct("u1", "u2").await.unwrap().dm_partner("u1"), Some("u2"));
    assert_eq!(client.join_channel("c9", "u1").await.unwrap().channel_id, "c9");
    client.leave_channel("c9", "u1").await.unwrap();

    // Search is allowed read-only; DMs and joins are not.
    let ro = Client::new(&server.uri()).unwrap().with_token("t").read_only(true);
    assert!(ro.search_channels("t1", "ru").await.is_ok());
    assert!(matches!(ro.create_direct("u1", "u2").await, Err(Error::ReadOnly(_))));
    assert!(matches!(ro.join_channel("c9", "u1").await, Err(Error::ReadOnly(_))));
}

#[tokio::test]
async fn search_and_threads() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v4/teams/t1/posts/search"))
        .and(body_json(json!({ "terms": "@alice", "is_or_search": false, "page": 0, "per_page": 40, "include_deleted_channels": false })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "order": ["p1"], "posts": { "p1": { "id": "p1", "message": "hi @alice" } } })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v4/users/me/teams/t1/threads"))
        .and(query_param("extended", "true"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total": 1, "total_unread_threads": 1, "total_unread_mentions": 0,
            "threads": [{ "id": "r1", "reply_count": 2, "participants": null, "post": { "id": "r1" }, "unread_replies": 1 }]
        })))
        .mount(&server)
        .await;
    Mock::given(method("PUT")).and(path("/api/v4/users/me/teams/t1/threads/r1/read/123")).respond_with(ResponseTemplate::new(200)).mount(&server).await;

    let client = Client::new(&server.uri()).unwrap().with_token("t");
    assert_eq!(client.search_posts("t1", "@alice").await.unwrap().order, ["p1"]);
    let threads = client.user_threads("t1").await.unwrap();
    assert_eq!((threads.total_unread_threads, threads.threads[0].unread_replies), (1, 1));
    client.mark_thread_read("t1", "r1", 123).await.unwrap();
    let ro = Client::new(&server.uri()).unwrap().with_token("t").read_only(true);
    assert!(ro.search_posts("t1", "@alice").await.is_ok());
    assert!(matches!(ro.mark_thread_read("t1", "r1", 123).await, Err(Error::ReadOnly(_))));
}

#[tokio::test]
async fn change_password_uses_mattermosts_shape() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v4/users/u1/password"))
        .and(body_json(json!({ "current_password": "old one", "new_password": "a new one" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "status": "OK" })))
        .expect(1)
        .mount(&server)
        .await;
    Client::new(&server.uri()).unwrap().with_token("t").change_password("u1", "old one", "a new one").await.unwrap();
}
