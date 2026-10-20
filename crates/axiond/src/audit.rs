//! The audit log: what admins did, so two admins can see each other's work.

use crate::{db::Db, ids};

/// Records an action. `actor` is empty for the `axiond` command line.
pub async fn record(db: &Db, actor: &str, action: &str, target: &str, detail: &str) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO audit (id, create_at, actor_id, action, target_id, detail) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(ids::new_id())
        .bind(ids::now())
        .bind(actor)
        .bind(action)
        .bind(target)
        .bind(detail)
        .execute(db)
        .await?;
    Ok(())
}

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub struct Entry {
    pub id: String,
    pub create_at: i64,
    pub actor_id: String,
    /// The actor's username, or empty for the command line.
    pub actor: String,
    pub action: String,
    pub target_id: String,
    pub detail: String,
}

/// The newest entries first.
pub async fn recent(db: &Db, limit: i64) -> sqlx::Result<Vec<Entry>> {
    sqlx::query_as(
        "SELECT a.id, a.create_at, a.actor_id, COALESCE(u.username, '') AS actor, a.action, a.target_id, a.detail \
         FROM audit a LEFT JOIN users u ON u.id = a.actor_id ORDER BY a.create_at DESC, a.id DESC LIMIT ?",
    )
    .bind(limit)
    .fetch_all(db)
    .await
}
