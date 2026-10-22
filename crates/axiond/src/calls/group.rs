//! Each call's own MLS group (`call:<id>`), as the server tracks it: its
//! public state and a log of handshakes, in memory with the call. The same
//! delivery-service checks as a channel's group (`dm_crypto::ds`), with
//! the call's rules on top:
//! - only devices in the call are in it, and each joins by itself
//!   (external commit): nobody adds anyone;
//! - a device that leaves the call is proposed for removal by the server,
//!   and a member commits that; nobody else takes out a device still in
//!   the call.
//!
//! Media keys come from the group (`dm_crypto::call`), so the server, which
//! sees only public state, can't derive them.

use std::collections::{HashMap, HashSet};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use dm_crypto::ds::{Refusal, Server};
use mm_api::models::{GroupState, HandshakeEntry, NewHandshake};
use openmls_memory_storage::MemoryStorage;
use serde_json::json;

use super::{Call, Member};
use crate::App;
use crate::auth::Me;
use crate::e2ee::Keys;
use crate::error::{Error, Result};
use crate::hub::Event;

/// The handshakes kept per call: a device that missed more joins again.
const LOG: usize = 512;

pub struct Group {
    /// openmls's rows for the group's public state.
    values: HashMap<Vec<u8>, Vec<u8>>,
    group_info: Vec<u8>,
    epoch: u64,
    log: Vec<HandshakeEntry>,
    next_seq: i64,
}

/// The server's own key rows, loaded once: what [`Server`] needs to sign
/// removals and recognize itself as the external sender.
pub struct ServerRows(HashMap<Vec<u8>, Vec<u8>>);

pub async fn server_rows(app: &App) -> anyhow::Result<ServerRows> {
    let mut conn = app.db.acquire().await?;
    let (server, _) = crate::e2ee::load(&mut conn, app, "").await?;
    Ok(ServerRows(server.storage().values.read().unwrap().clone()))
}

