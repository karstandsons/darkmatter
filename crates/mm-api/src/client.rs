use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::{Method, RequestBuilder, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use url::Url;

use crate::models::*;
use crate::{ApiError, Error};

/// REST client for one Mattermost server. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    token: Option<String>,
}

/// Which slice of a channel's history to fetch.
#[derive(Clone, Debug)]
pub enum PostsQuery {
    /// Newest posts, paged backwards.
    Page { page: u32, per_page: u32 },
    /// Everything created or edited after this time (for resync after reconnect).
    Since(Millis),
    /// Older history before a given post.
    Before { post_id: Id, per_page: u32 },
}

impl Client {
    /// `server` is the site URL, e.g. `https://chat.example.com`.
    pub fn new(server: &str) -> Result<Self, Error> {
        let mut base = Url::parse(server)?;
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        Ok(Client { http: reqwest::Client::new(), base: base.join("api/v4/")?, token: None })
    }

    /// Use an existing session or personal access token.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Base URL of the API, ending in `/api/v4/`.
    pub fn api_url(&self) -> &Url {
        &self.base
    }

    /// Log in with username/email and password. Pass `mfa` when
    /// [`Error::needs_mfa`] says the account requires a code.
    pub async fn login(&mut self, login_id: &str, password: &str, mfa: Option<&str>) -> Result<User, Error> {
        let mut body = json!({ "login_id": login_id, "password": password });
        if let Some(code) = mfa {
            body["token"] = code.into();
        }
        let resp = check(self.http.post(self.url("users/login")?).json(&body).send().await?).await?;
        let token = resp
            .headers()
            .get("Token")
            .and_then(|v| v.to_str().ok())
            .ok_or(Error::MissingToken)?
            .to_string();
        let user = resp.json().await?;
        self.token = Some(token);
        Ok(user)
    }

    pub async fn logout(&mut self) -> Result<(), Error> {
        self.send_unit(Method::POST, "users/logout", None::<&()>).await?;
        self.token = None;
        Ok(())
    }

    pub async fn me(&self) -> Result<User, Error> {
        self.get("users/me").await
    }

    pub async fn my_teams(&self) -> Result<Vec<Team>, Error> {
        self.get("users/me/teams").await
    }

    /// All channels (including DMs/GMs) the user belongs to in a team.
    pub async fn my_channels(&self, team_id: &str) -> Result<Vec<Channel>, Error> {
        self.get(&format!("users/me/teams/{team_id}/channels")).await
    }

    pub async fn my_channel_members(&self, team_id: &str) -> Result<Vec<ChannelMember>, Error> {
        self.get(&format!("users/me/teams/{team_id}/channels/members")).await
    }

    pub async fn channel(&self, channel_id: &str) -> Result<Channel, Error> {
        self.get(&format!("channels/{channel_id}")).await
    }

    /// A channel's posts. With `collapsed_threads` the server returns only root
    /// posts with accurate `reply_count`s (replies are fetched per thread);
    /// without it, replies are inline and `reply_count` is always 0.
    pub async fn posts(&self, channel_id: &str, query: PostsQuery, collapsed_threads: bool) -> Result<PostList, Error> {
        let path = format!("channels/{channel_id}/posts");
        let req = self.request(Method::GET, &path)?.query(&[("collapsedThreads", collapsed_threads)]);
        let req = match query {
            PostsQuery::Page { page, per_page } => req.query(&[("page", page), ("per_page", per_page)]),
            PostsQuery::Since(since) => req.query(&[("since", since)]),
            PostsQuery::Before { post_id, per_page } => {
                req.query(&[("before", post_id), ("per_page", per_page.to_string())])
            }
        };
        Ok(check(req.send().await?).await?.json().await?)
    }

    /// The root post and all its replies.
    pub async fn thread(&self, post_id: &str) -> Result<PostList, Error> {
        self.get(&format!("posts/{post_id}/thread")).await
    }

    pub async fn create_post(&self, post: &NewPost) -> Result<Post, Error> {
        self.send(Method::POST, "posts", Some(post)).await
    }

    pub async fn edit_post(&self, post_id: &str, message: &str) -> Result<Post, Error> {
        self.send(Method::PUT, &format!("posts/{post_id}/patch"), Some(&json!({ "message": message })))
            .await
    }

    pub async fn delete_post(&self, post_id: &str) -> Result<(), Error> {
        self.send_unit(Method::DELETE, &format!("posts/{post_id}"), None::<&()>).await
    }

