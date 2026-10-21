//! `/api/v4/e2ee/…`: identity keys, device certificates, key packages and
//! the groups' handshake logs (docs/axiond/E2EE.md §15). The checks
//! themselves are in `crate::e2ee` and `dm-crypto`.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use dm_crypto::ds::Refusal;
use dm_crypto::{DeviceCertificate, IdentityKey};
use mm_api::models::{AxiondInfo, ClaimedKeyPackage, E2eeEvent, E2eeIdentity, E2eeLink, E2eeLinkApproval, GroupState, HandshakeEntry, NewE2eeEvent, NewHandshake, PendingWelcome};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{self, Me};
use crate::e2ee::{self, Keys};
use crate::error::{Error, Result};
use crate::hub::Event;
use crate::{App, db, ids};

/// How many unclaimed one-time key packages a device may keep here.
const MAX_KEY_PACKAGES: i64 = 200;

pub async fn config(State(app): State<App>) -> Json<AxiondInfo> {
    Json(AxiondInfo {
        server: "axiond".into(),
        version: crate::VERSION.into(),
        enabled: true,
        ciphersuite: u16::from(dm_crypto::CIPHERSUITE),
        external_sender: B64.encode(&app.e2ee.server_key),
    })
}

fn decode(what: &str, data: &str) -> Result<Vec<u8>> {
    B64.decode(data.trim()).map_err(|_| Error::bad_request(format!("The {what} isn't valid base64.")))
}

fn refused(e: dm_crypto::Error) -> Error {
    Error::bad_request(format!("Refused: {e}"))
}

/// Which device the signed-in session is: Darkmatter asks rather than
/// naming one at login, which Mattermost would take for a push address.
pub async fn my_device(me: Me) -> Json<Value> {
    Json(json!({ "user_id": me.user_id, "device_id": me.device_id }))
}

#[derive(Deserialize)]
pub struct NewIdentity {
    identity_key: String,
}

/// Publishes your identity key, the first time you set up encryption.
pub async fn put_identity(State(app): State<App>, me: Me, Json(body): Json<NewIdentity>) -> Result<Json<Value>> {
    let key = IdentityKey::from_slice(&decode("identity key", &body.identity_key)?).map_err(refused)?;
    let encoded = B64.encode(key.0);
    let current: Option<String> = sqlx::query_scalar("SELECT identity_key FROM e2ee_identities WHERE user_id = ?").bind(&me.user_id).fetch_optional(&app.db).await?;
    match current {
        Some(k) if k == encoded => super::ok(),
        // Replacing one (every device lost, no recovery key) comes with the
        // recovery work, and warns everyone who talks to you.
        Some(_) => Err(Error::conflict("This account already has a different identity key.")),
        None => {
            let now = ids::now();
            sqlx::query("INSERT INTO e2ee_identities (user_id, identity_key, create_at, update_at) VALUES (?, ?, ?, ?)")
                .bind(&me.user_id)
                .bind(&encoded)
                .bind(now)
                .bind(now)
                .execute(&app.db)
                .await?;
            super::ok()
        }
    }
}