fn ds(app: &App, rows: &ServerRows, group: Option<&Group>) -> Result<Server<MemoryStorage>> {
    let storage = MemoryStorage::default();
    {
        let mut values = storage.values.write().unwrap();
        values.extend(rows.0.iter().map(|(k, v)| (k.clone(), v.clone())));
        if let Some(g) = group {
            values.extend(g.values.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
    }
    Server::open(&app.e2ee.server_key, storage).map_err(|e| {
        tracing::error!("the server's encryption key: {e}");
        Error::internal()
    })
}

/// The group's rows after an operation: everything but the server's own.
fn rows_of(server: &Server<MemoryStorage>, rows: &ServerRows) -> HashMap<Vec<u8>, Vec<u8>> {
    server.storage().values.read().unwrap().iter().filter(|(k, _)| !rows.0.contains_key(*k)).map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn refused(e: dm_crypto::Error) -> Error {
    Error::bad_request(format!("Refused: {e}"))
}

impl Group {
    fn push(&mut self, epoch: u64, kind: &str, data: &[u8]) -> HandshakeEntry {
        let entry = HandshakeEntry { seq: self.next_seq, epoch, kind: kind.into(), data: B64.encode(data) };
        self.next_seq += 1;
        self.log.push(entry.clone());
        if self.log.len() > LOG {
            self.log.remove(0);
        }
        entry
    }
}

impl Call {
    fn group_id(&self) -> String {
        dm_crypto::call::group(&self.id)
    }

    fn is_in(&self, me: &Me) -> bool {
        self.members.iter().any(|m| m.user_id == me.user_id && m.device_id == me.device_id)
    }

    /// The server proposes taking `gone` out of the call's group, if it's
    /// in it. Returns the proposal's place in the log.
    pub(super) fn propose_removal(&mut self, app: &App, gone: &Member) -> Option<i64> {
        let rows = app.calls.rows.get()?;
        let group_id = self.group_id();
        let group = self.group.as_mut()?;
        let server = ds(app, rows, Some(group)).ok()?;
        let device = (gone.user_id.clone(), gone.device_id.clone());
        if !server.members(&group_id).ok()?.contains(&device) {
            return None;
        }
        match server.propose_remove(&group_id, &device) {
            Ok(bytes) => {
                group.values = rows_of(&server, rows);
                Some(group.push(group.epoch, "proposal", &bytes).seq)
            }
            Err(e) => {
                tracing::warn!("couldn't propose taking {}:{} out of call {}: {e}", device.0, device.1, self.id);
                None
            }
        }
    }
}

/// The call this device is in, in `channel_id`.
fn with_call<T>(app: &App, me: &Me, channel_id: &str, f: impl FnOnce(&mut Call) -> Result<T>) -> Result<T> {
    let mut calls = app.calls.calls.lock().unwrap();
    let call = calls.get_mut(channel_id).filter(|c| c.is_in(me)).ok_or_else(|| Error::not_found("Call"))?;
    f(call)
}

fn participants(app: &App, me: &Me, channel_id: &str) -> Result<Vec<String>> {
    with_call(app, me, channel_id, |c| Ok(c.members.iter().map(|m| m.user_id.clone()).collect()))
}

/// The group's latest state, to join it by external commit.
pub async fn state(app: &App, me: &Me, channel_id: &str) -> Result<GroupState> {
    with_call(app, me, channel_id, |call| {
        let g = call.group.as_ref().ok_or_else(|| Error::not_found("Call group"))?;
        Ok(GroupState { epoch: g.epoch, group_info: B64.encode(&g.group_info), seq: g.next_seq - 1 })
    })
}

/// The first device to start the call's encryption registers its group.
pub async fn register(app: &App, me: &Me, channel_id: &str, group_info: &str) -> Result<()> {
    let info = B64.decode(group_info.trim()).map_err(|_| Error::bad_request("The group info isn't valid base64."))?;
    let keys = {
        let mut conn = app.db.acquire().await?;
        Keys::of(&mut conn, std::slice::from_ref(&me.user_id)).await?
    };
    let rows = app.calls.rows.get().ok_or_else(Error::internal)?;
    with_call(app, me, channel_id, |call| {
        if call.group.is_some() {
            return Err(Error::conflict("The call's encryption is set up already: join it."));
        }
        let server = ds(app, rows, None)?;
        server.register(&call.group_id(), &info, &(me.user_id.clone(), me.device_id.clone()), &keys).map_err(refused)?;
        call.group = Some(Group { values: rows_of(&server, rows), group_info: info, epoch: 0, log: Vec::new(), next_seq: 1 });
        Ok(())
    })
}

/// A commit from a device in the call: its own joining, or the server's
/// removals.
pub async fn commit(app: &App, me: &Me, channel_id: &str, body: &NewHandshake) -> Result<HandshakeEntry> {
    let decode = |s: &str, what: &str| B64.decode(s.trim()).map_err(|_| Error::bad_request(format!("The {what} isn't valid base64.")));
    let (commit, info) = (decode(&body.commit, "commit")?, decode(&body.group_info, "group info")?);
    let users = participants(app, me, channel_id)?;
    let keys = {
        let mut conn = app.db.acquire().await?;
        Keys::of(&mut conn, &users).await?
    };
    let rows = app.calls.rows.get().ok_or_else(Error::internal)?;
    let (entry, notify) = with_call(app, me, channel_id, |call| {
        let group_id = call.group_id();
        let in_call: HashSet<(String, String)> = call.members.iter().map(|m| (m.user_id.clone(), m.device_id.clone())).collect();
        let group = call.group.as_mut().ok_or_else(|| Error::not_found("Call group"))?;
        let server = ds(app, rows, Some(group))?;
        let sender = (me.user_id.clone(), me.device_id.clone());
        let in_call_user = |u: &str| in_call.iter().any(|(user, _)| user == u);
        let applied = match server.commit_call(&group_id, &sender, &commit, &info, in_call_user, &keys) {
            Ok(applied) => applied,
            Err(Refusal::Stale { current }) => return Err(Error::conflict(format!("The call's group is at epoch {current} now: catch up and try again."))),
            Err(Refusal::Invalid(e)) => return Err(refused(e)),
        };
        if !applied.external && !applied.added.is_empty() {
            return Err(Error::bad_request("Nobody is added to a call: each device joins by itself."));
        }
        if applied.external && applied.added != vec![sender.clone()] {
            return Err(Error::bad_request("A device joins a call only as itself."));
        }
        if let Some(d) = applied.removed.iter().find(|d| in_call.contains(*d) && **d != sender) {
            return Err(Error::bad_request(format!("{}:{} is still in the call.", d.0, d.1)));
        }
        let made_in = group.epoch;
        group.values = rows_of(&server, rows);
        group.group_info = info;
        group.epoch = applied.epoch;
        let entry = group.push(made_in, "commit", &commit);
        // Removals this join couldn't carry: proposed again, for the new epoch.
        for (user_id, device_id) in applied.still_to_remove {
            let _ = call.propose_removal(app, &Member { peer: 0, user_id, device_id, joined_at: 0 });
        }
        let others: Vec<String> = call.members.iter().filter(|m| m.user_id != me.user_id).map(|m| m.user_id.clone()).collect();
        Ok((entry, others))
    })?;
    tell(app, channel_id, &notify, entry.seq);
    Ok(entry)
}

/// What a device in the call missed, after `after`.
pub async fn handshakes(app: &App, me: &Me, channel_id: &str, after: i64) -> Result<Vec<HandshakeEntry>> {
    with_call(app, me, channel_id, |call| {
        let g = call.group.as_ref().ok_or_else(|| Error::not_found("Call group"))?;
        if g.log.first().is_some_and(|e| e.seq > after + 1) {
            return Err(Error::conflict("Too far behind: join the call's group again."));
        }
        Ok(g.log.iter().filter(|e| e.seq > after).cloned().collect())
    })
}

/// Tells the people in a call its group's log grew.
pub(super) fn tell(app: &App, channel_id: &str, users: &[String], seq: i64) {
    let event = Event::new("call_mls", json!({ "channel_id": channel_id, "seq": seq })).channel(channel_id);
    app.hub.send(users, &event);
}
