//! The SQLite database: opening it, and turning rows into the API's models.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use mm_api::models::{Channel, ChannelMember, ChannelType, CustomEmoji, Post, Reaction, Team, User};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

pub type Db = SqlitePool;

/// Opens (creating if needed) `axiond.db` in `data_dir` and brings its schema
/// up to date.
pub async fn open(data_dir: &Path) -> anyhow::Result<Db> {
    std::fs::create_dir_all(data_dir)?;
    let path = data_dir.join("axiond.db");
    private(data_dir, &path);
    let url = format!("sqlite://{}", path.display());
    let options = SqliteConnectOptions::from_str(&url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5));
    let db = SqlitePoolOptions::new().max_connections(8).connect_with(options).await?;
    let migrator = sqlx::migrate!("./migrations");
    // Before an upgrade changes an existing database, keep a copy of it.
    let applied: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success = 1").fetch_all(&db).await.unwrap_or_default();
    if !applied.is_empty() && migrator.iter().any(|m| !applied.contains(&m.version)) {
        let dir = data_dir.join("backups");
        std::fs::create_dir_all(&dir)?;
        let copy = dir.join(format!("before-upgrade-{}.db", crate::ids::now()));
        crate::backup::snapshot_db(&db, &copy).await?;
        tracing::info!("upgrading the database; the old one is kept at {}", copy.display());
    }
    migrator.run(&db).await?;
    Ok(db)
}

/// Only the server's user can read the data folder and database. SQLite
/// gives its `-wal` and `-shm` files the database's permissions.
fn private(dir: &Path, db: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
            tracing::warn!("couldn't make {} private: {e}", dir.display());
        }
        let made = std::fs::OpenOptions::new().write(true).create(true).truncate(false).mode(0o600).open(db);
        if let Err(e) = made.and_then(|_| std::fs::set_permissions(db, std::fs::Permissions::from_mode(0o600))) {
            tracing::warn!("couldn't make {} private: {e}", db.display());
        }
    }
    #[cfg(not(unix))]
    let _ = (dir, db);
}

#[derive(sqlx::FromRow)]
pub struct UserRow {
    pub id: String,
    pub create_at: i64,
    pub update_at: i64,
    pub delete_at: i64,
    pub username: String,
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    pub nickname: String,
    pub roles: String,
    pub locale: String,
    pub last_picture_update: i64,
    pub mfa_active: bool,
}

pub const USER_COLUMNS: &str =
    "id, create_at, update_at, delete_at, username, email, first_name, last_name, nickname, roles, locale, last_picture_update, mfa_active";

impl UserRow {
    /// Email addresses (and whether two-factor codes are on) only go to their owner.
    pub fn into_user(self, viewer: &str) -> User {
        let own = self.id == viewer;
        User {
            mfa_active: own && self.mfa_active,
            email: if own { self.email } else { String::new() },
            id: self.id,
            create_at: self.create_at,
            update_at: self.update_at,
            delete_at: self.delete_at,
            username: self.username,
            first_name: self.first_name,
            last_name: self.last_name,
            nickname: self.nickname,
            roles: self.roles,
            locale: self.locale,
            last_picture_update: self.last_picture_update,
            ..Default::default()
        }
    }
}

#[derive(sqlx::FromRow)]
pub struct TeamRow {
    pub id: String,
    pub create_at: i64,
    pub update_at: i64,
    pub delete_at: i64,
    pub name: String,
    pub display_name: String,
    pub description: String,
    #[sqlx(rename = "type")]
    pub kind: String,
}

impl From<TeamRow> for Team {
    fn from(r: TeamRow) -> Self {
        Team {
            id: r.id,
            create_at: r.create_at,
            update_at: r.update_at,
            delete_at: r.delete_at,
            display_name: r.display_name,
            name: r.name,
            description: r.description,
            kind: r.kind,
            last_team_icon_update: 0,
        }
    }
}

#[derive(Clone, sqlx::FromRow)]
pub struct ChannelRow {
    pub id: String,
    pub team_id: String,
    #[sqlx(rename = "type")]
    pub kind: String,
    pub name: String,
    pub display_name: String,
    pub header: String,
    pub purpose: String,
    pub create_at: i64,
    pub update_at: i64,
    pub delete_at: i64,
    pub last_post_at: i64,
    pub last_root_post_at: i64,
    pub total_msg_count: i64,
    pub total_msg_count_root: i64,
    pub creator_id: String,
    /// End-to-end encrypted: the server can't read what's posted.
    pub encrypted: bool,
}

