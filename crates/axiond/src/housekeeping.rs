//! Tidying that runs every hour: files of deleted messages and uploads
//! never posted are removed from disk (so deleting a message really deletes
//! its pictures), and expired sessions and used-up invites are forgotten.

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
    sqlx::query("DELETE FROM sessions WHERE expires_at < ?").bind(now).execute(&app.db).await?;
    sqlx::query("DELETE FROM invites WHERE uses_left <= 0 OR expires_at < ?").bind(now).execute(&app.db).await?;
    Ok(gone.len())
}
