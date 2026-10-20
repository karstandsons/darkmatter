//! Messages sent but not yet confirmed by the server. Each is written to disk
//! before it's sent and stays until the server has it, so going offline,
//! the server restarting or Darkmatter closing never loses one. Retries
//! carry the same `pending_post_id`, which servers use to avoid duplicates.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use mm_api::models::NewPost;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Queued {
    pub post: NewPost,
    /// Unix ms when it was sent.
    pub queued_at: i64,
    #[serde(default)]
    pub attempts: u32,
    /// The last problem, shown under the message.
    #[serde(default)]
    pub problem: Option<String>,
    #[serde(skip)]
    next_try: Option<Instant>,
    #[serde(skip)]
    pub in_flight: bool,
}

/// One server's queue. The file holds every server's, by URL.
#[derive(Default)]
pub struct Outbox {
    server: String,
    pub items: Vec<Queued>,
    /// No file (tests): kept in memory only.
    path: Option<PathBuf>,
}

fn path() -> Option<PathBuf> {
    Some(dirs::state_dir().or_else(dirs::data_local_dir)?.join("darkmatter/outbox.json"))
}

fn read_all(path: &std::path::Path) -> HashMap<String, Vec<Queued>> {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// How long to wait before try `attempts + 1`: 2 s, 4 s, 8 s… up to a minute.
fn backoff(attempts: u32) -> Duration {
    Duration::from_secs(2u64.saturating_pow(attempts.clamp(1, 6)).min(60))
}

impl Outbox {
    pub fn load(server: &str) -> Self {
        // Tests never touch the real file.
        let path = if cfg!(test) { None } else { path() };
        let items = path.as_deref().map(read_all).unwrap_or_default().remove(server).unwrap_or_default();
        Outbox { server: server.into(), items, path }
    }

    /// Writes the queue (others' servers untouched), readable only by you.
    fn save(&self) {
        let Some(path) = &self.path else { return };
        let mut all = read_all(path);
        if self.items.is_empty() {
            all.remove(&self.server);
        } else {
            all.insert(self.server.clone(), self.items.clone());
        }
        let result = (|| {
            std::fs::create_dir_all(path.parent().ok_or(std::io::ErrorKind::NotFound)?)?;
            let tmp = path.with_extension("json.part");
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            std::io::Write::write_all(&mut options.open(&tmp)?, serde_json::to_string(&all)?.as_bytes())?;
            std::fs::rename(&tmp, path)
        })();
        if let Err(e) = result {
            tracing::warn!("couldn't save the outbox: {e}");
        }
    }

    pub fn push(&mut self, post: NewPost) {
        self.items.push(Queued {
            post,
            queued_at: chrono::Utc::now().timestamp_millis(),
            attempts: 0,
            problem: None,
            next_try: None,
            in_flight: false,
        });
        self.save();
    }

    /// The ones to send now, marked as being sent.
    pub fn due(&mut self) -> Vec<NewPost> {
        let now = Instant::now();
        self.items
            .iter_mut()
            .filter(|q| !q.in_flight && q.next_try.is_none_or(|t| now >= t))
            .map(|q| {
                q.in_flight = true;
                q.post.clone()
            })
            .collect()
    }

    /// The server has it (or refused it for good): forget it.
    pub fn remove(&mut self, pending_post_id: &str) -> Option<Queued> {
        let i = self.items.iter().position(|q| q.post.pending_post_id == pending_post_id)?;
        let gone = self.items.remove(i);
        self.save();
        Some(gone)
    }

    /// It didn't get through: try again later.
    pub fn failed(&mut self, pending_post_id: &str, problem: String) {
        if let Some(q) = self.items.iter_mut().find(|q| q.post.pending_post_id == pending_post_id) {
            q.in_flight = false;
            q.attempts += 1;
            q.next_try = Some(Instant::now() + backoff(q.attempts));
            q.problem = Some(problem);
        }
        self.save();
    }

    /// The connection is back: try everything now.
    pub fn retry_now(&mut self) {
        for q in &mut self.items {
            q.next_try = None;
        }
    }

    /// Waiting messages in a channel (`root` empty) or a thread.
    pub fn waiting<'a>(&'a self, channel: &'a str, root: &'a str) -> impl Iterator<Item = &'a Queued> + 'a {
        self.items.iter().filter(move |q| q.post.channel_id == channel && q.post.root_id == root)
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Whether a failed send is worth retrying: the network or the server
/// having trouble, as opposed to the server refusing the message.
pub fn retryable(e: &mm_api::Error) -> bool {
    match e {
        mm_api::Error::Api(api) => matches!(api.status_code, 0 | 408 | 429 | 500..=599),
        mm_api::Error::Http(_) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(id: &str) -> NewPost {
        NewPost { channel_id: "c".into(), message: id.into(), pending_post_id: id.into(), ..Default::default() }
    }

    #[test]
    fn retries_back_off_and_reset_on_reconnect() {
        let mut o = Outbox::default();
        o.push(post("a"));
        assert_eq!(o.due().len(), 1);
        assert!(o.due().is_empty(), "already being sent");
        o.failed("a", "offline".into());
        assert!(o.due().is_empty(), "waits before the next try");
        o.retry_now();
        assert_eq!(o.due().len(), 1);
        assert!(o.remove("a").is_some());
        assert!(o.is_empty());
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(10), Duration::from_secs(60));
    }

    #[tokio::test]
    async fn an_unreachable_server_is_retried_a_refusal_is_not() {
        // Nothing listens on port 9 (discard) here.
        let offline = mm_api::Client::new("http://127.0.0.1:9").unwrap().with_token("t");
        let e = offline.create_post(&post("p")).await.unwrap_err();
        assert!(retryable(&e), "{e}");
        let refused = mm_api::Error::Api(mm_api::ApiError { status_code: 400, ..Default::default() });
        assert!(!retryable(&refused));
        let busy = mm_api::Error::Api(mm_api::ApiError { status_code: 503, ..Default::default() });
        assert!(retryable(&busy));
    }

    #[test]
    fn survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("outbox.json");
        let mut o = Outbox { server: "https://a".into(), items: Vec::new(), path: Some(file.clone()) };
        o.push(post("x"));
        let mut other = Outbox { server: "https://b".into(), items: Vec::new(), path: Some(file.clone()) };
        other.push(post("y"));
        let all = read_all(&file);
        assert_eq!((all["https://a"].len(), all["https://b"].len()), (1, 1));
        #[cfg(unix)]
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&file).unwrap().permissions()) & 0o777, 0o600);
        o.remove("x");
        assert!(!read_all(&file).contains_key("https://a"));
    }
}