pub const CHANNEL_COLUMNS: &str = "c.id, c.team_id, c.type, c.name, c.display_name, c.header, c.purpose, c.create_at, \
     c.update_at, c.delete_at, c.last_post_at, c.last_root_post_at, c.total_msg_count, c.total_msg_count_root, c.creator_id, c.encrypted";

pub fn channel_type(code: &str) -> ChannelType {
    match code {
        "O" => ChannelType::Open,
        "P" => ChannelType::Private,
        "D" => ChannelType::Direct,
        "G" => ChannelType::Group,
        _ => ChannelType::Unknown,
    }
}

impl From<ChannelRow> for Channel {
    fn from(r: ChannelRow) -> Self {
        Channel {
            id: r.id,
            create_at: r.create_at,
            update_at: r.update_at,
            delete_at: r.delete_at,
            team_id: r.team_id,
            kind: channel_type(&r.kind),
            display_name: r.display_name,
            name: r.name,
            header: r.header,
            purpose: r.purpose,
            last_post_at: r.last_post_at,
            last_root_post_at: r.last_root_post_at,
            total_msg_count: r.total_msg_count,
            total_msg_count_root: r.total_msg_count_root,
            creator_id: r.creator_id,
            encrypted: r.encrypted,
        }
    }
}

#[derive(sqlx::FromRow)]
pub struct MemberRow {
    pub channel_id: String,
    pub user_id: String,
    pub roles: String,
    pub last_viewed_at: i64,
    pub msg_count: i64,
    pub msg_count_root: i64,
    pub mention_count: i64,
    pub mention_count_root: i64,
    pub notify_props: String,
    pub last_update_at: i64,
}

pub const MEMBER_COLUMNS: &str = "m.channel_id, m.user_id, m.roles, m.last_viewed_at, m.msg_count, m.msg_count_root, \
     m.mention_count, m.mention_count_root, m.notify_props, m.last_update_at";

impl From<MemberRow> for ChannelMember {
    fn from(r: MemberRow) -> Self {
        ChannelMember {
            channel_id: r.channel_id,
            user_id: r.user_id,
            roles: r.roles,
            last_viewed_at: r.last_viewed_at,
            msg_count: r.msg_count,
            mention_count: r.mention_count,
            mention_count_root: r.mention_count_root,
            msg_count_root: r.msg_count_root,
            urgent_mention_count: 0,
            notify_props: serde_json::from_str(&r.notify_props).unwrap_or_default(),
            last_update_at: r.last_update_at,
        }
    }
}

#[derive(sqlx::FromRow)]
pub struct PostRow {
    pub id: String,
    pub channel_id: String,
    pub user_id: String,
    pub root_id: String,
    pub create_at: i64,
    pub update_at: i64,
    pub edit_at: i64,
    pub delete_at: i64,
    pub message: String,
    #[sqlx(rename = "type")]
    pub kind: String,
    pub props: String,
    pub file_ids: String,
    pub is_pinned: bool,
    pub pending_post_id: String,
    pub reply_count: i64,
}

/// Columns of `posts p`, with the size of the thread each post is in.
pub const POST_COLUMNS: &str = "p.id, p.channel_id, p.user_id, p.root_id, p.create_at, p.update_at, p.edit_at, \
     p.delete_at, p.message, p.type, p.props, p.file_ids, p.is_pinned, p.pending_post_id, \
     (SELECT COUNT(*) FROM posts r WHERE r.delete_at = 0 \
        AND r.root_id = CASE p.root_id WHEN '' THEN p.id ELSE p.root_id END) AS reply_count";

impl From<PostRow> for Post {
    fn from(r: PostRow) -> Self {
        Post {
            id: r.id,
            create_at: r.create_at,
            update_at: r.update_at,
            edit_at: r.edit_at,
            delete_at: r.delete_at,
            is_pinned: r.is_pinned,
            user_id: r.user_id,
            channel_id: r.channel_id,
            root_id: r.root_id,
            message: r.message,
            kind: r.kind,
            props: serde_json::from_str(&r.props).unwrap_or_else(|_| serde_json::json!({})),
            file_ids: serde_json::from_str(&r.file_ids).unwrap_or_default(),
            reply_count: r.reply_count,
            // Encrypted posts are bound to it (E2EE.md §6), so readers need it.
            pending_post_id: r.pending_post_id,
            ..Default::default()
        }
    }
}

pub async fn user(db: &Db, id: &str, viewer: &str) -> sqlx::Result<User> {
    let sql = format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?");
    Ok(sqlx::query_as::<_, UserRow>(&sql).bind(id).fetch_one(db).await?.into_user(viewer))
}

pub async fn channel(db: &Db, id: &str) -> sqlx::Result<Option<ChannelRow>> {
    let sql = format!("SELECT {CHANNEL_COLUMNS} FROM channels c WHERE c.id = ? AND c.delete_at = 0");
    sqlx::query_as(&sql).bind(id).fetch_optional(db).await
}