pub async fn identity(State(app): State<App>, _me: Me, Path(user_id): Path<String>) -> Result<Json<E2eeIdentity>> {
    let identity_key: Option<String> = sqlx::query_scalar("SELECT identity_key FROM e2ee_identities WHERE user_id = ?").bind(&user_id).fetch_optional(&app.db).await?;
    let rows: Vec<(String, String, i64)> = sqlx::query_as("SELECT device_id, certificate, revoked_at FROM e2ee_devices WHERE user_id = ? ORDER BY create_at")
        .bind(&user_id)
        .fetch_all(&app.db)
        .await?;
    let mut out = E2eeIdentity { user_id, identity_key: identity_key.unwrap_or_default(), ..Default::default() };
    // Revoked devices' certificates stay listed: devices catching up on a
    // group's history check the commits that once added them.
    for (device, cert, revoked_at) in rows {
        if revoked_at > 0 {
            out.revoked.push(device);
        }
        if let Ok(cert) = serde_json::from_str(&cert) {
            out.devices.push(cert);
        }
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct NewDevice {
    certificate: DeviceCertificate,
}

/// Publishes this device's certificate. It must be for the signed-in
/// session's device, and verify against your identity key.
pub async fn publish_device(State(app): State<App>, me: Me, Json(body): Json<NewDevice>) -> Result<Json<Value>> {
    let cert = body.certificate;
    if cert.user_id != me.user_id || cert.device_id != me.device_id {
        return Err(Error::bad_request("A certificate can only be published from the device it's for."));
    }
    let identity: Option<String> = sqlx::query_scalar("SELECT identity_key FROM e2ee_identities WHERE user_id = ?").bind(&me.user_id).fetch_optional(&app.db).await?;
    let identity = identity.ok_or_else(|| Error::bad_request("Publish your identity key first."))?;
    let identity = IdentityKey::from_slice(&B64.decode(identity).map_err(|_| Error::internal())?).map_err(|_| Error::internal())?;
    cert.verify(&identity).map_err(refused)?;
    let existing: Option<(String, i64)> = sqlx::query_as("SELECT certificate, revoked_at FROM e2ee_devices WHERE user_id = ? AND device_id = ?")
        .bind(&me.user_id)
        .bind(&me.device_id)
        .fetch_optional(&app.db)
        .await?;
    let json = serde_json::to_string(&cert).map_err(|_| Error::internal())?;
    match existing {
        Some((_, revoked)) if revoked > 0 => Err(Error::bad_request("This device was taken out. Sign in again to start a new one.")),
        Some((old, _)) if old == json => super::ok(),
        Some(_) => Err(Error::conflict("This device already has a certificate.")),
        None => {
            sqlx::query("INSERT INTO e2ee_devices (user_id, device_id, certificate, signature_key, create_at) VALUES (?, ?, ?, ?, ?)")
                .bind(&me.user_id)
                .bind(&me.device_id)
                .bind(&json)
                .bind(B64.encode(&cert.signature_key))
                .bind(ids::now())
                .execute(&app.db)
                .await?;
            sqlx::query("DELETE FROM e2ee_links WHERE user_id = ? AND device_id = ?").bind(&me.user_id).bind(&me.device_id).execute(&app.db).await?;
            app.hub.send(&teammates(&app, &me.user_id).await?, &Event::new("device_added", json!({ "user_id": me.user_id, "device_id": me.device_id })));
            super::ok()
        }
    }
}

/// Everyone who shares a channel with `user_id` (and them).
async fn teammates(app: &App, user_id: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT DISTINCT b.user_id FROM channel_members a JOIN channel_members b ON b.channel_id = a.channel_id WHERE a.user_id = ? \
         UNION SELECT ?",
    )
    .bind(user_id)
    .bind(user_id)
    .fetch_all(&app.db)
    .await?)
}

/// Takes out another of your devices (lost, sold, not yours any more): its
/// sessions end and it leaves every group.
pub async fn revoke_device(State(app): State<App>, me: Me, Path(device_id): Path<String>) -> Result<Json<Value>> {
    let sessions: Vec<String> = sqlx::query_scalar("DELETE FROM sessions WHERE user_id = ? AND device_id = ? RETURNING id")
        .bind(&me.user_id)
        .bind(&device_id)
        .fetch_all(&app.db)
        .await?;
    for s in &sessions {
        app.hub.end_session(s);
    }
    e2ee::device_gone(&app, &me.user_id, &device_id).await?;
    super::ok()
}

#[derive(Deserialize)]
pub struct NewBackup {
    kind: String,
    data: Value,
}

/// Keeps your identity key's backup (encrypted on your device), replacing
/// the one of the same kind. Only from a device that's set up.
pub async fn put_backup(State(app): State<App>, me: Me, Json(body): Json<NewBackup>) -> Result<Json<Value>> {
    if !matches!(body.kind.as_str(), "key" | "passphrase") {
        return Err(Error::bad_request("A backup is under a key or a passphrase."));
    }
    let set_up: Option<i64> = sqlx::query_scalar("SELECT revoked_at FROM e2ee_devices WHERE user_id = ? AND device_id = ?")
        .bind(&me.user_id)
        .bind(&me.device_id)
        .fetch_optional(&app.db)
        .await?;
    if set_up != Some(0) {
        return Err(Error::bad_request("Only a device with encryption set up can back it up."));
    }
    let data = body.data.to_string();
    if data.len() > 4096 {
        return Err(Error::bad_request("That backup is too big."));
    }
    sqlx::query("INSERT INTO e2ee_backups (user_id, kind, data, update_at) VALUES (?, ?, ?, ?) ON CONFLICT (user_id, kind) DO UPDATE SET data = excluded.data, update_at = excluded.update_at")
        .bind(&me.user_id)
        .bind(&body.kind)
        .bind(&data)
        .bind(ids::now())
        .execute(&app.db)
        .await?;
    super::ok()
}

