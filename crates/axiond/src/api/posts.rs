use std::collections::HashMap;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use mm_api::models::{ChannelUnreadAt, Post, PostList};
use serde::Deserialize;
use serde_json::{Value, json};

use super::channels::readable;
use crate::auth::Me;
use crate::db::{self, ChannelRow, POST_COLUMNS, PostRow};
use crate::error::{Error, Result};
use crate::hub::Event;
use crate::{App, ids};

/// Mattermost's limit, in characters.
const MAX_MESSAGE: usize = 16_383;

/// A post about to be written.
pub struct New {
    pub root_id: String,
    pub message: String,
    /// Empty for people's messages; `system_*` for joins, leaves and the like.
    pub kind: &'static str,
    pub pending_post_id: String,
}

impl New {
    pub fn system(kind: &'static str, message: String) -> Self {
        New { root_id: String::new(), message, kind, pending_post_id: String::new() }
    }
}

#[derive(Deserialize)]
pub struct Create {
    channel_id: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    root_id: String,
    #[serde(default)]
    file_ids: Vec<String>,
    #[serde(default)]
    pending_post_id: String,
}

pub async fn create(State(app): State<App>, me: Me, Json(body): Json<Create>) -> Result<(StatusCode, Json<Post>)> {
    if !body.file_ids.is_empty() {
        return Err(Error::not_yet());
    }
    let message = body.message.trim_end().to_string();
    if message.trim().is_empty() {
        return Err(Error::bad_request("A message can't be empty."));
    }
    if message.chars().count() > MAX_MESSAGE {
        return Err(Error::bad_request(format!("Messages can be at most {MAX_MESSAGE} characters.")));
    }
    let channel = db::channel(&app.db, &body.channel_id).await?.ok_or_else(|| Error::not_found("Channel"))?;
    if db::member(&app.db, &channel.id, &me.user_id).await?.is_none() {
        return Err(Error::forbidden());
    }
    if !body.root_id.is_empty() {
        let root = db::post(&app.db, &body.root_id).await?;
        if !root.is_some_and(|r| r.delete_at == 0 && r.channel_id == channel.id && r.root_id.is_empty()) {
            return Err(Error::bad_request("That thread isn't in this channel."));
        }
    }
    let new = New { root_id: body.root_id, message, kind: "", pending_post_id: body.pending_post_id };
    Ok((StatusCode::CREATED, Json(insert(&app, &channel, &me.user_id, new).await?)))
}

