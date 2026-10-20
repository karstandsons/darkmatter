use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use mm_api::models::{Post, Reaction};
use serde::Deserialize;
use serde_json::{Value, json};

use super::channels::readable;
use crate::auth::Me;
use crate::db;
use crate::error::{Error, Result};
use crate::hub::Event;
use crate::{App, ids};

#[derive(Deserialize)]
pub struct New {
    user_id: String,
    post_id: String,
    emoji_name: String,
}

/// Emoji names as Mattermost allows them: `thumbsup`, `+1`, `party-parrot`.
fn check_emoji(name: &str) -> Result<()> {
    let ok = (1..=64).contains(&name.len()) && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_+-".contains(c));
    if ok { Ok(()) } else { Err(Error::bad_request("That isn't an emoji name.")) }
}

/// The post, if `me` may react to it: a live post in a channel they're in.
async fn reactable(app: &App, me: &Me, post_id: &str) -> Result<Post> {
    let post = db::post(&app.db, post_id).await?.filter(|p| p.delete_at == 0).ok_or_else(|| Error::not_found("Message"))?;
    if db::member(&app.db, &post.channel_id, &me.user_id).await?.is_none() {
        return Err(Error::forbidden());
    }
    Ok(post)
}

pub async fn add(State(app): State<App>, me: Me, Json(body): Json<New>) -> Result<(StatusCode, Json<Reaction>)> {
    if body.user_id != me.user_id {
        return Err(Error::forbidden());
    }
    check_emoji(&body.emoji_name)?;
    let post = reactable(&app, &me, &body.post_id).await?;
    let now = ids::now();
    let added = sqlx::query("INSERT OR IGNORE INTO reactions (post_id, user_id, emoji_name, create_at) VALUES (?, ?, ?, ?)")
        .bind(&post.id)
        .bind(&me.user_id)
        .bind(&body.emoji_name)
        .bind(now)
        .execute(&app.db)
        .await?;
    let reaction: Reaction = {
        let (create_at,): (i64,) = sqlx::query_as("SELECT create_at FROM reactions WHERE post_id = ? AND user_id = ? AND emoji_name = ?")
            .bind(&post.id)
            .bind(&me.user_id)
            .bind(&body.emoji_name)
            .fetch_one(&app.db)
            .await?;
        Reaction { user_id: me.user_id.clone(), post_id: post.id.clone(), emoji_name: body.emoji_name, create_at }
    };
    if added.rows_affected() > 0 {
        changed(&app, &post, "reaction_added", &reaction).await?;
    }
    Ok((StatusCode::CREATED, Json(reaction)))
}

pub async fn remove(State(app): State<App>, me: Me, Path((user, post_id, emoji)): Path<(String, String, String)>) -> Result<Json<Value>> {
    if user != me.user_id {
        return Err(Error::forbidden());
    }
    let post = reactable(&app, &me, &post_id).await?;
    let gone = sqlx::query("DELETE FROM reactions WHERE post_id = ? AND user_id = ? AND emoji_name = ?")
        .bind(&post.id)
        .bind(&me.user_id)
        .bind(&emoji)
        .execute(&app.db)
        .await?;
    if gone.rows_affected() > 0 {
        let reaction = Reaction { user_id: me.user_id.clone(), post_id: post.id.clone(), emoji_name: emoji, create_at: ids::now() };
        changed(&app, &post, "reaction_removed", &reaction).await?;
    }
    super::ok()
}

pub async fn of_post(State(app): State<App>, me: Me, Path(post_id): Path<String>) -> Result<Json<Vec<Reaction>>> {
    let post = db::post(&app.db, &post_id).await?.filter(|p| p.delete_at == 0).ok_or_else(|| Error::not_found("Message"))?;
    readable(&app, &me, &post.channel_id).await?;
    Ok(Json(post.metadata.reactions))
}

/// Marks the post changed (so `since` fetches pick it up) and tells the channel.
async fn changed(app: &App, post: &Post, event: &'static str, reaction: &Reaction) -> Result<()> {
    sqlx::query("UPDATE posts SET update_at = ? WHERE id = ?").bind(ids::now()).bind(&post.id).execute(&app.db).await?;
    let members = db::member_ids(&app.db, &post.channel_id).await?;
    let data = json!({ "reaction": serde_json::to_string(reaction).map_err(|_| Error::internal())? });
    app.hub.send(&members, &Event::new(event, data).channel(&post.channel_id));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emoji_names() {
        for ok in ["thumbsup", "+1", "party-parrot", "100", "e_mail"] {
            assert!(check_emoji(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Thumbsup", "a b", "x/y", ":smile:", &"a".repeat(65)] {
            assert!(check_emoji(bad).is_err(), "{bad}");
        }
    }
}