    /// Mark a channel read. Pass the channel being left so its state updates too.
    pub async fn view_channel(&self, channel_id: &str, prev_channel_id: Option<&str>) -> Result<(), Error> {
        let body = json!({ "channel_id": channel_id, "prev_channel_id": prev_channel_id.unwrap_or("") });
        self.send_unit(Method::POST, "channels/members/me/view", Some(&body)).await
    }

    pub async fn add_reaction(&self, user_id: &str, post_id: &str, emoji: &str) -> Result<Reaction, Error> {
        let body = json!({ "user_id": user_id, "post_id": post_id, "emoji_name": emoji });
        self.send(Method::POST, "reactions", Some(&body)).await
    }

    pub async fn remove_reaction(&self, user_id: &str, post_id: &str, emoji: &str) -> Result<(), Error> {
        let path = format!("users/{user_id}/posts/{post_id}/reactions/{emoji}");
        self.send_unit(Method::DELETE, &path, None::<&()>).await
    }

    pub async fn users_by_ids(&self, ids: &[Id]) -> Result<Vec<User>, Error> {
        self.send(Method::POST, "users/ids", Some(ids)).await
    }

    pub async fn statuses_by_ids(&self, ids: &[Id]) -> Result<Vec<Status>, Error> {
        self.send(Method::POST, "users/status/ids", Some(ids)).await
    }

    pub async fn upload_file(&self, channel_id: &str, name: &str, bytes: Vec<u8>) -> Result<UploadResult, Error> {
        let part = reqwest::multipart::Part::bytes(bytes).file_name(name.to_string());
        let form = reqwest::multipart::Form::new().text("channel_id", channel_id.to_string()).part("files", part);
        let req = self.request(Method::POST, "files")?.multipart(form);
        Ok(check(req.send().await?).await?.json().await?)
    }

    pub async fn file(&self, file_id: &str) -> Result<Vec<u8>, Error> {
        self.bytes(&format!("files/{file_id}")).await
    }

    /// Medium-size rendering of an image attachment (what chat views show inline).
    pub async fn file_preview(&self, file_id: &str) -> Result<Vec<u8>, Error> {
        self.bytes(&format!("files/{file_id}/preview")).await
    }

    pub async fn file_thumbnail(&self, file_id: &str) -> Result<Vec<u8>, Error> {
        self.bytes(&format!("files/{file_id}/thumbnail")).await
    }

    pub async fn profile_image(&self, user_id: &str) -> Result<Vec<u8>, Error> {
        self.bytes(&format!("users/{user_id}/image")).await
    }

    /// Headers for opening the WebSocket with the same credentials.
    pub(crate) fn auth_headers(&self) -> Result<HeaderMap, Error> {
        let token = self.token.as_ref().ok_or(Error::NoToken)?;
        let mut h = HeaderMap::new();
        let value = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| Error::NoToken)?;
        h.insert(AUTHORIZATION, value);
        Ok(h)
    }

    fn url(&self, path: &str) -> Result<Url, Error> {
        Ok(self.base.join(path)?)
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, Error> {
        let mut req = self.http.request(method, self.url(path)?).header("X-Requested-With", "XMLHttpRequest");
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        Ok(req)
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        Ok(check(self.request(Method::GET, path)?.send().await?).await?.json().await?)
    }

    async fn bytes(&self, path: &str) -> Result<Vec<u8>, Error> {
        Ok(check(self.request(Method::GET, path)?.send().await?).await?.bytes().await?.to_vec())
    }

    async fn send<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<T, Error> {
        Ok(self.send_raw(method, path, body).await?.json().await?)
    }

    async fn send_unit<B: Serialize + ?Sized>(&self, method: Method, path: &str, body: Option<&B>) -> Result<(), Error> {
        self.send_raw(method, path, body).await.map(drop)
    }

    async fn send_raw<B: Serialize + ?Sized>(&self, method: Method, path: &str, body: Option<&B>) -> Result<Response, Error> {
        let mut req = self.request(method, path)?;
        if let Some(body) = body {
            req = req.json(body);
        }
        check(req.send().await?).await
    }
}

/// Turn a non-2xx response into [`Error::Api`], keeping the server's message.
async fn check(resp: Response) -> Result<Response, Error> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status().as_u16();
    let mut err: ApiError = resp.json().await.unwrap_or_default();
    if err.status_code == 0 {
        err.status_code = status;
    }
    Err(Error::Api(err))
}