/// Writes a post, counts it, and tells everyone in the channel.
pub async fn insert(app: &App, channel: &ChannelRow, user_id: &str, new: New) -> Result<Post> {
    let (id, now) = (ids::new_id(), ids::now());
    let is_root = new.root_id.is_empty();
    let mentioned = if new.kind.is_empty() { mentions(app, channel, user_id, &new.message).await? } else { Vec::new() };

    let mut tx = app.db.begin().await?;
    sqlx::query("INSERT INTO posts (id, channel_id, user_id, root_id, create_at, update_at, message, type) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(&channel.id)
        .bind(user_id)
        .bind(&new.root_id)
        .bind(now)
        .bind(now)
        .bind(&new.message)
        .bind(new.kind)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE channels SET last_post_at = ?1, total_msg_count = total_msg_count + 1, \
           last_root_post_at = CASE WHEN ?2 THEN ?1 ELSE last_root_post_at END, \
           total_msg_count_root = total_msg_count_root + ?2 WHERE id = ?3",
    )
    .bind(now)
    .bind(is_root)
    .bind(&channel.id)
    .execute(&mut *tx)
    .await?;
    // You've read what you just wrote.
    sqlx::query(
        "UPDATE channel_members SET last_viewed_at = ?1, last_update_at = ?1, \
           msg_count = (SELECT total_msg_count FROM channels WHERE id = ?2), \
           msg_count_root = (SELECT total_msg_count_root FROM channels WHERE id = ?2) \
         WHERE channel_id = ?2 AND user_id = ?3",
    )
    .bind(now)
    .bind(&channel.id)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    for user in &mentioned {
        sqlx::query(
            "UPDATE channel_members SET mention_count = mention_count + 1, mention_count_root = mention_count_root + ?, \
               last_update_at = ? WHERE channel_id = ? AND user_id = ?",
        )
        .bind(is_root)
        .bind(now)
        .bind(&channel.id)
        .bind(user)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO post_mentions (post_id, user_id) VALUES (?, ?)").bind(&id).bind(user).execute(&mut *tx).await?;
    }
    if !is_root {
        // The replier is caught up and following; the thread's starter and
        // anyone mentioned follow too (the starter only if they never unfollowed).
        sqlx::query(
            "INSERT INTO thread_members (post_id, user_id, following, last_viewed, last_update) VALUES (?1, ?2, 1, ?3, ?3) \
             ON CONFLICT (post_id, user_id) DO UPDATE SET following = 1, last_viewed = ?3, last_update = ?3",
        )
        .bind(&new.root_id)
        .bind(user_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO thread_members (post_id, user_id, following, last_viewed, last_update) \
             SELECT id, user_id, 1, create_at, ?2 FROM posts WHERE id = ?1",
        )
        .bind(&new.root_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        for user in &mentioned {
            sqlx::query(
                "INSERT INTO thread_members (post_id, user_id, following, last_viewed, last_update) VALUES (?1, ?2, 1, 0, ?3) \
                 ON CONFLICT (post_id, user_id) DO UPDATE SET following = 1, last_update = ?3",
            )
            .bind(&new.root_id)
            .bind(user)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE thread_members SET last_update = ? WHERE post_id = ?").bind(now).bind(&new.root_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;

    let mut post = db::post(&app.db, &id).await?.ok_or_else(Error::internal)?;
    post.pending_post_id = new.pending_post_id;
    let sender: String = sqlx::query_scalar("SELECT username FROM users WHERE id = ?").bind(user_id).fetch_one(&app.db).await?;
    let mut data = json!({
        "post": serde_json::to_string(&post).map_err(|_| Error::internal())?,
        "team_id": channel.team_id,
        "channel_type": channel.kind,
        "channel_name": channel.name,
        "channel_display_name": channel.display_name,
        "sender_name": format!("@{sender}"),
    });
    if !mentioned.is_empty() {
        data["mentions"] = serde_json::to_string(&mentioned).unwrap_or_default().into();
    }
    let members = db::member_ids(&app.db, &channel.id).await?;
    app.hub.send(&members, &Event::new("posted", data).channel(&channel.id));
    if !is_root {
        super::threads::replied(app, &new.root_id, &channel.team_id).await?;
    }
    Ok(post)
}

/// Who a message mentions: `@username`s in the channel, everyone for
/// `@channel`/`@all`, whoever's online for `@here`, and the other person in a
/// direct message. Never the author.
async fn mentions(app: &App, channel: &ChannelRow, author: &str, message: &str) -> Result<Vec<String>> {
    let members: Vec<(String, String)> = sqlx::query_as(
        "SELECT u.id, u.username FROM users u JOIN channel_members m ON m.user_id = u.id WHERE m.channel_id = ? AND u.id != ?",
    )
    .bind(&channel.id)
    .bind(author)
    .fetch_all(&app.db)
    .await?;
    if channel.kind == "D" {
        return Ok(members.into_iter().map(|(id, _)| id).collect());
    }
    let words = mention_words(message);
    let everyone = words.iter().any(|w| w == "channel" || w == "all");
    let here = words.iter().any(|w| w == "here");
    Ok(members
        .into_iter()
        .filter(|(id, name)| everyone || words.contains(name) || (here && app.hub.online(id)))
        .map(|(id, _)| id)
        .collect())
}

/// The lowercased names after each `@`, without trailing punctuation.
fn mention_words(message: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut rest = message;
    while let Some(at) = rest.find('@') {
        // Not part of an email address.
        let before = rest[..at].chars().next_back();
        rest = &rest[at + 1..];
        if before.is_some_and(|c| c.is_alphanumeric()) {
            continue;
        }
        let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))).unwrap_or(rest.len());
        let word = rest[..end].trim_end_matches(['.', '-', '_']);
        if !word.is_empty() {
            words.push(word.to_ascii_lowercase());
        }
    }
    words
}

