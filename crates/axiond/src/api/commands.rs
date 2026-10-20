//! Slash commands run on the server, as Mattermost names them. Darkmatter's
//! own commands (/split, /theme…) never get here.

use axum::Json;
use axum::extract::{Query, State};
use mm_api::models::{CommandSuggestion, Post};
use serde::Deserialize;
use serde_json::{Value, json};

use super::channels::{self, readable};
use super::posts;
use crate::auth::Me;
use crate::db::{self, CHANNEL_COLUMNS, ChannelRow};
use crate::error::{Error, Result};
use crate::hub::Event;
use crate::{App, ids};

/// Every command: trigger, argument hint, description.
const COMMANDS: &[(&str, &str, &str)] = &[
    ("away", "", "Set your status to away"),
    ("dnd", "", "Set your status to do not disturb"),
    ("header", "[text]", "Set the channel header"),
    ("help", "", "List the server's commands"),
    ("invite", "@username [~channel]", "Add someone to a channel"),
    ("join", "~channel", "Join a public channel"),
    ("kick", "@username", "Remove someone from this channel"),
    ("leave", "", "Leave this channel"),
    ("me", "[text]", "Say something about yourself, in italics"),
    ("msg", "@username [text]", "Send a direct message"),
    ("mute", "", "Mute or unmute this channel"),
    ("offline", "", "Set your status to offline"),
    ("online", "", "Set your status to online"),
    ("purpose", "[text]", "Set the channel purpose"),
    ("remove", "@username", "Remove someone from this channel"),
    ("shrug", "[text]", "Add ¯\\_(ツ)_/¯ to a message"),
];

#[derive(Deserialize)]
pub struct Execute {
    command: String,
    channel_id: String,
    #[serde(default)]
    root_id: String,
}

/// What a command answers: shown only to whoever ran it, or nothing.
struct Reply {
    text: String,
    goto: String,
}

impl Reply {
    fn say(text: impl Into<String>) -> Self {
        Reply { text: text.into(), goto: String::new() }
    }

    fn quiet() -> Self {
        Reply::say("")
    }
}

pub async fn execute(State(app): State<App>, me: Me, Json(body): Json<Execute>) -> Result<Json<Value>> {
    let channel = readable(&app, &me, &body.channel_id).await?;
    let line = body.command.trim();
    let Some(line) = line.strip_prefix('/') else { return Err(Error::bad_request("Commands start with /.")) };
    let (trigger, args) = line.split_once(char::is_whitespace).map_or((line, ""), |(t, a)| (t, a.trim()));
    let trigger = trigger.to_ascii_lowercase();
    let reply = run(&app, &me, &channel, &body.root_id, &trigger, args).await?;
    if !reply.text.is_empty() {
        ephemeral(&app, &me.user_id, &channel.id, &body.root_id, &reply.text);
    }
    Ok(Json(json!({ "response_type": "ephemeral", "text": reply.text, "goto_location": reply.goto })))
}

