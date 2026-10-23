//! Tidying that runs every hour: files of deleted messages and uploads
//! never posted are removed from disk (so deleting a message really deletes
//! its pictures), and expired sessions, used-up invites and old counts of
//! wrong passwords are forgotten.

use std::time::Duration;

use crate::{App, ids};

/// Deleted messages' files are kept this long, in case of a mistaken delete.
const DELETED_FILES_DAYS: i64 = 7;
/// Uploads nobody posted are removed after this long.
const UNPOSTED_HOURS: i64 = 24;

pub fn start(app: &App) {
    let app = app.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            match run(&app).await {
                Ok(n) if n > 0 => tracing::info!("housekeeping: removed {n} files"),
                Ok(_) => {}
                Err(e) => tracing::warn!("housekeeping: {e}"),
            }
        }
    });
}

/// How often the server checks that open connections and calls still have
/// a session.
const SESSION_WATCH: Duration = Duration::from_secs(5);

/// Closes the sockets and calls of sessions that ended without the server
/// doing it: expired, or ended by an admin command that couldn't reach the
/// admin socket (`control`).
pub fn watch_sessions(app: &App) {
    let app = app.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(SESSION_WATCH);
        loop {
            tick.tick().await;
            if let Err(e) = ended_sessions(&app).await {
                tracing::warn!("checking sessions: {e}");
            }
        }
    });
}

/// One check of the session watch.
pub async fn ended_sessions(app: &App) -> sqlx::Result<()> {
    let now = ids::now();
    let live = app.hub.session_ids();
    if !live.is_empty() {
        let kept: Vec<String> = sqlx::query_scalar(
            "SELECT s.id FROM sessions s JOIN users u ON u.id = s.user_id \
             WHERE s.id IN (SELECT value FROM json_each(?1)) AND s.expires_at > ?2 AND u.delete_at = 0",
        )
        .bind(serde_json::to_string(&live).unwrap_or_default())
        .bind(now)
        .fetch_all(&app.db)
        .await?;
        for id in live.iter().filter(|id| !kept.contains(id)) {
            app.hub.end_session(id);
        }
    }
    let devices = crate::calls::devices(app);
    if !devices.is_empty() {
        let kept: Vec<(String, String)> = sqlx::query_as(
            "SELECT DISTINCT s.user_id, s.device_id FROM sessions s JOIN users u ON u.id = s.user_id \
             WHERE s.user_id IN (SELECT json_extract(value, '$[0]') FROM json_each(?1)) AND s.expires_at > ?2 AND u.delete_at = 0",
        )
        .bind(serde_json::to_string(&devices).unwrap_or_default())
        .bind(now)
        .fetch_all(&app.db)
        .await?;
        for (user, device) in devices.iter().filter(|d| !kept.contains(d)) {
            crate::calls::device_gone(app, user, device).await;
        }
    }
    Ok(())
}

/// One pass. Returns how many files went.
pub async fn run(app: &App) -> anyhow::Result<usize> {
    let now = ids::now();
    let gone: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM files WHERE (delete_at > 0 AND delete_at < ?1) OR (post_id = '' AND create_at < ?2)",
    )
    .bind(now - DELETED_FILES_DAYS * 86_400_000)
    .bind(now - UNPOSTED_HOURS * 3_600_000)
    .fetch_all(&app.db)
    .await?;
    for id in &gone {
        crate::api::files::remove_stored(app, id).await;
        sqlx::query("DELETE FROM files WHERE id = ?").bind(id).execute(&app.db).await?;
    }
    let expired: Vec<(String, String, String)> = sqlx::query_as("DELETE FROM sessions WHERE expires_at < ?1 OR create_at < ?1 - ?2 RETURNING id, user_id, device_id")
        .bind(now)
        .bind(crate::auth::SESSION_MAX_DAYS * 86_400_000)
        .fetch_all(&app.db)
        .await?;
    for (session, user, device) in expired {
        // Its sockets close too: an open one would keep getting events.
        app.hub.end_session(&session);
        crate::e2ee::device_gone(app, &user, &device).await?;
    }
    sqlx::query("DELETE FROM invites WHERE uses_left <= 0 OR expires_at < ?").bind(now).execute(&app.db).await?;
    crate::auth::forget_old_failures(&app.db).await?;
    Ok(gone.len())
}
