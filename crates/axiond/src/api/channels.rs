use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use mm_api::models::{Channel, ChannelMember};
use serde::Deserialize;
use serde_json::{Value, json};

use super::posts;
use crate::auth::Me;
use crate::db::{self, CHANNEL_COLUMNS, ChannelRow, MEMBER_COLUMNS, MemberRow};
use crate::error::{Error, Result};
use crate::hub::Event;
use crate::{App, ids};

/// The channel, if `me` may read it: a member, or anyone in the team for a
/// public channel.
pub async fn readable(app: &App, me: &Me, channel_id: &str) -> Result<ChannelRow> {
    let channel = db::channel(&app.db, channel_id).await?.ok_or_else(|| Error::not_found("Channel"))?;
    if db::member(&app.db, channel_id, &me.user_id).await?.is_some()
        || (channel.kind == "O" && db::in_team(&app.db, &channel.team_id, &me.user_id).await?)
    {
        Ok(channel)
    } else {
        Err(Error::forbidden())
    }
}

async fn team_member(app: &App, me: &Me, team_id: &str) -> Result<()> {
    if db::in_team(&app.db, team_id, &me.user_id).await? { Ok(()) } else { Err(Error::forbidden()) }
}

/// The user's channels in a team, plus their direct messages (which belong to no team).
pub async fn mine(State(app): State<App>, me: Me, Path(team): Path<String>) -> Result<Json<Vec<Channel>>> {
    team_member(&app, &me, &team).await?;
    let sql = format!(
        "SELECT {CHANNEL_COLUMNS} FROM channels c JOIN channel_members m ON m.channel_id = c.id \
         WHERE m.user_id = ? AND (c.team_id = ? OR c.team_id = '') AND c.delete_at = 0"
    );
    let rows: Vec<ChannelRow> = sqlx::query_as(&sql).bind(&me.user_id).bind(&team).fetch_all(&app.db).await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

pub async fn my_members(State(app): State<App>, me: Me, Path(team): Path<String>) -> Result<Json<Vec<ChannelMember>>> {
    team_member(&app, &me, &team).await?;
    let sql = format!(
        "SELECT {MEMBER_COLUMNS} FROM channel_members m JOIN channels c ON c.id = m.channel_id \
         WHERE m.user_id = ? AND (c.team_id = ? OR c.team_id = '') AND c.delete_at = 0"
    );
    let rows: Vec<MemberRow> = sqlx::query_as(&sql).bind(&me.user_id).bind(&team).fetch_all(&app.db).await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// Everyone in a channel you can read (Mattermost's paging is ignored:
/// axiond's channels are a group's size).
pub async fn members(State(app): State<App>, me: Me, Path(id): Path<String>) -> Result<Json<Vec<ChannelMember>>> {
    readable(&app, &me, &id).await?;
    let sql = format!("SELECT {MEMBER_COLUMNS} FROM channel_members m WHERE m.channel_id = ?");
    let rows: Vec<MemberRow> = sqlx::query_as(&sql).bind(&id).fetch_all(&app.db).await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

pub async fn get_one(State(app): State<App>, me: Me, Path(id): Path<String>) -> Result<Json<Channel>> {
    Ok(Json(readable(&app, &me, &id).await?.into()))
}

#[derive(Deserialize)]
pub struct Search {
    #[serde(default)]
    term: String,
}

/// Public channels in a team whose name matches, joined or not.
pub async fn search(State(app): State<App>, me: Me, Path(team): Path<String>, Json(body): Json<Search>) -> Result<Json<Vec<Channel>>> {
    team_member(&app, &me, &team).await?;
    let like = format!("%{}%", body.term.trim().replace(['%', '_', '\\'], ""));
    let sql = format!(
        "SELECT {CHANNEL_COLUMNS} FROM channels c WHERE c.team_id = ?1 AND c.type = 'O' AND c.delete_at = 0 \
         AND (c.name LIKE ?2 OR c.display_name LIKE ?2) ORDER BY c.display_name LIMIT 50"
    );
    let rows: Vec<ChannelRow> = sqlx::query_as(&sql).bind(&team).bind(&like).fetch_all(&app.db).await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize)]
pub struct View {
    channel_id: String,
    #[serde(default)]
    prev_channel_id: String,
}

/// Marks a channel (and the one being left) read.
pub async fn view(State(app): State<App>, me: Me, Json(body): Json<View>) -> Result<Json<Value>> {
    let now = ids::now();
    let mut times = serde_json::Map::new();
    for id in [&body.channel_id, &body.prev_channel_id].into_iter().filter(|id| !id.is_empty()) {
        let done = sqlx::query(
            "UPDATE channel_members SET last_viewed_at = ?1, last_update_at = ?1, mention_count = 0, mention_count_root = 0, \
               msg_count = (SELECT total_msg_count FROM channels WHERE id = ?2), \
               msg_count_root = (SELECT total_msg_count_root FROM channels WHERE id = ?2) \
             WHERE channel_id = ?2 AND user_id = ?3",
        )
        .bind(now)
        .bind(id)
        .bind(&me.user_id)
        .execute(&app.db)
        .await?;
        if done.rows_affected() > 0 {
            times.insert(id.clone(), now.into());
            // The user's other apps clear the badge too.
            app.hub.send(std::slice::from_ref(&me.user_id), &Event::new("channel_viewed", json!({ "channel_id": id })));
        }
    }
    Ok(Json(json!({ "status": "OK", "last_viewed_at_times": times })))
}

/// The direct-message channel between two people, made the first time.
pub async fn direct(State(app): State<App>, me: Me, Json(pair): Json<Vec<String>>) -> Result<Json<Channel>> {
    let [a, b] = pair.as_slice() else { return Err(Error::bad_request("A direct message needs exactly two people.")) };
    if *a != me.user_id && *b != me.user_id {
        return Err(Error::forbidden());
    }
    let other = if *a == me.user_id { b } else { a };
    Ok(Json(direct_channel(&app, &me.user_id, other).await?.into()))
}

/// The direct-message channel between `me` and `other`, made the first time.
pub async fn direct_channel(app: &App, me: &str, other: &str) -> Result<ChannelRow> {
    let (a, b) = (me, other);
    if super::safety::blocked_between(app, a, b).await? {
        return Err(Error::bad_request("You can't message this person."));
    }
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = ? AND delete_at = 0").bind(other).fetch_one(&app.db).await?;
    if exists == 0 {
        return Err(Error::not_found("User"));
    }
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let name = format!("{lo}__{hi}");
    let find = format!("SELECT {CHANNEL_COLUMNS} FROM channels c WHERE c.team_id = '' AND c.type = 'D' AND c.name = ?");
    if let Some(row) = sqlx::query_as::<_, ChannelRow>(&find).bind(&name).fetch_optional(&app.db).await? {
        return Ok(row);
    }
    let (id, now) = (ids::new_id(), ids::now());
    let mut tx = app.db.begin().await?;
    // Direct messages are always end-to-end encrypted.
    sqlx::query("INSERT INTO channels (id, team_id, type, name, create_at, update_at, creator_id, encrypted) VALUES (?, '', 'D', ?, ?, ?, ?, 1)")
        .bind(&id)
        .bind(&name)
        .bind(now)
        .bind(now)
        .bind(me)
        .execute(&mut *tx)
        .await?;
    for user in if a == b { vec![a] } else { vec![a, b] } {
        sqlx::query("INSERT INTO channel_members (channel_id, user_id, last_viewed_at, last_update_at) VALUES (?, ?, ?, ?)")
            .bind(&id)
            .bind(user)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    // Both people's apps add it to their list.
    let users: Vec<String> = if a == b { vec![a.into()] } else { vec![a.into(), b.into()] };
    for user in &users {
        let teammate = users.iter().find(|u| *u != user).unwrap_or(user);
        app.hub.send(std::slice::from_ref(user), &Event::new("direct_added", json!({ "teammate_id": teammate })).channel(&id));
    }
    db::channel(&app.db, &id).await?.ok_or_else(Error::internal)
}

#[derive(Deserialize)]
pub struct NewChannel {
    team_id: String,
    name: String,
    #[serde(default)]
    display_name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    purpose: String,
    #[serde(default)]
    header: String,
}

/// `POST /channels`: a public (`O`) or private (`P`) channel, with its
/// creator in it. Private channels are end-to-end encrypted.
pub async fn create(State(app): State<App>, me: Me, Json(body): Json<NewChannel>) -> Result<(StatusCode, Json<Channel>)> {
    team_member(&app, &me, &body.team_id).await?;
    if !matches!(body.kind.as_str(), "O" | "P") {
        return Err(Error::bad_request("A channel is public (O) or private (P)."));
    }
    let name = body.name.trim().to_lowercase();
    crate::admin::check_name(&name).map_err(|e| Error::bad_request(e.to_string()))?;
    let display = if body.display_name.trim().is_empty() { name.clone() } else { body.display_name.trim().chars().take(64).collect() };
    let (id, now) = (ids::new_id(), ids::now());
    let created = sqlx::query(
        "INSERT INTO channels (id, team_id, type, name, display_name, purpose, header, create_at, update_at, creator_id, encrypted) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&body.team_id)
    .bind(&body.kind)
    .bind(&name)
    .bind(&display)
    .bind(body.purpose.trim())
    .bind(body.header.trim())
    .bind(now)
    .bind(now)
    .bind(&me.user_id)
    .bind(body.kind == "P")
    .execute(&app.db)
    .await;
    match created {
        Err(sqlx::Error::Database(d)) if d.is_unique_violation() => return Err(Error::bad_request(format!("~{name} already exists."))),
        other => other?,
    };
    let channel = db::channel(&app.db, &id).await?.ok_or_else(Error::internal)?;
    add_member(&app.db, &channel, &me.user_id).await?;
    app.hub.send(std::slice::from_ref(&me.user_id), &Event::new("channel_created", json!({ "channel_id": id, "team_id": body.team_id })).channel(&id));
    Ok((StatusCode::CREATED, Json(channel.into())))
}

/// One member of a channel you can read.
pub async fn member(State(app): State<App>, me: Me, Path((id, user)): Path<(String, String)>) -> Result<Json<ChannelMember>> {
    readable(&app, &me, &id).await?;
    Ok(Json(db::member(&app.db, &id, &user).await?.ok_or_else(|| Error::not_found("Member"))?))
}

/// Turns end-to-end encryption on for a public channel (an admin's choice;
/// private channels and DMs always have it). It applies to new messages;
/// it can't be turned off yet.
pub async fn set_encrypted(app: &App, actor: &str, channel: &ChannelRow) -> Result<()> {
    if channel.encrypted {
        return Ok(());
    }
    if channel.kind != "O" {
        return Err(Error::bad_request("Only public channels can have encryption switched on."));
    }
    sqlx::query("UPDATE channels SET encrypted = 1, update_at = ? WHERE id = ?").bind(ids::now()).bind(&channel.id).execute(&app.db).await?;
    let text = "End-to-end encryption is on: new messages here can only be read by the people in the channel.";
    let mut on = channel.clone();
    on.encrypted = true;
    posts::insert(app, &on, actor, posts::New::system("system_encryption_on", text.into())).await?;
    crate::audit::record(&app.db, actor, "channel.encrypt", &channel.id, &channel.name).await?;
    let members = db::member_ids(&app.db, &channel.id).await?;
    app.hub.send(&members, &Event::new("channel_updated", json!({ "channel_id": channel.id })).channel(&channel.id));
    Ok(())
}

#[derive(Deserialize)]
pub struct Encrypted {
    encrypted: bool,
}

/// `PUT e2ee/channels/{id}/encrypted` (admins).
pub async fn put_encrypted(State(app): State<App>, me: Me, Path(id): Path<String>, Json(body): Json<Encrypted>) -> Result<Json<Value>> {
    if !me.is_admin() {
        return Err(Error::forbidden());
    }
    if !body.encrypted {
        return Err(Error::bad_request("Encryption can't be turned off."));
    }
    let channel = db::channel(&app.db, &id).await?.ok_or_else(|| Error::not_found("Channel"))?;
    set_encrypted(&app, &me.user_id, &channel).await?;
    super::ok()
}

#[derive(Deserialize)]
pub struct Join {
    user_id: String,
}

/// Joins a public channel in one of the user's teams.
pub async fn join(State(app): State<App>, me: Me, Path(id): Path<String>, Json(body): Json<Join>) -> Result<Json<ChannelMember>> {
    if body.user_id != me.user_id {
        // Adding other people comes with invites.
        return Err(Error::not_yet());
    }
    let channel = db::channel(&app.db, &id).await?.ok_or_else(|| Error::not_found("Channel"))?;
    join_channel(&app, &me.user_id, &channel).await?;
    Ok(Json(db::member(&app.db, &id, &me.user_id).await?.ok_or_else(Error::internal)?))
}

/// Joins a public channel in one of the user's teams (nothing if already in it).
pub async fn join_channel(app: &App, user_id: &str, channel: &ChannelRow) -> Result<()> {
    if db::member(&app.db, &channel.id, user_id).await?.is_some() {
        return Ok(());
    }
    if channel.kind != "O" || !db::in_team(&app.db, &channel.team_id, user_id).await? {
        return Err(Error::forbidden());
    }
    add_member(&app.db, channel, user_id).await?;
    let user = db::user(&app.db, user_id, user_id).await?;
    posts::insert(app, channel, user_id, posts::New::system("system_join_channel", format!("@{} joined the channel.", user.username))).await?;
    let members = db::member_ids(&app.db, &channel.id).await?;
    app.hub.send(&members, &Event::new("user_added", json!({ "user_id": user_id, "team_id": channel.team_id })).channel(&channel.id));
    Ok(())
}

/// A new member starts with everything read.
pub async fn add_member(db: &db::Db, channel: &ChannelRow, user_id: &str) -> sqlx::Result<()> {
    let now = ids::now();
    sqlx::query(
        "INSERT OR IGNORE INTO channel_members (channel_id, user_id, last_viewed_at, msg_count, msg_count_root, last_update_at) \
         SELECT id, ?, ?, total_msg_count, total_msg_count_root, ? FROM channels WHERE id = ?",
    )
    .bind(user_id)
    .bind(now)
    .bind(now)
    .bind(&channel.id)
    .execute(db)
    .await?;
    Ok(())
}

pub async fn leave(State(app): State<App>, me: Me, Path((id, user)): Path<(String, String)>) -> Result<Json<Value>> {
    if user != me.user_id {
        return Err(Error::not_yet());
    }
    let channel = db::channel(&app.db, &id).await?.ok_or_else(|| Error::not_found("Channel"))?;
    leave_channel(&app, &me.user_id, &channel).await?;
    super::ok()
}

/// Leaves a channel. Not direct or group messages, and not Town Square.
pub async fn leave_channel(app: &App, user_id: &str, channel: &ChannelRow) -> Result<()> {
    if matches!(channel.kind.as_str(), "D" | "G") || channel.name == "town-square" {
        return Err(Error::bad_request("You can't leave this channel."));
    }
    let left = sqlx::query("DELETE FROM channel_members WHERE channel_id = ? AND user_id = ?").bind(&channel.id).bind(user_id).execute(&app.db).await?;
    if left.rows_affected() > 0 {
        let user = db::user(&app.db, user_id, user_id).await?;
        posts::insert(app, channel, user_id, posts::New::system("system_leave_channel", format!("@{} left the channel.", user.username))).await?;
        crate::e2ee::user_left(app, &channel.id, user_id).await?;
    }
    Ok(())
}