#[derive(Deserialize)]
pub struct Patch {
    message: Option<String>,
}

pub async fn edit(State(app): State<App>, me: Me, Path(id): Path<String>, Json(body): Json<Patch>) -> Result<Json<Post>> {
    let post = db::post(&app.db, &id).await?.filter(|p| p.delete_at == 0).ok_or_else(|| Error::not_found("Message"))?;
    if post.user_id != me.user_id || post.is_system() {
        return Err(Error::forbidden());
    }
    let Some(message) = body.message.map(|m| m.trim_end().to_string()) else { return Ok(Json(post)) };
    if message.trim().is_empty() || message.chars().count() > MAX_MESSAGE {
        return Err(Error::bad_request("A message needs 1 to 16383 characters."));
    }
    let now = ids::now();
    sqlx::query("UPDATE posts SET message = ?, edit_at = ?, update_at = ? WHERE id = ?")
        .bind(&message)
        .bind(now)
        .bind(now)
        .bind(&id)
        .execute(&app.db)
        .await?;
    let post = db::post(&app.db, &id).await?.ok_or_else(Error::internal)?;
    tell(&app, "post_edited", &post).await?;
    Ok(Json(post))
}

/// Deletes a post, and a thread's replies with it.
pub async fn remove(State(app): State<App>, me: Me, Path(id): Path<String>) -> Result<Json<Value>> {
    let post = db::post(&app.db, &id).await?.filter(|p| p.delete_at == 0).ok_or_else(|| Error::not_found("Message"))?;
    if post.user_id != me.user_id && !me.is_admin() {
        return Err(Error::forbidden());
    }
    let now = ids::now();
    let replies: Vec<String> = if post.root_id.is_empty() {
        sqlx::query_scalar("SELECT id FROM posts WHERE root_id = ? AND delete_at = 0").bind(&id).fetch_all(&app.db).await?
    } else {
        Vec::new()
    };
    sqlx::query("UPDATE posts SET delete_at = ?1, update_at = ?1 WHERE (id = ?2 OR root_id = ?2) AND delete_at = 0")
        .bind(now)
        .bind(&id)
        .execute(&app.db)
        .await?;
    for gone in std::iter::once(id).chain(replies) {
        if let Some(p) = db::post(&app.db, &gone).await? {
            tell(&app, "post_deleted", &p).await?;
        }
    }
    super::ok()
}

/// Sends a post event to the post's channel.
async fn tell(app: &App, event: &'static str, post: &Post) -> Result<()> {
    let members = db::member_ids(&app.db, &post.channel_id).await?;
    let data = json!({ "post": serde_json::to_string(post).map_err(|_| Error::internal())? });
    app.hub.send(&members, &Event::new(event, data).channel(&post.channel_id));
    Ok(())
}

/// A thread: its first post and every reply, from any post in it.
pub async fn thread(State(app): State<App>, me: Me, Path(id): Path<String>) -> Result<Json<PostList>> {
    let post = db::post(&app.db, &id).await?.filter(|p| p.delete_at == 0).ok_or_else(|| Error::not_found("Message"))?;
    readable(&app, &me, &post.channel_id).await?;
    let root = if post.root_id.is_empty() { post.id } else { post.root_id };
    let sql = format!("SELECT {POST_COLUMNS} FROM posts p WHERE (p.id = ?1 OR p.root_id = ?1) AND p.delete_at = 0 ORDER BY p.create_at DESC, p.id DESC");
    let rows: Vec<PostRow> = sqlx::query_as(&sql).bind(&root).fetch_all(&app.db).await?;
    Ok(Json(list(&app, rows).await?))
}

