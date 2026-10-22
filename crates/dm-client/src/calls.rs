//! The encryption of calls (M3 Phase 2): each call's own MLS group, which
//! this device joins as it joins the call, follows while in it, and forgets
//! after. Its media keys come from there (`dm_crypto::call`).

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use dm_crypto::call::{self, Keys as CallKeys};
use mm_api::models::NewHandshake;

use crate::{E2ee, Keys, LocalStore, RETRIES, conflict, not_found, s};

impl<S: LocalStore> E2ee<S> {
    /// Joins the encryption of the call this device just joined in
    /// `channel_id`: sets up its group, or joins it by itself. Returns the
    /// call's keys as they are now.
    pub async fn join_call(&self, channel_id: &str, call_id: &str) -> Result<CallKeys, String> {
        let _held = self.lock.lock().await;
        let group = call::group(call_id);
        let mut keys = self.call_people(channel_id).await?;
        for _ in 0..RETRIES {
            match self.api.call_group(channel_id).await {
                Err(e) if not_found(&e) => {
                    let info = self.device.create_group(&group, &self.server).map_err(s)?;
                    match self.api.create_call_group(channel_id, &B64.encode(info)).await {
                        Ok(state) => {
                            self.set_call_cursor(&group, state.seq);
                            return self.call_keys(&group);
                        }
                        // Someone else set it up meanwhile: join theirs.
                        Err(e) if conflict(&e) => self.device.forget(&group).map_err(s)?,
                        Err(e) => {
                            self.device.forget(&group).map_err(s)?;
                            return Err(s(e));
                        }
                    }
                }
                Err(e) => return Err(s(e)),
                Ok(state) => {
                    let info = B64.decode(&state.group_info).map_err(s)?;
                    let h = match self.device.join_external(&info, &self.server, &keys) {
                        Ok((_, h)) => h,
                        // Someone in it whose keys weren't fetched (just joined).
                        Err(dm_crypto::Error::UnknownDevice { user, .. }) => {
                            keys.extend(self.keys(std::slice::from_ref(&user)).await?);
                            continue;
                        }
                        Err(e) => return Err(s(e)),
                    };
                    let body = NewHandshake { commit: B64.encode(&h.commit), group_info: B64.encode(&h.group_info), welcome: String::new() };
                    match self.api.post_call_handshake(channel_id, &body).await {
                        Ok(entry) => {
                            self.device.confirm(&group).map_err(s)?;
                            self.set_call_cursor(&group, entry.seq);
                            return self.call_keys(&group);
                        }
                        // The group moved on: try again from its new state.
                        Err(e) if conflict(&e) => {
                            self.device.forget(&group).map_err(s)?;
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        Err(e) => {
                            self.device.forget(&group).map_err(s)?;
                            return Err(s(e));
                        }
                    }
                }
            }
        }
        Err("the call's encryption kept changing; try again".into())
    }

    /// Applies the call group's new handshakes, and commits the server's
    /// removals of devices that left. Returns the keys if they changed.
    pub async fn sync_call(&self, channel_id: &str, call_id: &str) -> Result<Option<CallKeys>, String> {
        let _held = self.lock.lock().await;
        let group = call::group(call_id);
        if !self.device.has_group(&group) {
            return Err("not in the call's encryption".into());
        }
        let before = self.device.epoch(&group).map_err(s)?;
        let mut keys: Option<Keys> = None;
        for round in 0..RETRIES {
            self.apply_call_handshakes(channel_id, &group, &mut keys).await?;
            if !self.device.has_pending_proposals(&group) {
                break;
            }
            // Everyone left in the call hears of a removal at once: each waits
            // its own while, so most find it done by another.
            if round == 0 {
                tokio::time::sleep(self.backoff()).await;
                continue;
            }
            let Some(h) = self.device.commit_pending(&group).map_err(s)? else { break };
            let body = NewHandshake { commit: B64.encode(&h.commit), group_info: B64.encode(&h.group_info), welcome: String::new() };
            match self.api.post_call_handshake(channel_id, &body).await {
                Ok(entry) => {
                    self.device.confirm(&group).map_err(s)?;
                    self.set_call_cursor(&group, entry.seq);
                    break;
                }
                Err(e) if conflict(&e) => self.device.discard(&group).map_err(s)?,
                Err(e) => {
                    self.device.discard(&group).map_err(s)?;
                    return Err(s(e));
                }
            }
        }
        let now = self.device.epoch(&group).map_err(s)?;
        Ok(if now != before { Some(self.call_keys(&group)?) } else { None })
    }

    /// Done with a call: its group goes.
    pub fn leave_call(&self, call_id: &str) {
        let group = call::group(call_id);
        let _ = self.device.forget(&group);
        self.call_cursors.lock().unwrap().remove(&group);
        self.persist();
    }

    async fn apply_call_handshakes(&self, channel_id: &str, group: &str, keys: &mut Option<Keys>) -> Result<(), String> {
        let cursor = self.call_cursors.lock().unwrap().get(group).copied().unwrap_or(0);
        let page = self.api.call_handshakes(channel_id, cursor).await.map_err(s)?;
        for h in page {
            if keys.is_none() {
                *keys = Some(self.call_people(channel_id).await?);
            }
            let bytes = B64.decode(&h.data).map_err(s)?;
            if let Err(e) = self.process_with_keys(group, &bytes, keys.as_mut().unwrap()).await {
                let behind = self.device.epoch(group).is_ok_and(|now| h.epoch < now);
                if !(self.device.is_member(group) && behind) {
                    // Out of step with the call: join it again from scratch.
                    let _ = self.device.forget(group);
                    return Err(format!("lost track of the call's encryption ({e})"));
                }
            }
            self.set_call_cursor(group, h.seq);
        }
        if !self.device.is_member(group) {
            let _ = self.device.forget(group);
            return Err("taken out of the call's encryption".into());
        }
        Ok(())
    }

    /// The keys of the people in the channel's call, refusing anyone whose
    /// key changed since they were verified (as sending to them would be).
    async fn call_people(&self, channel_id: &str) -> Result<Keys, String> {
        let calls = self.api.calls().await.map_err(s)?;
        let users: Vec<String> = calls.into_iter().filter(|c| c.channel_id == channel_id).flat_map(|c| c.participants).map(|p| p.user_id).collect();
        let keys = self.keys(&users).await?;
        if !keys.blocked.is_empty() {
            let names: Vec<String> = match self.api.users_by_ids(&keys.blocked).await {
                Ok(users) => users.into_iter().map(|u| format!("@{}", u.username)).collect(),
                Err(_) => keys.blocked.clone(),
            };
            return Err(format!("{}'s identity key changed since you verified it: compare the new safety number before calling", names.join(", ")));
        }
        Ok(keys)
    }

    fn call_keys(&self, group: &str) -> Result<CallKeys, String> {
        self.persist();
        self.device.call_keys(group).map_err(s)
    }

    fn set_call_cursor(&self, group: &str, seq: i64) {
        self.call_cursors.lock().unwrap().insert(group.to_string(), seq);
    }

    /// 100–600 ms, the same for this device each time.
    fn backoff(&self) -> Duration {
        let n = self.device.device_id.bytes().fold(0u64, |n, b| n.wrapping_mul(31).wrapping_add(b as u64));
        Duration::from_millis(100 + n % 500)
    }
}