async fn run(app: &App, me: &Me, channel: &ChannelRow, root_id: &str, trigger: &str, args: &str) -> Result<Reply> {
    let team = team_of(app, me, channel).await?;
    match trigger {
        "away" | "dnd" | "offline" | "online" => {
            super::status::set(app, &me.user_id, trigger).await?;
            let said = match trigger {
                "away" => "You are now away.",
                "dnd" => "Do not disturb is on. You won't get notifications.",
                "offline" => "You now appear offline.",
                _ => "You are now online.",
            };
            Ok(Reply::say(said))
        }
        "me" | "shrug" => {
            let message = if trigger == "me" { format!("*{args}*") } else { format!("{args} ¯\\\\\\_(ツ)\\_/¯").trim_start().to_string() };
            if trigger == "me" && args.is_empty() {
                return Ok(Reply::say("Usage: /me text"));
            }
            post_text(app, me, channel, root_id, message).await?;
            Ok(Reply::quiet())
        }
        "join" => {
            let name = args.trim_start_matches('~').to_ascii_lowercase();
            if name.is_empty() {
                return Ok(Reply::say("Usage: /join ~channel"));
            }
            let sql = format!("SELECT {CHANNEL_COLUMNS} FROM channels c WHERE c.team_id = ? AND c.name = ? AND c.delete_at = 0");
            let found: Option<ChannelRow> = sqlx::query_as(&sql).bind(&team.0).bind(&name).fetch_optional(&app.db).await?;
            let Some(target) = found.filter(|c| c.kind == "O") else { return Ok(Reply::say(format!("There's no public channel ~{name}."))) };
            channels::join_channel(app, &me.user_id, &target).await?;
            Ok(Reply { text: String::new(), goto: format!("/{}/channels/{}", team.1, target.name) })
        }
        "leave" => {
            channels::leave_channel(app, &me.user_id, channel).await?;
            Ok(Reply { text: String::new(), goto: format!("/{}/channels/town-square", team.1) })
        }
        "msg" => {
            let (who, text) = args.split_once(char::is_whitespace).map_or((args, ""), |(w, t)| (w, t.trim()));
            let Some(other) = user_named(app, who).await? else { return Ok(Reply::say("Usage: /msg @username [text]")) };
            let dm = channels::direct_channel(app, &me.user_id, &other).await?;
            let goto = format!("/{}/channels/{}", team.1, dm.name);
            if !text.is_empty() {
                // The server can't write in an encrypted conversation.
                if dm.encrypted {
                    return Ok(Reply { text: "That conversation is end-to-end encrypted, so the server can't send it for you: type it there.".into(), goto });
                }
                post_text(app, me, &dm, "", text.to_string()).await?;
            }
            Ok(Reply { text: String::new(), goto })
        }
        "header" | "purpose" => {
            if matches!(channel.kind.as_str(), "O" | "P") && db::member(&app.db, &channel.id, &me.user_id).await?.is_none() {
                return Err(Error::forbidden());
            }
            set_channel_text(app, me, channel, trigger, args).await?;
            Ok(Reply::quiet())
        }
        "invite" => {
            let mut words = args.split_whitespace();
            let Some(other) = user_named(app, words.next().unwrap_or_default()).await? else {
                return Ok(Reply::say("Usage: /invite @username [~channel]"));
            };
            let target = match words.next() {
                Some(name) => {
                    let sql = format!("SELECT {CHANNEL_COLUMNS} FROM channels c WHERE c.team_id = ? AND c.name = ? AND c.delete_at = 0");
                    let found: Option<ChannelRow> =
                        sqlx::query_as(&sql).bind(&team.0).bind(name.trim_start_matches('~')).fetch_optional(&app.db).await?;
                    found.ok_or_else(|| Error::not_found("Channel"))?
                }
                None => channel.clone(),
            };
            invite(app, me, &target, &other).await
        }
        "kick" | "remove" => {
            let Some(other) = user_named(app, args).await? else { return Ok(Reply::say(format!("Usage: /{trigger} @username"))) };
            kick(app, me, channel, &other).await
        }
        "mute" => {
            let member = db::member(&app.db, &channel.id, &me.user_id).await?.ok_or_else(Error::forbidden)?;
            let muted = !member.is_muted();
            let mut props = member.notify_props.clone();
            props.insert("mark_unread".into(), if muted { "mention" } else { "all" }.into());
            let json = serde_json::to_string(&props).map_err(|_| Error::internal())?;
            sqlx::query("UPDATE channel_members SET notify_props = ?1, last_update_at = ?2 WHERE channel_id = ?3 AND user_id = ?4")
                .bind(json)
                .bind(ids::now())
                .bind(&channel.id)
                .bind(&me.user_id)
                .execute(&app.db)
                .await?;
            let member = db::member(&app.db, &channel.id, &me.user_id).await?.ok_or_else(Error::internal)?;
            let data = json!({ "channelMember": serde_json::to_string(&member).map_err(|_| Error::internal())? });
            app.hub.send(std::slice::from_ref(&me.user_id), &Event::new("channel_member_updated", data).user(&me.user_id));
            Ok(Reply::say(if muted { "Muted this channel." } else { "Unmuted this channel." }))
        }
        "help" => {
            let lines: Vec<String> = COMMANDS.iter().map(|(t, hint, about)| format!("- `/{t}{}{hint}`: {about}", if hint.is_empty() { "" } else { " " })).collect();
            Ok(Reply::say(format!("The server's commands:\n{}", lines.join("\n"))))
        }
        _ => Err(Error {
            status: axum::http::StatusCode::NOT_FOUND,
            id: "api.command.execute_command.not_found.app_error",
            message: format!("There's no /{trigger} command."),
        }),
    }
}

