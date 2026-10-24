//! Live events for connected apps, the way Mattermost sends them.
//!
//! Each connection numbers its events (`seq`) and keeps the last few. An app
//! that drops and comes back with its `connection_id` and the next `seq` it
//! expects gets what it missed replayed, then a `hello` with the same id. If
//! that's no longer possible it gets a new id, which tells it to refetch.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::ids;

/// Events kept per connection for replay.
const BACKLOG: usize = 256;
/// How long a dropped connection can still be resumed.
const RESUME_WINDOW: Duration = Duration::from_secs(120);
/// Events queued for a slow socket before it's cut off (it resumes from the backlog).
const QUEUE: usize = 64;
/// Sockets one person may have open at once; a new one past that closes
/// their oldest.
pub const SOCKETS_PER_USER: usize = 10;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Broadcast {
    pub user_id: String,
    pub channel_id: String,
    pub team_id: String,
}

#[derive(Clone, Debug)]
pub struct Event {
    pub name: &'static str,
    pub data: Value,
    pub broadcast: Broadcast,
}

impl Event {
    pub fn new(name: &'static str, data: Value) -> Self {
        Event { name, data, broadcast: Broadcast::default() }
    }

    pub fn channel(mut self, channel_id: &str) -> Self {
        self.broadcast.channel_id = channel_id.into();
        self
    }

    /// Meant for one user (their other apps, say).
    pub fn user(mut self, user_id: &str) -> Self {
        self.broadcast.user_id = user_id.into();
        self
    }

    pub fn team(mut self, team_id: &str) -> Self {
        self.broadcast.team_id = team_id.into();
        self
    }

    fn frame(&self, seq: i64) -> Arc<str> {
        json!({ "event": self.name, "data": self.data, "broadcast": self.broadcast, "seq": seq }).to_string().into()
    }
}

struct Conn {
    user_id: String,
    session_id: String,
    next_seq: i64,
    backlog: VecDeque<(i64, Arc<str>)>,
    /// The socket currently attached, and which attach it was.
    live: Option<(u64, mpsc::Sender<Arc<str>>)>,
    left_at: Option<Instant>,
}

impl Conn {
    fn push(&mut self, event: &Event) {
        let frame = event.frame(self.next_seq);
        self.backlog.push_back((self.next_seq, frame.clone()));
        if self.backlog.len() > BACKLOG {
            self.backlog.pop_front();
        }
        self.next_seq += 1;
        if let Some((_, tx)) = &self.live {
            // Full or closed: drop the socket. The app reconnects and the
            // backlog fills the gap.
            if tx.try_send(frame).is_err() {
                self.live = None;
                self.left_at = Some(Instant::now());
            }
        }
    }

    /// Can an app that expects `seq` next pick up from here?
    fn can_resume(&self, seq: i64) -> bool {
        let oldest = self.backlog.front().map_or(self.next_seq, |(s, _)| *s);
        (oldest..=self.next_seq).contains(&seq)
    }
}

/// A socket's view of the hub after attaching.
pub struct Attached {
    pub connection_id: String,
    pub generation: u64,
    /// Frames to send first: anything missed, then `hello`.
    pub first: Vec<Arc<str>>,
    pub rx: mpsc::Receiver<Arc<str>>,
}

#[derive(Default)]
pub struct Hub {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    conns: HashMap<String, Conn>,
    generation: u64,
}

impl Hub {
    /// Attaches a socket, resuming `resume = (connection_id, next seq)` when it can.
    pub fn attach(&self, user_id: &str, session_id: &str, resume: Option<(&str, i64)>, server_version: &str) -> Attached {
        let mut inner = self.inner.lock().unwrap();
        inner.sweep();
        inner.generation += 1;
        let generation = inner.generation;
        let (tx, rx) = mpsc::channel(QUEUE);

        let resumable = resume.filter(|(id, seq)| {
            inner.conns.get(*id).is_some_and(|c| c.user_id == user_id && c.session_id == session_id && c.can_resume(*seq))
        });
        let (connection_id, mut first) = match resumable {
            Some((id, seq)) => {
                let conn = inner.conns.get_mut(id).unwrap();
                let missed = conn.backlog.iter().filter(|(s, _)| *s >= seq).map(|(_, f)| f.clone()).collect();
                (id.to_string(), missed)
            }
            None => {
                let id = ids::new_id();
                let conn = Conn {
                    user_id: user_id.into(),
                    session_id: session_id.into(),
                    next_seq: 0,
                    backlog: VecDeque::new(),
                    live: None,
                    left_at: None,
                };
                inner.conns.insert(id.clone(), conn);
                (id, Vec::new())
            }
        };
        // Past the limit, this person's oldest sockets close.
        let mut theirs: Vec<(u64, String)> = inner
            .conns
            .iter()
            .filter(|(id, c)| c.user_id == user_id && **id != connection_id)
            .filter_map(|(id, c)| c.live.as_ref().map(|(g, _)| (*g, id.clone())))
            .collect();
        theirs.sort();
        let over = (theirs.len() + 1).saturating_sub(SOCKETS_PER_USER);
        for (_, id) in theirs.into_iter().take(over) {
            let conn = inner.conns.get_mut(&id).unwrap();
            conn.live = None;
            conn.left_at = Some(Instant::now());
        }
        let conn = inner.conns.get_mut(&connection_id).unwrap();
        // A socket the app gave up on stops getting events.
        conn.live = None;
        // `hello` is numbered like any event, so the app's count stays in step.
        let hello = Event::new("hello", json!({ "connection_id": connection_id, "server_version": server_version }));
        conn.push(&hello);
        first.push(conn.backlog.back().unwrap().1.clone());
        conn.live = Some((generation, tx));
        conn.left_at = None;
        Attached { connection_id, generation, first, rx }
    }

