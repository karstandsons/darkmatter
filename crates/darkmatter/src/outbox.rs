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
    /// Waiting for its link's preview before it goes. Not kept: after a
    /// restart it goes without.
    #[serde(skip)]
    held: bool,
}

/// One account's queue, kept in its encrypted database ([`crate::vault`]).
#[derive(Default)]
pub struct Outbox {
    pub items: Vec<Queued>,
    /// No vault (tests, or it couldn't be opened): kept in memory only.
    vault: Option<crate::vault::Writer>,
}

/// Where versions before the vault kept every server's queue, as JSON.
fn legacy_path() -> Option<PathBuf> {
    Some(dirs::state_dir().or_else(dirs::data_local_dir)?.join("darkmatter/outbox.json"))
}

fn read_all(path: &std::path::Path) -> HashMap<String, Vec<Queued>> {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Takes `server`'s messages out of the old JSON file (removing the file when
/// nothing is left in it), to move them into the vault.
pub fn take_legacy(server: &str) -> Vec<Queued> {
    let Some(path) = legacy_path() else { return Vec::new() };
    take_legacy_from(&path, server)
}

fn take_legacy_from(path: &std::path::Path, server: &str) -> Vec<Queued> {
    let mut all = read_all(path);
    let Some(items) = all.remove(server) else { return Vec::new() };
    let result = if all.is_empty() { std::fs::remove_file(path) } else { serde_json::to_vec(&all).map_err(std::io::Error::other).and_then(|b| std::fs::write(path, b)) };
    if let Err(e) = result {
        tracing::warn!("couldn't update the old outbox file: {e}");
    }
    items
}

/// How long to wait before try `attempts + 1`: 2 s, 4 s, 8 s… up to a minute.
fn backoff(attempts: u32) -> Duration {
    Duration::from_secs(2u64.saturating_pow(attempts.clamp(1, 6)).min(60))
}

impl Outbox {
    /// The queue as the vault had it.
    pub fn new(items: Vec<Queued>, vault: Option<crate::vault::Writer>) -> Self {
        Outbox { items, vault }
    }

    fn save(&self) {
        if let Some(vault) = &self.vault {
            vault.outbox(self.items.clone());
        }
    }

    pub fn push(&mut self, post: NewPost) {
        self.add(post, false);
    }

    /// Like [`Outbox::push`], but it waits for [`Outbox::release`] (its
    /// link's preview) before it's due. It's kept meanwhile, so a crash or
    /// a slow site can't lose it.
    pub fn hold(&mut self, post: NewPost) {
        self.add(post, true);
    }

    fn add(&mut self, post: NewPost, held: bool) {
        self.items.push(Queued {
            post,
            queued_at: chrono::Utc::now().timestamp_millis(),
            attempts: 0,
            problem: None,
            next_try: None,
            in_flight: false,
            held,
        });
        self.save();
    }

    /// A held message can go, after `change` (adding its preview). Whether
    /// it's still here.
    pub fn release(&mut self, pending_post_id: &str, change: impl FnOnce(&mut NewPost)) -> bool {
        let Some(q) = self.items.iter_mut().find(|q| q.post.pending_post_id == pending_post_id) else { return false };
        q.held = false;
        change(&mut q.post);
        self.save();
        true
    }

    /// The ones to send now, marked as being sent.
    pub fn due(&mut self) -> Vec<NewPost> {
        let now = Instant::now();
        self.items
            .iter_mut()
            .filter(|q| !q.in_flight && !q.held && q.next_try.is_none_or(|t| now >= t))
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
        // 409: an encrypted conversation's keys moved on meanwhile.
        mm_api::Error::Api(api) => matches!(api.status_code, 0 | 408 | 409 | 429 | 500..=599),
        mm_api::Error::Http(_) => true,
        _ => false,
    }
}

/// What went wrong, in words for under the message.
pub fn problem(e: &mm_api::Error) -> String {
    match e {
        mm_api::Error::Http(_) => "can't reach the server".into(),
        mm_api::Error::Api(api) if api.status_code == 429 => "sending too fast".into(),
        mm_api::Error::Api(api) if api.status_code == 0 || api.status_code >= 500 => "the server is having trouble".into(),
        e => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued(id: &str) -> Queued {
        Queued { post: post(id), queued_at: 0, attempts: 0, problem: None, next_try: None, in_flight: false, held: false }
    }

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

    #[test]
    fn a_held_message_waits_for_its_release_but_is_kept() {
        let mut o = Outbox::default();
        o.hold(post("a"));
        assert!(o.due().is_empty(), "waits for its preview");
        assert_eq!(o.items.len(), 1, "kept meanwhile");
        assert!(o.release("a", |p| p.message.push('!')));
        assert_eq!(o.due()[0].message, "a!");
        assert!(!o.release("gone", |_| {}));
        // After a restart (read back from the vault) it goes without.
        o.hold(post("b"));
        let saved: Vec<Queued> = serde_json::from_value(serde_json::to_value(&o.items).unwrap()).unwrap();
        let mut o = Outbox::new(saved.into_iter().filter(|q| q.post.pending_post_id == "b").collect(), None);
        assert_eq!(o.due().len(), 1);
    }

    #[tokio::test]
    async fn an_unreachable_server_is_retried_a_refusal_is_not() {
        // Nothing listens on port 9 (discard) here.
        let offline = mm_api::Client::new("http://127.0.0.1:9").unwrap().with_token("t");
        let e = offline.create_post(&post("p")).await.unwrap_err();
        assert!(retryable(&e), "{e}");
        assert_eq!(problem(&e), "can't reach the server");
        let refused = mm_api::Error::Api(mm_api::ApiError { status_code: 400, ..Default::default() });
        assert!(!retryable(&refused));
        let busy = mm_api::Error::Api(mm_api::ApiError { status_code: 503, ..Default::default() });
        assert!(retryable(&busy));
    }

    #[test]
    fn old_queues_move_out_of_the_json_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("outbox.json");
        let all = HashMap::from([("https://a".to_string(), vec![queued("x")]), ("https://b".to_string(), vec![queued("y")])]);
        std::fs::write(&file, serde_json::to_vec(&all).unwrap()).unwrap();
        assert_eq!(take_legacy_from(&file, "https://a").len(), 1);
        assert!(take_legacy_from(&file, "https://a").is_empty(), "taken once");
        assert!(read_all(&file).contains_key("https://b"));
        assert_eq!(take_legacy_from(&file, "https://b").len(), 1);
        assert!(!file.exists(), "gone once empty");
    }

}
