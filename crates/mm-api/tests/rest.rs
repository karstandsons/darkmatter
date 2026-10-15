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
    let list = client.posts("c1", PostsQuery::Page { page: 0, per_page: 2 }).await.unwrap();
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
