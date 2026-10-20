//! Server-side setup the `axiond` command runs: the first admin and team,
//! adding people, resetting passwords, adding channels.

use anyhow::{Context, bail};

use crate::{App, auth, ids};

pub struct NewUser {
    pub username: String,
    pub email: String,
    pub password: String,
}

/// What a username may be (as in Mattermost).
pub fn check_username(name: &str) -> anyhow::Result<()> {
    let ok = (3..=22).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'));
    if !ok {
        bail!("usernames are 3–22 characters: lowercase letters, digits, '.', '-' or '_', starting with a letter");
    }
    Ok(())
}

/// What a team or channel name may be: the URL form, e.g. `town-square`.
pub fn check_name(name: &str) -> anyhow::Result<()> {
    let ok = (2..=64).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
        && !name.starts_with('-');
    if !ok {
        bail!("names are 2–64 characters: lowercase letters, digits, '-' or '_' (e.g. town-square)");
    }
    Ok(())
}

/// Adds a custom emoji from a picture file, as the first admin.
pub async fn add_emoji(app: &App, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let admin: String = sqlx::query_scalar("SELECT id FROM users WHERE roles LIKE '%system_admin%' AND delete_at = 0 ORDER BY create_at LIMIT 1")
        .fetch_optional(&app.db)
        .await?
        .context("set up the server first")?;
    crate::api::images::add_emoji(app, &admin, name, bytes).await?;
    Ok(())
}

/// Sets someone's profile picture from a picture file.
pub async fn set_picture(app: &App, username: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
    let id: String = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(&app.db)
        .await?
        .with_context(|| format!("there's no {username}"))?;
    crate::api::images::set_picture(app, &id, bytes).await?;
    Ok(())
}

async fn id_of(app: &App, username: &str) -> anyhow::Result<String> {
    sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
        .bind(username.trim().trim_start_matches('@').to_lowercase())
        .fetch_optional(&app.db)
        .await?
        .with_context(|| format!("there's no {username}"))
}

/// A new invite code, shown once.
pub async fn invite(app: &App, uses: i64, days: i64, note: &str) -> anyhow::Result<(String, i64)> {
    Ok(crate::api::accounts::create_invite(app, "", uses, days, note).await?)
}

/// Deactivates (or reactivates) someone; deactivating signs them out.
pub async fn set_active(app: &App, username: &str, active: bool) -> anyhow::Result<()> {
    let id = id_of(app, username).await?;
    Ok(crate::api::accounts::set_active(app, "", &id, active).await?)
}

/// Deletes someone's account for good.
pub async fn delete_user(app: &App, username: &str, delete_messages: bool) -> anyhow::Result<()> {
    let id = id_of(app, username).await?;
    Ok(crate::api::accounts::delete_account(app, "", &id, delete_messages).await?)
}

/// Open reports, one line each.
pub async fn open_reports(app: &App) -> anyhow::Result<Vec<String>> {
    let rows: Vec<(String, i64, String, String, String)> = sqlx::query_as(
        "SELECT r.id, r.create_at, COALESCE(a.username, '?'), COALESCE(u.username, ''), r.reason FROM reports r \
         LEFT JOIN users a ON a.id = r.reporter_id LEFT JOIN users u ON u.id = r.user_id WHERE r.resolved_at = 0 ORDER BY r.create_at",
    )
    .fetch_all(&app.db)
    .await?;
    Ok(rows.into_iter().map(|(id, at, by, about, reason)| format!("{id}  {at}  @{by} reported @{about}: {reason}")).collect())
}

/// Turns off someone's two-factor codes (a lost phone) and signs them out.
pub async fn mfa_off(app: &App, username: &str) -> anyhow::Result<()> {
    let done = sqlx::query("UPDATE users SET mfa_active = 0, mfa_secret = '', mfa_last_step = 0 WHERE username = ?")
        .bind(username)
        .execute(&app.db)
        .await?;
    if done.rows_affected() == 0 {
        bail!("there's no {username}");
    }
    sqlx::query("DELETE FROM sessions WHERE user_id = (SELECT id FROM users WHERE username = ?)").bind(username).execute(&app.db).await?;
    let id = id_of(app, username).await?;
    crate::audit::record(&app.db, "", "user.mfa_off", &id, "").await?;
    Ok(())
}