/// Your backups, for a device that lost the others.
pub async fn backups(State(app): State<App>, me: Me) -> Result<Json<Vec<Value>>> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as("SELECT kind, data, update_at FROM e2ee_backups WHERE user_id = ? ORDER BY kind").bind(&me.user_id).fetch_all(&app.db).await?;
    Ok(Json(
        rows.into_iter()
            .filter_map(|(kind, data, update_at)| Some(json!({ "kind": kind, "data": serde_json::from_str::<Value>(&data).ok()?, "update_at": update_at })))
            .collect(),
    ))
}

pub async fn delete_backup(State(app): State<App>, me: Me, Path(kind): Path<String>) -> Result<Json<Value>> {
    sqlx::query("DELETE FROM e2ee_backups WHERE user_id = ? AND kind = ?").bind(&me.user_id).bind(&kind).execute(&app.db).await?;
    super::ok()
}

#[derive(Deserialize)]
pub struct Reset {
    password: String,
    identity_key: String,
}

/// Starts over with a new identity key, when every device that had the old
/// one is gone and there's no backup. Needs your password. Every device of
/// yours is taken out, the old backups go, and the people you talk with are
/// told your key changed.
pub async fn reset_identity(State(app): State<App>, me: Me, Json(body): Json<Reset>) -> Result<Json<Value>> {
    let (username, hash): (String, String) = sqlx::query_as("SELECT username, password_hash FROM users WHERE id = ?").bind(&me.user_id).fetch_one(&app.db).await?;
    if let Some(wait) = app.guard.locked(&username) {
        return Err(Error::too_many(format!("Too many wrong passwords. Try again in {} minutes.", wait.as_secs().div_ceil(60))));
    }
    let password = body.password;
    let ok = tokio::task::spawn_blocking(move || auth::verify_password(Some(&hash), &password)).await.map_err(|_| Error::internal())?;
    if !ok {
        app.guard.failed(&username);
        return Err(Error::bad_request("That isn't your password."));
    }
    let key = IdentityKey::from_slice(&decode("identity key", &body.identity_key)?).map_err(refused)?;
    e2ee::all_devices_gone(&app, &me.user_id).await?;
    sqlx::query("DELETE FROM e2ee_backups WHERE user_id = ?").bind(&me.user_id).execute(&app.db).await?;
    sqlx::query("DELETE FROM e2ee_links WHERE user_id = ?").bind(&me.user_id).execute(&app.db).await?;
    // This session's device starts again too: it signs in as a new one.
    sqlx::query("DELETE FROM e2ee_devices WHERE user_id = ? AND device_id = ?").bind(&me.user_id).bind(&me.device_id).execute(&app.db).await?;
    let now = ids::now();
    sqlx::query("INSERT INTO e2ee_identities (user_id, identity_key, create_at, update_at) VALUES (?, ?, ?, ?) ON CONFLICT (user_id) DO UPDATE SET identity_key = excluded.identity_key, update_at = excluded.update_at")
        .bind(&me.user_id)
        .bind(B64.encode(key.0))
        .bind(now)
        .bind(now)
        .execute(&app.db)
        .await?;
    crate::audit::record(&app.db, &me.user_id, "e2ee_identity_reset", &me.user_id, "").await?;
    app.hub.send(&teammates(&app, &me.user_id).await?, &Event::new("identity_changed", json!({ "user_id": me.user_id })));
    super::ok()
}

/// A pending link is good for this long.
const LINK_TTL_MS: i64 = 60 * 60 * 1000;

#[derive(Deserialize)]
pub struct NewLink {
    signature_key: String,
    link_key: String,
}