#[derive(Deserialize)]
pub struct Page {
    page: Option<i64>,
    per_page: Option<i64>,
    since: Option<i64>,
    before: Option<String>,
    after: Option<String>,
    #[serde(rename = "collapsedThreads")]
    collapsed_threads: Option<bool>,
}

/// A channel's history, newest first: a page, the posts before or after one,
/// or everything changed since a time. With `collapsedThreads`, only posts
/// that start threads.
pub async fn in_channel(State(app): State<App>, me: Me, Path(id): Path<String>, Query(q): Query<Page>) -> Result<Json<PostList>> {
    readable(&app, &me, &id).await?;
    let roots = q.collapsed_threads.unwrap_or(false);
    let only_roots = if roots { "AND p.root_id = ''" } else { "" };

    if let Some(since) = q.since {
        // Threads with new replies count as changed, so their reply counts update.
        let changed = if roots { "(p.update_at >= ?2 OR EXISTS (SELECT 1 FROM posts r WHERE r.root_id = p.id AND r.update_at >= ?2))" } else { "p.update_at >= ?2" };
        let sql = format!(
            "SELECT {POST_COLUMNS} FROM posts p WHERE p.channel_id = ?1 AND p.delete_at = 0 {only_roots} AND {changed} \
             ORDER BY p.create_at DESC, p.id DESC LIMIT 1000"
        );
        let rows: Vec<PostRow> = sqlx::query_as(&sql).bind(&id).bind(since).fetch_all(&app.db).await?;
        return Ok(Json(list(&app, rows).await?));
    }

    let per_page = q.per_page.unwrap_or(60).clamp(1, 200);
    // Newer-than `after` reads oldest first, then flips.
    let (key, newer, offset) = match (q.before, q.after) {
        (Some(b), _) if !b.is_empty() => (anchor(&app, &id, &b).await?, false, 0),
        (_, Some(a)) if !a.is_empty() => (anchor(&app, &id, &a).await?, true, 0),
        _ => ((i64::MAX, String::new()), false, q.page.unwrap_or(0).max(0) * per_page),
    };
    let rows = slice(&app, &id, only_roots, &key, newer, per_page, offset).await?;
    let mut posts = list(&app, rows).await?;
    if let (Some(newest), Some(oldest)) = (posts.order.first().cloned(), posts.order.last().cloned()) {
        let at = |pid: &str| (posts.posts[pid].create_at, pid.to_string());
        let (newest, oldest) = (at(&newest), at(&oldest));
        posts.prev_post_id = slice(&app, &id, only_roots, &oldest, false, 1, 0).await?.pop().map(|p| p.id).unwrap_or_default();
        posts.next_post_id = slice(&app, &id, only_roots, &newest, true, 1, 0).await?.pop().map(|p| p.id).unwrap_or_default();
        posts.has_next = !posts.next_post_id.is_empty();
    }
    Ok(Json(posts))
}

/// Where a post sits in its channel's order.
async fn anchor(app: &App, channel: &str, post_id: &str) -> Result<(i64, String)> {
    let post = db::post(&app.db, post_id).await?.filter(|p| p.channel_id == channel).ok_or_else(|| Error::not_found("Message"))?;
    Ok((post.create_at, post.id))
}

/// Up to `limit` posts strictly older (or, with `newer`, newer) than `key`,
/// nearest first.
async fn slice(app: &App, channel: &str, only_roots: &str, key: &(i64, String), newer: bool, limit: i64, offset: i64) -> Result<Vec<PostRow>> {
    let (cmp, dir) = if newer { (">", "ASC") } else { ("<", "DESC") };
    let sql = format!(
        "SELECT {POST_COLUMNS} FROM posts p WHERE p.channel_id = ?1 AND p.delete_at = 0 {only_roots} \
         AND (p.create_at {cmp} ?2 OR (p.create_at = ?2 AND p.id {cmp} ?3)) \
         ORDER BY p.create_at {dir}, p.id {dir} LIMIT ?4 OFFSET ?5"
    );
    Ok(sqlx::query_as(&sql).bind(channel).bind(key.0).bind(&key.1).bind(limit).bind(offset).fetch_all(&app.db).await?)
}