/// Makes the first admin and their team, with Town Square and Off-Topic.
/// Only on an empty server.
pub async fn setup(app: &App, admin: NewUser, team: &str, team_display: &str) -> anyhow::Result<()> {
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users").fetch_one(&app.db).await?;
    if users > 0 {
        bail!("this server is already set up; add people with `axiond user add`");
    }
    check_name(team)?;
    let (team_id, now) = (ids::new_id(), ids::now());
    sqlx::query("INSERT INTO teams (id, create_at, update_at, name, display_name) VALUES (?, ?, ?, ?, ?)")
        .bind(&team_id)
        .bind(now)
        .bind(now)
        .bind(team)
        .bind(team_display)
        .execute(&app.db)
        .await?;
    add_channel(app, team, "town-square", "Town Square").await?;
    add_channel(app, team, "off-topic", "Off-Topic").await?;
    let id = add_user(app, admin, true).await?;
    crate::audit::record(&app.db, "", "server.setup", &id, &format!("team {team}; first admin")).await?;
    Ok(())
}

/// Adds someone to every team, in each team's Town Square and Off-Topic.
pub async fn add_user(app: &App, user: NewUser, admin: bool) -> anyhow::Result<String> {
    let username = user.username.trim().to_lowercase();
    check_username(&username)?;
    let email = user.email.trim().to_lowercase();
    if !email.contains('@') {
        bail!("that email address doesn't look right");
    }
    auth::check_new_password(&user.password).map_err(anyhow::Error::msg)?;
    let taken: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = ? OR email = ?")
        .bind(&username)
        .bind(&email)
        .fetch_one(&app.db)
        .await?;
    if taken > 0 {
        bail!("someone already has that username or email");
    }
    let hash = auth::hash_password(&user.password)?;
    let (id, now) = (ids::new_id(), ids::now());
    let roles = if admin { "system_admin system_user" } else { "system_user" };
    sqlx::query("INSERT INTO users (id, create_at, update_at, username, email, password_hash, roles) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(now)
        .bind(now)
        .bind(&username)
        .bind(&email)
        .bind(&hash)
        .bind(roles)
        .execute(&app.db)
        .await?;
    let teams: Vec<String> = sqlx::query_scalar("SELECT id FROM teams WHERE delete_at = 0").fetch_all(&app.db).await?;
    for team in teams {
        let role = if admin { "team_user team_admin" } else { "team_user" };
        sqlx::query("INSERT INTO team_members (team_id, user_id, roles) VALUES (?, ?, ?)").bind(&team).bind(&id).bind(role).execute(&app.db).await?;
        for channel in ["town-square", "off-topic"] {
            let found = sqlx::query_as::<_, crate::db::ChannelRow>(&format!(
                "SELECT {} FROM channels c WHERE c.team_id = ? AND c.name = ? AND c.delete_at = 0",
                crate::db::CHANNEL_COLUMNS
            ))
            .bind(&team)
            .bind(channel)
            .fetch_optional(&app.db)
            .await?;
            if let Some(channel) = found {
                crate::api::add_member(&app.db, &channel, &id).await?;
            }
        }
    }
    Ok(id)
}

/// Sets a new password and signs the person out everywhere.
pub async fn set_password(app: &App, username: &str, password: &str) -> anyhow::Result<()> {
    auth::check_new_password(password).map_err(anyhow::Error::msg)?;
    let hash = auth::hash_password(password)?;
    let id: String = sqlx::query_scalar("SELECT id FROM users WHERE username = ?")
        .bind(username.trim().to_lowercase())
        .fetch_optional(&app.db)
        .await?
        .with_context(|| format!("no user called {username}"))?;
    sqlx::query("UPDATE users SET password_hash = ?, update_at = ? WHERE id = ?").bind(&hash).bind(ids::now()).bind(&id).execute(&app.db).await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = ?").bind(&id).execute(&app.db).await?;
    crate::audit::record(&app.db, "", "user.password", &id, "").await?;
    Ok(())
}

/// Adds a public channel to a team, with everyone in the team in it.
pub async fn add_channel(app: &App, team: &str, name: &str, display: &str) -> anyhow::Result<String> {
    check_name(name)?;
    let team_id: String = sqlx::query_scalar("SELECT id FROM teams WHERE name = ? AND delete_at = 0")
        .bind(team)
        .fetch_optional(&app.db)
        .await?
        .with_context(|| format!("no team called {team}"))?;
    let (id, now) = (ids::new_id(), ids::now());
    sqlx::query("INSERT INTO channels (id, team_id, type, name, display_name, create_at, update_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(&team_id)
        .bind("O")
        .bind(name)
        .bind(display)
        .bind(now)
        .bind(now)
        .execute(&app.db)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => anyhow::anyhow!("~{name} already exists in {team}"),
            e => e.into(),
        })?;
    sqlx::query(
        "INSERT INTO channel_members (channel_id, user_id, last_viewed_at, last_update_at) \
         SELECT ?, user_id, ?, ? FROM team_members WHERE team_id = ?",
    )
    .bind(&id)
    .bind(now)
    .bind(now)
    .bind(&team_id)
    .execute(&app.db)
    .await?;
    Ok(id)
}
