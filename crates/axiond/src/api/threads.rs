//! Collapsed reply threads: the threads someone follows, and what's unread in them.
//!
//! You follow a thread once you start it, reply to it or are mentioned in it,
//! until you unfollow it.

use axum::Json;
use axum::extract::{Path, Query, State};
use mm_api::models::{User, UserThread, UserThreads};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::App;
use crate::auth::Me;
use crate::db::{self, USER_COLUMNS, UserRow};
use crate::error::{Error, Result};
use crate::hub::Event;

/// One user's view of a thread, or `None` if the root is gone.
pub async fn user_thread(app: &App, user_id: &str, root_id: &str) -> Result<Option<UserThread>> {
    let Some(post) = db::post(&app.db, root_id).await?.filter(|p| p.delete_at == 0 && p.root_id.is_empty()) else { return Ok(None) };
    let (last_viewed,): (i64,) = sqlx::query_as("SELECT COALESCE(MAX(last_viewed), 0) FROM thread_members WHERE post_id = ? AND user_id = ?")
        .bind(root_id)
        .bind(user_id)
        .fetch_one(&app.db)
        .await?;
    let (last_reply_at, unread_replies, unread_mentions): (i64, i64, i64) = sqlx::query_as(
        "SELECT COALESCE(MAX(p.create_at), 0), \
           COUNT(CASE WHEN p.create_at > ?2 AND p.user_id != ?3 THEN 1 END), \
           COUNT(CASE WHEN p.create_at > ?2 AND EXISTS \
             (SELECT 1 FROM post_mentions m WHERE m.post_id = p.id AND m.user_id = ?3) THEN 1 END) \
         FROM posts p WHERE p.root_id = ?1 AND p.delete_at = 0",
    )
    .bind(root_id)
    .bind(last_viewed)
    .bind(user_id)
    .fetch_one(&app.db)
    .await?;
    // The thread's starter, then everyone who replied, in order of first reply.
    let sql = format!(
        "SELECT {USER_COLUMNS} FROM (SELECT u.*, a.first FROM users u JOIN ( \
           SELECT ?1 AS user_id, 0 AS first \
           UNION ALL SELECT user_id, MIN(create_at) FROM posts WHERE root_id = ?2 AND delete_at = 0 AND user_id != ?1 GROUP BY user_id \
         ) a ON a.user_id = u.id) ORDER BY first"
    );
    let rows: Vec<UserRow> = sqlx::query_as(&sql).bind(&post.user_id).bind(root_id).fetch_all(&app.db).await?;
    let participants: Vec<User> = rows.into_iter().map(|r| r.into_user(user_id)).collect();
    Ok(Some(UserThread {
        id: post.id.clone(),
        reply_count: post.reply_count,
        last_reply_at,
        last_viewed_at: last_viewed,
        participants,
        unread_replies,
        unread_mentions,
        post,
        ..Default::default()
    }))
}