    /// The socket for `generation` closed; keep its events for a while in case it resumes.
    pub fn detach(&self, connection_id: &str, generation: u64) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(conn) = inner.conns.get_mut(connection_id)
            && conn.live.as_ref().is_some_and(|(g, _)| *g == generation)
        {
            conn.live = None;
            conn.left_at = Some(Instant::now());
        }
    }

    pub fn send(&self, users: &[String], event: &Event) {
        let mut inner = self.inner.lock().unwrap();
        for conn in inner.conns.values_mut().filter(|c| users.contains(&c.user_id)) {
            conn.push(event);
        }
    }

    pub fn online(&self, user_id: &str) -> bool {
        self.inner.lock().unwrap().conns.values().any(|c| c.user_id == user_id && c.live.is_some())
    }

    /// The sessions with a connection, open or resumable.
    pub fn session_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.inner.lock().unwrap().conns.values().map(|c| c.session_id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// A session ended: close its sockets and forget their events.
    pub fn end_session(&self, session_id: &str) {
        self.inner.lock().unwrap().conns.retain(|_, c| c.session_id != session_id);
    }
}

impl Inner {
    fn sweep(&mut self) {
        self.conns.retain(|_, c| c.left_at.is_none_or(|t| t.elapsed() < RESUME_WINDOW));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(frame: &str) -> i64 {
        serde_json::from_str::<Value>(frame).unwrap()["seq"].as_i64().unwrap()
    }

    #[test]
    fn a_dropped_connection_resumes_with_what_it_missed() {
        let hub = Hub::default();
        let mut a = hub.attach("u", "s", None, "t");
        assert_eq!(a.first.iter().map(|f| seq(f)).collect::<Vec<_>>(), [0]);
        let users = ["u".to_string()];
        hub.send(&users, &Event::new("posted", json!({})));
        assert_eq!(seq(&a.rx.try_recv().unwrap()), 1);
        hub.detach(&a.connection_id, a.generation);
        hub.send(&users, &Event::new("posted", json!({})));
        hub.send(&users, &Event::new("posted", json!({})));

        let b = hub.attach("u", "s", Some((&a.connection_id, 2)), "t");
        assert_eq!(b.connection_id, a.connection_id);
        assert_eq!(b.first.iter().map(|f| seq(f)).collect::<Vec<_>>(), [2, 3, 4]);
        assert!(b.first[2].contains("\"hello\""));
        // The old socket's late goodbye doesn't detach the new one.
        hub.detach(&a.connection_id, a.generation);
        assert!(hub.online("u"));
    }

    #[test]
    fn someone_elses_connection_or_a_lost_gap_starts_fresh() {
        let hub = Hub::default();
        let a = hub.attach("u", "s", None, "t");
        let other = hub.attach("v", "s2", Some((&a.connection_id, 1)), "t");
        assert_ne!(other.connection_id, a.connection_id);
        let late = hub.attach("u", "s", Some((&a.connection_id, 99)), "t");
        assert_ne!(late.connection_id, a.connection_id);
        assert_eq!(seq(&late.first[0]), 0);
    }

    #[test]
    fn a_new_socket_past_the_limit_closes_the_oldest() {
        let hub = Hub::default();
        let mut sockets: Vec<Attached> = (0..SOCKETS_PER_USER).map(|_| hub.attach("u", "s", None, "t")).collect();
        let other = hub.attach("v", "s2", None, "t");
        let newest = hub.attach("u", "s", None, "t");
        assert!(matches!(sockets[0].rx.try_recv(), Err(mpsc::error::TryRecvError::Disconnected)), "the oldest closed");
        for s in &mut sockets[1..] {
            assert!(matches!(s.rx.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        }
        let live = hub.inner.lock().unwrap().conns.values().filter(|c| c.user_id == "u" && c.live.is_some()).count();
        assert_eq!(live, SOCKETS_PER_USER);
        // The oldest can still resume, closing the next oldest.
        let back = hub.attach("u", "s", Some((&sockets[0].connection_id, 1)), "t");
        assert_eq!(back.connection_id, sockets[0].connection_id);
        assert!(matches!(sockets[1].rx.try_recv(), Err(mpsc::error::TryRecvError::Disconnected)));
        drop((other, newest));
    }

    #[test]
    fn ending_a_session_closes_its_sockets() {
        let hub = Hub::default();
        let mut a = hub.attach("u", "s", None, "t");
        let _ = a.rx.try_recv();
        hub.end_session("s");
        assert!(!hub.online("u"));
        assert!(matches!(a.rx.try_recv(), Err(mpsc::error::TryRecvError::Disconnected)));
    }
}