/// A new device of yours asks to be linked: its keys, which its link code
/// is a hash of. Asking again replaces the request.
pub async fn request_link(State(app): State<App>, me: Me, Json(body): Json<NewLink>) -> Result<Json<Value>> {
    let set_up: Option<i64> = sqlx::query_scalar("SELECT revoked_at FROM e2ee_devices WHERE user_id = ? AND device_id = ?")
        .bind(&me.user_id)
        .bind(&me.device_id)
        .fetch_optional(&app.db)
        .await?;
    if set_up.is_some() {
        return Err(Error::bad_request("This device is set up already."));
    }
    let (signature_key, link_key) = (decode("signature key", &body.signature_key)?, decode("link key", &body.link_key)?);
    if signature_key.len() != 32 || link_key.len() != 32 {
        return Err(Error::bad_request("Those keys aren't the right size."));
    }
    sqlx::query("INSERT OR REPLACE INTO e2ee_links (user_id, device_id, signature_key, link_key, create_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&me.user_id)
        .bind(&me.device_id)
        .bind(B64.encode(&signature_key))
        .bind(B64.encode(&link_key))
        .bind(ids::now())
        .execute(&app.db)
        .await?;
    app.hub.send(std::slice::from_ref(&me.user_id), &Event::new("link_requested", json!({ "device_id": me.device_id })));
    super::ok()
}

/// Your new devices waiting to be linked, still signed in.
pub async fn links(State(app): State<App>, me: Me) -> Result<Json<Vec<E2eeLink>>> {
    let rows: Vec<(String, String, String, i64, String)> = sqlx::query_as(
        "SELECT l.device_id, l.signature_key, l.link_key, l.create_at, \
         (SELECT s.user_agent FROM sessions s WHERE s.user_id = l.user_id AND s.device_id = l.device_id ORDER BY s.last_activity_at DESC LIMIT 1) \
         FROM e2ee_links l WHERE l.user_id = ? AND l.certificate = '' AND l.create_at > ? AND l.device_id != ? \
         AND EXISTS (SELECT 1 FROM sessions s WHERE s.user_id = l.user_id AND s.device_id = l.device_id) ORDER BY l.create_at DESC",
    )
    .bind(&me.user_id)
    .bind(ids::now() - LINK_TTL_MS)
    .bind(&me.device_id)
    .fetch_all(&app.db)
    .await?;
    Ok(Json(rows.into_iter().map(|(device_id, signature_key, link_key, create_at, user_agent)| E2eeLink { device_id, signature_key, link_key, user_agent, create_at }).collect()))
}

#[derive(Deserialize)]
pub struct Approval {
    certificate: DeviceCertificate,
    sealed: Value,
}

/// Approves a new device from one already set up: its certificate (which
/// must vouch for the key it asked with) and the identity key sealed to it.
pub async fn approve_link(State(app): State<App>, me: Me, Path(device_id): Path<String>, Json(body): Json<Approval>) -> Result<Json<Value>> {
    let approver: Option<i64> = sqlx::query_scalar("SELECT revoked_at FROM e2ee_devices WHERE user_id = ? AND device_id = ?")
        .bind(&me.user_id)
        .bind(&me.device_id)
        .fetch_optional(&app.db)
        .await?;
    if approver != Some(0) {
        return Err(Error::bad_request("Only a device with encryption set up can link another."));
    }
    let pending: Option<(String, String)> = sqlx::query_as("SELECT signature_key, certificate FROM e2ee_links WHERE user_id = ? AND device_id = ? AND create_at > ?")
        .bind(&me.user_id)
        .bind(&device_id)
        .bind(ids::now() - LINK_TTL_MS)
        .fetch_optional(&app.db)
        .await?;
    let Some((signature_key, done)) = pending else { return Err(Error::not_found("link request")) };
    if !done.is_empty() {
        return super::ok();
    }
    let cert = body.certificate;
    if cert.user_id != me.user_id || cert.device_id != device_id || B64.encode(&cert.signature_key) != signature_key {
        return Err(Error::bad_request("That certificate isn't for the device that asked."));
    }
    let identity: Option<String> = sqlx::query_scalar("SELECT identity_key FROM e2ee_identities WHERE user_id = ?").bind(&me.user_id).fetch_optional(&app.db).await?;
    let identity = IdentityKey::from_slice(&B64.decode(identity.ok_or_else(Error::internal)?).map_err(|_| Error::internal())?).map_err(|_| Error::internal())?;
    cert.verify(&identity).map_err(refused)?;
    sqlx::query("UPDATE e2ee_links SET certificate = ?, sealed = ? WHERE user_id = ? AND device_id = ?")
        .bind(serde_json::to_string(&cert).map_err(|_| Error::internal())?)
        .bind(body.sealed.to_string())
        .bind(&me.user_id)
        .bind(&device_id)
        .execute(&app.db)
        .await?;
    app.hub.send(std::slice::from_ref(&me.user_id), &Event::new("link_approved", json!({ "device_id": device_id })));
    super::ok()
}

/// This device's approval, once another of your devices gave it.
pub async fn my_link(State(app): State<App>, me: Me) -> Result<Json<E2eeLinkApproval>> {
    let row: Option<(String, String)> = sqlx::query_as("SELECT certificate, sealed FROM e2ee_links WHERE user_id = ? AND device_id = ? AND certificate != ''")
        .bind(&me.user_id)
        .bind(&me.device_id)
        .fetch_optional(&app.db)
        .await?;
    let (certificate, sealed) = row.ok_or_else(|| Error::not_found("approval"))?;
    let parse = |s: &str| serde_json::from_str(s).map_err(|_| Error::internal());
    Ok(Json(E2eeLinkApproval { certificate: parse(&certificate)?, sealed: parse(&sealed)? }))
}

#[derive(Deserialize)]
pub struct NewKeyPackages {
    key_packages: Vec<String>,
}

/// Uploads key packages for this device. A last-resort one replaces the
/// one before it.
pub async fn upload_key_packages(State(app): State<App>, me: Me, Json(body): Json<NewKeyPackages>) -> Result<Json<Value>> {
    let signature_key: Option<String> = sqlx::query_scalar("SELECT signature_key FROM e2ee_devices WHERE user_id = ? AND device_id = ? AND revoked_at = 0")
        .bind(&me.user_id)
        .bind(&me.device_id)
        .fetch_optional(&app.db)
        .await?;
    let signature_key = B64.decode(signature_key.ok_or_else(|| Error::bad_request("Publish this device's certificate first."))?).map_err(|_| Error::internal())?;
    let mut checked = Vec::new();
    for kp in &body.key_packages {
        let bytes = decode("key package", kp)?;
        let ((user, device), key, last_resort) = dm_crypto::ds::check_key_package(&bytes).map_err(refused)?;
        if user != me.user_id || device != me.device_id || key != signature_key {
            return Err(Error::bad_request("A key package must be for this device and its certified key."));
        }
        checked.push((bytes, last_resort));
    }
    let mut tx = app.db.begin().await?;
    let now = ids::now();
    for (bytes, last_resort) in checked {
        if last_resort {
            sqlx::query("DELETE FROM e2ee_key_packages WHERE user_id = ? AND device_id = ? AND last_resort = 1").bind(&me.user_id).bind(&me.device_id).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO e2ee_key_packages (user_id, device_id, data, last_resort, create_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&me.user_id)
            .bind(&me.device_id)
            .bind(bytes)
            .bind(last_resort)
            .bind(now)
            .execute(&mut *tx)
            .await?;
    }
    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM e2ee_key_packages WHERE user_id = ? AND device_id = ? AND last_resort = 0")
        .bind(&me.user_id)
        .bind(&me.device_id)
        .fetch_one(&mut *tx)
        .await?;
    if stored > MAX_KEY_PACKAGES {
        return Err(Error::bad_request(format!("A device can keep at most {MAX_KEY_PACKAGES} key packages here.")));
    }
    tx.commit().await?;
    Ok(Json(json!({ "stored": stored })))
}

#[derive(Deserialize)]
pub struct Claim {
    user_ids: Vec<String>,
}

/// One key package for each device of each user (but this one): a one-time
/// one while they last, then the last-resort one.
pub async fn claim_key_packages(State(app): State<App>, me: Me, Json(body): Json<Claim>) -> Result<Json<Vec<ClaimedKeyPackage>>> {
    if body.user_ids.len() > 200 {
        return Err(Error::bad_request("Too many people at once."));
    }
    let mut out = Vec::new();
    let mut tx = app.db.begin().await?;
    for user in &body.user_ids {
        let devices: Vec<String> = sqlx::query_scalar("SELECT device_id FROM e2ee_devices WHERE user_id = ? AND revoked_at = 0").bind(user).fetch_all(&mut *tx).await?;
        for device in devices.into_iter().filter(|d| !(user == &me.user_id && d == &me.device_id)) {
            let row: Option<(i64, Vec<u8>, bool)> = sqlx::query_as(
                "SELECT id, data, last_resort FROM e2ee_key_packages WHERE user_id = ? AND device_id = ? ORDER BY last_resort, id LIMIT 1",
            )
            .bind(user)
            .bind(&device)
            .fetch_optional(&mut *tx)
            .await?;
            let Some((id, data, last_resort)) = row else { continue };
            if !last_resort {
                sqlx::query("DELETE FROM e2ee_key_packages WHERE id = ?").bind(id).execute(&mut *tx).await?;
            }
            out.push(ClaimedKeyPackage { user_id: user.clone(), device_id: device, key_package: B64.encode(data) });
        }
    }
    tx.commit().await?;
    Ok(Json(out))
}

/// An encrypted channel `me` is in.
async fn encrypted_channel(app: &App, me: &Me, channel_id: &str) -> Result<db::ChannelRow> {
    let channel = db::channel(&app.db, channel_id).await?.ok_or_else(|| Error::not_found("Channel"))?;
    if db::member(&app.db, channel_id, &me.user_id).await?.is_none() {
        return Err(Error::forbidden());
    }
    if !channel.encrypted {
        return Err(Error::bad_request("This channel isn't encrypted."));
    }
    Ok(channel)
}

/// The largest ciphertext (base64) an event may carry: a reaction is tiny.
const MAX_EVENT: usize = 4 * 1024;

#[derive(sqlx::FromRow)]
struct EventRow {
    id: String,
    channel_id: String,
    post_id: String,
    user_id: String,
    client_id: String,
    create_at: i64,
    epoch: i64,
    ct: String,
}

impl From<EventRow> for E2eeEvent {
    fn from(r: EventRow) -> Self {
        E2eeEvent {
            id: r.id,
            channel_id: r.channel_id,
            post_id: r.post_id,
            user_id: r.user_id,
            client_id: r.client_id,
            create_at: r.create_at,
            dm_e2ee: json!({ "v": 1, "epoch": r.epoch, "ct": r.ct }),
        }
    }
}

const EVENT_COLUMNS: &str = "id, channel_id, post_id, user_id, client_id, create_at, epoch, ct";

/// The encrypted events of these posts (a JSON array of ids), oldest first.
pub async fn events_of(db: &db::Db, post_ids: &str) -> sqlx::Result<Vec<E2eeEvent>> {
    let sql = format!("SELECT {EVENT_COLUMNS} FROM e2ee_events WHERE post_id IN (SELECT value FROM json_each(?1)) ORDER BY create_at, id");
    let rows: Vec<EventRow> = sqlx::query_as(&sql).bind(post_ids).fetch_all(db).await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// `POST e2ee/channels/{id}/events`: an encrypted event (a reaction) on a
/// post (E2EE.md §8). The server stores it against the post and tells the
/// channel; it can't see what it says.
pub async fn post_event(State(app): State<App>, me: Me, Path(channel_id): Path<String>, Json(body): Json<NewE2eeEvent>) -> Result<(StatusCode, Json<E2eeEvent>)> {
    if app.limiter.take(&format!("post {}", me.user_id), crate::limits::POSTS).is_err() {
        return Err(Error::too_many("You're reacting too fast. Wait a moment."));
    }
    encrypted_channel(&app, &me, &channel_id).await?;
    let post = db::post(&app.db, &body.post_id).await?.filter(|p| p.delete_at == 0 && p.channel_id == channel_id).ok_or_else(|| Error::not_found("Message"))?;
    if body.client_id.is_empty() || body.client_id.len() > 64 {
        return Err(Error::bad_request("An event needs its own id."));
    }
    let e2ee = super::posts::ciphertext(&app, &channel_id, &body.dm_e2ee, MAX_EVENT).await?;
    // A retry of one that arrived: the same event, not a second.
    let sql = format!("SELECT {EVENT_COLUMNS} FROM e2ee_events WHERE user_id = ? AND client_id = ?");
    if let Some(row) = sqlx::query_as::<_, EventRow>(&sql).bind(&me.user_id).bind(&body.client_id).fetch_optional(&app.db).await? {
        return Ok((StatusCode::CREATED, Json(row.into())));
    }
    let event = E2eeEvent {
        id: ids::new_id(),
        channel_id: channel_id.clone(),
        post_id: post.id.clone(),
        user_id: me.user_id.clone(),
        client_id: body.client_id,
        create_at: ids::now(),
        dm_e2ee: e2ee.clone(),
    };
    sqlx::query("INSERT INTO e2ee_events (id, channel_id, post_id, user_id, client_id, create_at, epoch, ct) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(&event.id)
        .bind(&event.channel_id)
        .bind(&event.post_id)
        .bind(&event.user_id)
        .bind(&event.client_id)
        .bind(event.create_at)
        .bind(e2ee["epoch"].as_i64())
        .bind(e2ee["ct"].as_str())
        .execute(&app.db)
        .await?;
    // Marks the post changed, so `since` fetches pick it up.
    sqlx::query("UPDATE posts SET update_at = ? WHERE id = ?").bind(event.create_at).bind(&post.id).execute(&app.db).await?;
    let members = db::member_ids(&app.db, &channel_id).await?;
    let data = json!({ "event": serde_json::to_string(&event).map_err(|_| Error::internal())? });
    app.hub.send(&members, &Event::new("e2ee_event", data).channel(&channel_id));
    Ok((StatusCode::CREATED, Json(event)))
}

#[derive(Deserialize)]
pub struct NewGroup {
    group_info: String,
}

/// Starts a channel's group: the creating device alone, at epoch 0.
pub async fn create_group(State(app): State<App>, me: Me, Path(channel_id): Path<String>, Json(body): Json<NewGroup>) -> Result<Json<GroupState>> {
    encrypted_channel(&app, &me, &channel_id).await?;
    let info = decode("group info", &body.group_info)?;
    let _held = app.e2ee.lock.lock().await;
    let mut tx = app.db.begin().await?;
    let exists: bool = sqlx::query_scalar("SELECT COUNT(*) > 0 FROM e2ee_groups WHERE channel_id = ?").bind(&channel_id).fetch_one(&mut *tx).await?;
    if exists {
        return Err(Error::conflict("This channel already has a group: join it instead."));
    }
    let keys = Keys::of(&mut tx, std::slice::from_ref(&me.user_id)).await?;
    let (server, own) = e2ee::load(&mut tx, &app, &channel_id).await?;
    server.register(&channel_id, &info, &(me.user_id.clone(), me.device_id.clone()), &keys).map_err(refused)?;
    e2ee::save(&mut tx, &channel_id, server.storage(), &own).await?;
    let now = ids::now();
    sqlx::query("INSERT INTO e2ee_groups (channel_id, epoch, group_info, create_at, update_at) VALUES (?, 0, ?, ?, ?)")
        .bind(&channel_id)
        .bind(&info)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(GroupState { epoch: 0, group_info: body.group_info, seq: 0 }))
}

pub async fn group_state(State(app): State<App>, me: Me, Path(channel_id): Path<String>) -> Result<Json<GroupState>> {
    encrypted_channel(&app, &me, &channel_id).await?;
    let row: Option<(i64, Vec<u8>)> = sqlx::query_as("SELECT epoch, group_info FROM e2ee_groups WHERE channel_id = ?").bind(&channel_id).fetch_optional(&app.db).await?;
    let (epoch, info) = row.ok_or_else(|| Error::not_found("Group"))?;
    let seq: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM e2ee_handshakes WHERE channel_id = ?").bind(&channel_id).fetch_one(&app.db).await?;
    Ok(Json(GroupState { epoch: epoch as u64, group_info: B64.encode(info), seq }))
}

/// Takes a commit: checked against the group's public state, then logged,
/// with Welcomes kept for the devices it adds. `409` if another commit got
/// there first.
pub async fn post_handshake(State(app): State<App>, me: Me, Path(channel_id): Path<String>, Json(body): Json<NewHandshake>) -> Result<Json<HandshakeEntry>> {
    encrypted_channel(&app, &me, &channel_id).await?;
    let commit = decode("commit", &body.commit)?;
    let info = decode("group info", &body.group_info)?;
    let welcome = if body.welcome.is_empty() { None } else { Some(decode("welcome", &body.welcome)?) };
    let members = db::member_ids(&app.db, &channel_id).await?;
    let sender = (me.user_id.clone(), me.device_id.clone());

    let (entry, applied) = {
        let _held = app.e2ee.lock.lock().await;
        let mut tx = app.db.begin().await?;
        let keys = Keys::of(&mut tx, &members).await?;
        let (server, own) = e2ee::load(&mut tx, &app, &channel_id).await?;
        if !server.has_group(&channel_id) {
            return Err(Error::not_found("Group"));
        }
        let applied = match server.commit(&channel_id, &sender, &commit, &info, |user| members.iter().any(|m| m == user), &keys) {
            Ok(applied) => applied,
            Err(Refusal::Stale { current }) => return Err(Error::conflict(format!("The group is at epoch {current} now: fetch the handshakes you missed and try again."))),
            Err(Refusal::Invalid(e)) => return Err(refused(e)),
        };
        if !applied.added.is_empty() && !applied.external && welcome.is_none() {
            return Err(Error::bad_request("A commit that adds devices needs its Welcome."));
        }
        e2ee::save(&mut tx, &channel_id, server.storage(), &own).await?;
        let made_in = applied.epoch - 1;
        let seq = e2ee::log(&mut tx, &channel_id, made_in, "commit", Some(&sender), &commit).await?;
        sqlx::query("UPDATE e2ee_groups SET epoch = ?, group_info = ?, update_at = ? WHERE channel_id = ?")
            .bind(applied.epoch as i64)
            .bind(&info)
            .bind(ids::now())
            .bind(&channel_id)
            .execute(&mut *tx)
            .await?;
        if let Some(welcome) = &welcome {
            for (user, device) in &applied.added {
                sqlx::query("INSERT INTO e2ee_welcomes (user_id, device_id, channel_id, seq, data, create_at) VALUES (?, ?, ?, ?, ?, ?)")
                    .bind(user)
                    .bind(device)
                    .bind(&channel_id)
                    .bind(seq)
                    .bind(welcome)
                    .bind(ids::now())
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await?;
        (HandshakeEntry { seq, epoch: made_in, kind: "commit".into(), data: body.commit }, applied)
    };

    let mut told = members.clone();
    told.extend(applied.removed.iter().map(|(u, _)| u.clone()).filter(|u| !members.contains(u)));
    e2ee::announce(&app, &channel_id, entry.seq, "commit", &told);
    let mut added: Vec<String> = applied.added.iter().map(|(u, _)| u.clone()).collect();
    added.dedup();
    if !added.is_empty() {
        app.hub.send(&added, &Event::new("mls_welcome", json!({ "channel_id": channel_id, "seq": entry.seq })).channel(&channel_id));
    }
    Ok(Json(entry))
}

#[derive(Deserialize)]
pub struct After {
    #[serde(default)]
    after: i64,
}

/// The group's handshakes after `after`, in order (at most 500 at a time).
pub async fn handshakes(State(app): State<App>, me: Me, Path(channel_id): Path<String>, Query(q): Query<After>) -> Result<Json<Vec<HandshakeEntry>>> {
    encrypted_channel(&app, &me, &channel_id).await?;
    let rows: Vec<(i64, i64, String, Vec<u8>)> =
        sqlx::query_as("SELECT seq, epoch, kind, data FROM e2ee_handshakes WHERE channel_id = ? AND seq > ? ORDER BY seq LIMIT 500")
            .bind(&channel_id)
            .bind(q.after)
            .fetch_all(&app.db)
            .await?;
    Ok(Json(rows.into_iter().map(|(seq, epoch, kind, data)| HandshakeEntry { seq, epoch: epoch as u64, kind, data: B64.encode(data) }).collect()))
}

/// Welcomes waiting for this device.
pub async fn welcomes(State(app): State<App>, me: Me) -> Result<Json<Vec<PendingWelcome>>> {
    let rows: Vec<(i64, String, i64, Vec<u8>)> =
        sqlx::query_as("SELECT id, channel_id, seq, data FROM e2ee_welcomes WHERE user_id = ? AND device_id = ? ORDER BY id")
            .bind(&me.user_id)
            .bind(&me.device_id)
            .fetch_all(&app.db)
            .await?;
    Ok(Json(rows.into_iter().map(|(id, channel_id, seq, data)| PendingWelcome { id, channel_id, seq, welcome: B64.encode(data) }).collect()))
}

/// This device has taken a Welcome in.
pub async fn ack_welcome(State(app): State<App>, me: Me, Path(id): Path<i64>) -> Result<Json<Value>> {
    sqlx::query("DELETE FROM e2ee_welcomes WHERE id = ? AND user_id = ? AND device_id = ?").bind(id).bind(&me.user_id).bind(&me.device_id).execute(&app.db).await?;
    super::ok()
}