/// The team a command runs in: the channel's, or for a direct message the
/// user's first team. (id, name)
async fn team_of(app: &App, me: &Me, channel: &ChannelRow) -> Result<(String, String)> {
    let row: Option<(String, String)> = if channel.team_id.is_empty() {
        sqlx::query_as("SELECT t.id, t.name FROM teams t JOIN team_members m ON m.team_id = t.id WHERE m.user_id = ? ORDER BY t.create_at LIMIT 1")
            .bind(&me.user_id)
            .fetch_optional(&app.db)
            .await?
    } else {
        sqlx::query_as("SELECT id, name FROM teams WHERE id = ?").bind(&channel.team_id).fetch_optional(&app.db).await?
    };
    row.ok_or_else(|| Error::not_found("Team"))
}

async fn user_named(app: &App, name: &str) -> Result<Option<String>> {
    let name = name.trim().trim_start_matches('@').to_ascii_lowercase();
    if name.is_empty() {
        return Ok(None);
    }
    Ok(sqlx::query_scalar("SELECT id FROM users WHERE username = ? AND delete_at = 0").bind(name).fetch_optional(&app.db).await?)
}

/// Posts text as the user, as if typed. Not into encrypted channels, where
/// the server must never write readable text.
async fn post_text(app: &App, me: &Me, channel: &ChannelRow, root_id: &str, message: String) -> Result<Post> {
    if channel.encrypted {
        return Err(Error::bad_request("This channel is encrypted, so the server can't post in it for you."));
    }
    if db::member(&app.db, &channel.id, &me.user_id).await?.is_none() {
        return Err(Error::forbidden());
    }
    let new = posts::New { root_id: root_id.into(), message, kind: "", pending_post_id: String::new(), file_ids: Vec::new(), props: serde_json::json!({}) };
    posts::insert(app, channel, &me.user_id, new).await
}

async fn set_channel_text(app: &App, me: &Me, channel: &ChannelRow, what: &str, text: &str) -> Result<()> {
    let text: String = text.chars().take(if what == "header" { 1024 } else { 250 }).collect();
    let sql = format!("UPDATE channels SET {what} = ?1, update_at = ?2 WHERE id = ?3");
    sqlx::query(&sql).bind(&text).bind(ids::now()).bind(&channel.id).execute(&app.db).await?;
    let user = db::user(&app.db, &me.user_id, &me.user_id).await?;
    let said = if text.is_empty() { format!("@{} removed the channel {what}.", user.username) } else { format!("@{} changed the channel {what} to: {text}", user.username) };
    let kind = if what == "header" { "system_header_change" } else { "system_purpose_change" };
    posts::insert(app, channel, &me.user_id, posts::New::system(kind, said)).await?;
    let updated = db::channel(&app.db, &channel.id).await?.ok_or_else(Error::internal)?;
    let data = json!({ "channel": serde_json::to_string(&mm_api::models::Channel::from(updated)).map_err(|_| Error::internal())? });
    app.hub.send(&db::member_ids(&app.db, &channel.id).await?, &Event::new("channel_updated", data).channel(&channel.id));
    Ok(())
}