/// After a reply: tells each follower how the thread looks for them now.
pub async fn replied(app: &App, root_id: &str, team_id: &str) -> Result<()> {
    let followers: Vec<String> = sqlx::query_scalar("SELECT user_id FROM thread_members WHERE post_id = ? AND following = 1")
        .bind(root_id)
        .fetch_all(&app.db)
        .await?;
    for user in followers {
        if let Some(thread) = user_thread(app, &user, root_id).await? {
            let data = json!({ "thread": serde_json::to_string(&thread).map_err(|_| Error::internal())? });
            app.hub.send(std::slice::from_ref(&user), &Event::new("thread_updated", data).user(&user).team(team_id));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct List {
    per_page: Option<i64>,
}

/// The threads `me` follows in a team's channels and in their direct
/// messages, most recent reply first. Threads with no replies yet aren't listed.
pub async fn list(State(app): State<App>, me: Me, Path(team): Path<String>, Query(q): Query<List>) -> Result<Json<UserThreads>> {
    if !db::in_team(&app.db, &team, &me.user_id).await? {
        return Err(Error::forbidden());
    }
    let roots: Vec<String> = sqlx::query_scalar(
        "SELECT t.post_id FROM thread_members t \
         JOIN posts p ON p.id = t.post_id AND p.delete_at = 0 \
         JOIN channels c ON c.id = p.channel_id AND c.delete_at = 0 AND (c.team_id = ?2 OR c.team_id = '') \
         JOIN channel_members m ON m.channel_id = c.id AND m.user_id = ?1 \
         WHERE t.user_id = ?1 AND t.following = 1 \
           AND EXISTS (SELECT 1 FROM posts r WHERE r.root_id = p.id AND r.delete_at = 0) \
         ORDER BY (SELECT MAX(r.create_at) FROM posts r WHERE r.root_id = p.id AND r.delete_at = 0) DESC",
    )
    .bind(&me.user_id)
    .bind(&team)
    .fetch_all(&app.db)
    .await?;
    let mut all = UserThreads { total: roots.len() as i64, ..Default::default() };
    let per_page = q.per_page.unwrap_or(25).clamp(1, 200) as usize;
    for (i, root) in roots.iter().enumerate() {
        let Some(thread) = user_thread(&app, &me.user_id, root).await? else { continue };
        all.total_unread_threads += i64::from(thread.unread_replies > 0);
        all.total_unread_mentions += thread.unread_mentions;
        if i < per_page {
            all.threads.push(thread);
        }
    }
    Ok(Json(all))
}

/// The thread's root, if `me` can see it.
async fn visible_root(app: &App, me: &Me, root_id: &str) -> Result<mm_api::models::Post> {
    let post = db::post(&app.db, root_id).await?.filter(|p| p.delete_at == 0 && p.root_id.is_empty()).ok_or_else(|| Error::not_found("Thread"))?;
    super::channels::readable(app, me, &post.channel_id).await?;
    Ok(post)
}

/// Marks a thread read up to `at` (Unix ms).
pub async fn read(State(app): State<App>, me: Me, Path((team, root_id, at)): Path<(String, String, i64)>) -> Result<Json<UserThread>> {
    visible_root(&app, &me, &root_id).await?;
    let before = user_thread(&app, &me.user_id, &root_id).await?.ok_or_else(|| Error::not_found("Thread"))?;
    sqlx::query(
        "INSERT INTO thread_members (post_id, user_id, last_viewed, last_update) VALUES (?1, ?2, ?3, ?3) \
         ON CONFLICT (post_id, user_id) DO UPDATE SET last_viewed = ?3, last_update = ?3",
    )
    .bind(&root_id)
    .bind(&me.user_id)
    .bind(at)
    .execute(&app.db)
    .await?;
    let thread = user_thread(&app, &me.user_id, &root_id).await?.ok_or_else(|| Error::not_found("Thread"))?;
    let data = json!({
        "thread_id": root_id,
        "timestamp": at,
        "unread_replies": thread.unread_replies,
        "unread_mentions": thread.unread_mentions,
        "previous_unread_replies": before.unread_replies,
        "previous_unread_mentions": before.unread_mentions,
        "channel_id": thread.post.channel_id,
    });
    app.hub.send(std::slice::from_ref(&me.user_id), &Event::new("thread_read_changed", data).user(&me.user_id).team(&team));
    Ok(Json(thread))
}

pub async fn follow(State(app): State<App>, me: Me, Path((team, root_id)): Path<(String, String)>) -> Result<Json<Value>> {
    set_following(&app, &me, &team, &root_id, true).await
}

pub async fn unfollow(State(app): State<App>, me: Me, Path((team, root_id)): Path<(String, String)>) -> Result<Json<Value>> {
    set_following(&app, &me, &team, &root_id, false).await
}

async fn set_following(app: &App, me: &Me, team: &str, root_id: &str, following: bool) -> Result<Json<Value>> {
    let root = visible_root(app, me, root_id).await?;
    sqlx::query(
        "INSERT INTO thread_members (post_id, user_id, following, last_viewed, last_update) VALUES (?1, ?2, ?3, ?4, ?4) \
         ON CONFLICT (post_id, user_id) DO UPDATE SET following = ?3, last_update = ?4",
    )
    .bind(root_id)
    .bind(&me.user_id)
    .bind(following)
    .bind(crate::ids::now())
    .execute(&app.db)
    .await?;
    let data = json!({ "thread_id": root_id, "state": following, "reply_count": root.reply_count });
    app.hub.send(std::slice::from_ref(&me.user_id), &Event::new("thread_follow_changed", data).user(&me.user_id).team(team));
    super::ok()
}