pub async fn member(db: &Db, channel_id: &str, user_id: &str) -> sqlx::Result<Option<ChannelMember>> {
    let sql = format!("SELECT {MEMBER_COLUMNS} FROM channel_members m WHERE m.channel_id = ? AND m.user_id = ?");
    Ok(sqlx::query_as::<_, MemberRow>(&sql).bind(channel_id).bind(user_id).fetch_optional(db).await?.map(Into::into))
}

pub async fn member_ids(db: &Db, channel_id: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar("SELECT user_id FROM channel_members WHERE channel_id = ?").bind(channel_id).fetch_all(db).await
}

pub async fn in_team(db: &Db, team_id: &str, user_id: &str) -> sqlx::Result<bool> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM team_members WHERE team_id = ? AND user_id = ?")
        .bind(team_id)
        .bind(user_id)
        .fetch_one(db)
        .await?;
    Ok(n > 0)
}

/// A post with its reactions.
pub async fn post(db: &Db, id: &str) -> sqlx::Result<Option<Post>> {
    let sql = format!("SELECT {POST_COLUMNS} FROM posts p WHERE p.id = ?");
    let Some(row) = sqlx::query_as::<_, PostRow>(&sql).bind(id).fetch_optional(db).await? else { return Ok(None) };
    let mut posts = vec![row.into()];
    with_metadata(db, &mut posts).await?;
    Ok(posts.pop())
}

/// Fills in `metadata.reactions` and `metadata.files`, oldest first, as
/// Mattermost sends them.
pub async fn with_metadata(db: &Db, posts: &mut [Post]) -> sqlx::Result<()> {
    if posts.is_empty() {
        return Ok(());
    }
    let ids = serde_json::to_string(&posts.iter().map(|p| &p.id).collect::<Vec<_>>()).unwrap_or_default();
    for file in crate::api::files::of_posts(db, &ids).await? {
        if let Some(post) = posts.iter_mut().find(|p| p.id == file.post_id) {
            post.metadata.files.push(file);
        }
    }
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT post_id, user_id, emoji_name, create_at FROM reactions \
         WHERE post_id IN (SELECT value FROM json_each(?1)) ORDER BY create_at, user_id",
    )
    .bind(ids)
    .fetch_all(db)
    .await?;
    for (post_id, user_id, emoji_name, create_at) in rows {
        if let Some(post) = posts.iter_mut().find(|p| p.id == post_id) {
            post.metadata.reactions.push(Reaction { user_id, post_id, emoji_name, create_at });
        }
    }
    // The custom emoji each post uses, in its text or its reactions, so apps
    // can fetch their pictures.
    let names: std::collections::BTreeSet<String> = posts
        .iter()
        .flat_map(|p| emoji_names(&p.message).chain(p.metadata.reactions.iter().map(|r| r.emoji_name.clone())))
        .collect();
    if !names.is_empty() {
        let list = serde_json::to_string(&names).unwrap_or_default();
        let custom: Vec<(String, String, String)> =
            sqlx::query_as("SELECT id, name, creator_id FROM emoji WHERE delete_at = 0 AND name IN (SELECT value FROM json_each(?1))")
                .bind(list)
                .fetch_all(db)
                .await?;
        for post in posts.iter_mut() {
            let used: std::collections::BTreeSet<String> =
                emoji_names(&post.message).chain(post.metadata.reactions.iter().map(|r| r.emoji_name.clone())).collect();
            for (id, name, creator_id) in custom.iter().filter(|(_, n, _)| used.contains(n)) {
                post.metadata.emojis.push(CustomEmoji { id: id.clone(), name: name.clone(), creator_id: creator_id.clone(), delete_at: 0 });
            }
        }
    }
    Ok(())
}

/// The `:name:` codes in a message.
fn emoji_names(message: &str) -> impl Iterator<Item = String> + '_ {
    let ok = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || "_+-".contains(c);
    let mut rest = message;
    std::iter::from_fn(move || {
        while let Some(start) = rest.find(':') {
            let after = &rest[start + 1..];
            let len = after.find(|c: char| !ok(c)).unwrap_or(after.len());
            if len > 0 && len <= 64 && after[len..].starts_with(':') {
                rest = &after[len + 1..];
                return Some(after[..len].to_string());
            }
            rest = after;
        }
        None
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn emoji_codes_in_text() {
        let found: Vec<String> = super::emoji_names("hi :grad: and :party-parrot: at 10:30").collect();
        assert_eq!(found, ["grad", "party-parrot"]);
    }
}