/// Rows into a `PostList`, newest first, with reactions.
async fn list(app: &App, rows: Vec<PostRow>) -> Result<PostList> {
    let mut posts: Vec<Post> = rows.into_iter().map(Into::into).collect();
    posts.sort_by(|a, b| (b.create_at, &b.id).cmp(&(a.create_at, &a.id)));
    db::with_reactions(&app.db, &mut posts).await?;
    Ok(PostList {
        order: posts.iter().map(|p| p.id.clone()).collect(),
        posts: posts.into_iter().map(|p| (p.id.clone(), p)).collect::<HashMap<_, _>>(),
        ..Default::default()
    })
}

/// "Mark as unread": the channel reads as unread from this post on, with
/// its mentions counted again.
pub async fn set_unread(State(app): State<App>, me: Me, Path((user, post_id)): Path<(String, String)>) -> Result<Json<ChannelUnreadAt>> {
    if user != me.user_id {
        return Err(Error::forbidden());
    }
    let post = db::post(&app.db, &post_id).await?.filter(|p| p.delete_at == 0).ok_or_else(|| Error::not_found("Message"))?;
    if db::member(&app.db, &post.channel_id, &me.user_id).await?.is_none() {
        return Err(Error::forbidden());
    }
    // The channel's totals count every post ever written, so "read" is the
    // total minus everything from this post on, deleted or not.
    let (from_here, roots_from_here, mentions, root_mentions): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(CASE WHEN p.root_id = '' THEN 1 END), \
           COUNT(CASE WHEN p.delete_at = 0 AND m.user_id IS NOT NULL THEN 1 END), \
           COUNT(CASE WHEN p.delete_at = 0 AND m.user_id IS NOT NULL AND p.root_id = '' THEN 1 END) \
         FROM posts p LEFT JOIN post_mentions m ON m.post_id = p.id AND m.user_id = ?3 \
         WHERE p.channel_id = ?1 AND p.create_at >= ?2",
    )
    .bind(&post.channel_id)
    .bind(post.create_at)
    .bind(&me.user_id)
    .fetch_one(&app.db)
    .await?;
    let last_viewed_at = post.create_at - 1;
    sqlx::query(
        "UPDATE channel_members SET \
           msg_count = MAX(0, (SELECT total_msg_count FROM channels WHERE id = ?1) - ?3), \
           msg_count_root = MAX(0, (SELECT total_msg_count_root FROM channels WHERE id = ?1) - ?4), \
           mention_count = ?5, mention_count_root = ?6, last_viewed_at = ?7, last_update_at = ?8 \
         WHERE channel_id = ?1 AND user_id = ?2",
    )
    .bind(&post.channel_id)
    .bind(&me.user_id)
    .bind(from_here)
    .bind(roots_from_here)
    .bind(mentions)
    .bind(root_mentions)
    .bind(last_viewed_at)
    .bind(ids::now())
    .execute(&app.db)
    .await?;
    let member = db::member(&app.db, &post.channel_id, &me.user_id).await?.ok_or_else(Error::internal)?;
    let unread = ChannelUnreadAt {
        channel_id: post.channel_id.clone(),
        msg_count: member.msg_count,
        msg_count_root: member.msg_count_root,
        mention_count: member.mention_count,
        mention_count_root: member.mention_count_root,
        last_viewed_at,
    };
    let mut data = serde_json::to_value(&unread).map_err(|_| Error::internal())?;
    data["post_id"] = post.id.into();
    app.hub.send(std::slice::from_ref(&me.user_id), &Event::new("post_unread", data).channel(&post.channel_id).user(&me.user_id));
    Ok(Json(unread))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_skip_emails_and_trailing_dots() {
        assert_eq!(mention_words("hi @Bob. and @alice_, mail me@example.com, @here"), ["bob", "alice", "here"]);
        assert!(mention_words("no one").is_empty());
    }
}