/// Adds someone in the team to a channel you're in.
async fn invite(app: &App, me: &Me, channel: &ChannelRow, other: &str) -> Result<Reply> {
    if matches!(channel.kind.as_str(), "D" | "G") {
        return Ok(Reply::say("Direct messages can't have people added."));
    }
    if db::member(&app.db, &channel.id, &me.user_id).await?.is_none() {
        return Err(Error::forbidden());
    }
    let name = db::user(&app.db, other, &me.user_id).await?.username;
    if !db::in_team(&app.db, &channel.team_id, other).await? {
        return Ok(Reply::say(format!("@{name} isn't in this team.")));
    }
    if db::member(&app.db, &channel.id, other).await?.is_some() {
        return Ok(Reply::say(format!("@{name} is already in ~{}.", channel.name)));
    }
    channels::add_member(&app.db, channel, other).await?;
    let by = db::user(&app.db, &me.user_id, &me.user_id).await?.username;
    posts::insert(app, channel, &me.user_id, posts::New::system("system_add_to_channel", format!("@{name} added to the channel by @{by}."))).await?;
    // Their apps load the channel.
    app.hub.send(&[other.to_string()], &Event::new("user_added", json!({ "user_id": other, "team_id": channel.team_id })).channel(&channel.id));
    Ok(Reply::quiet())
}

/// Removes someone from a channel: admins, or whoever made the channel.
async fn kick(app: &App, me: &Me, channel: &ChannelRow, other: &str) -> Result<Reply> {
    if !me.is_admin() && channel.creator_id != me.user_id {
        return Err(Error::forbidden());
    }
    if matches!(channel.kind.as_str(), "D" | "G") || channel.name == "town-square" {
        return Ok(Reply::say("No one can be removed from this channel."));
    }
    let gone = sqlx::query("DELETE FROM channel_members WHERE channel_id = ? AND user_id = ?").bind(&channel.id).bind(other).execute(&app.db).await?;
    if gone.rows_affected() == 0 {
        return Ok(Reply::say("They aren't in this channel."));
    }
    let name = db::user(&app.db, other, &me.user_id).await?.username;
    let by = db::user(&app.db, &me.user_id, &me.user_id).await?.username;
    posts::insert(app, channel, &me.user_id, posts::New::system("system_remove_from_channel", format!("@{name} was removed from the channel by @{by}."))).await?;
    app.hub.send(&[other.to_string()], &Event::new("user_removed", json!({ "channel_id": channel.id, "remover_id": me.user_id })).user(other));
    crate::e2ee::user_left(app, &channel.id, other).await?;
    Ok(Reply::quiet())
}

/// A message only `user_id` sees, where the command was typed.
fn ephemeral(app: &App, user_id: &str, channel_id: &str, root_id: &str, text: &str) {
    let now = ids::now();
    let post = Post {
        id: ids::new_id(),
        create_at: now,
        update_at: now,
        user_id: user_id.into(),
        channel_id: channel_id.into(),
        root_id: root_id.into(),
        message: text.into(),
        kind: "system_ephemeral".into(),
        props: json!({}),
        ..Default::default()
    };
    let data = json!({ "post": serde_json::to_string(&post).unwrap_or_default() });
    app.hub.send(&[user_id.to_string()], &Event::new("ephemeral_message", data).channel(channel_id).user(user_id));
}

#[derive(Deserialize)]
pub struct Suggest {
    #[serde(default)]
    user_input: String,
}

/// Completions while someone types `/jo`: the commands that start that way.
pub async fn suggestions(_: Me, Query(q): Query<Suggest>) -> Json<Vec<CommandSuggestion>> {
    let typed = q.user_input.trim_start_matches('/');
    if typed.contains(char::is_whitespace) {
        return Json(Vec::new());
    }
    let typed = typed.to_ascii_lowercase();
    Json(
        COMMANDS
            .iter()
            .filter(|(t, _, _)| t.starts_with(&typed))
            .map(|(t, hint, about)| CommandSuggestion { complete: t.to_string(), suggestion: format!("/{t}"), hint: hint.to_string(), description: about.to_string() })
            .collect(),
    )
}
