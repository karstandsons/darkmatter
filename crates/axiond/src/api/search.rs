//! Message search, with Mattermost's syntax: words, `"exact phrases"`,
//! `prefix*`, `-excluded`, `from:alice`, `in:town-square`, and
//! `before:`/`after:`/`on:` dates (YYYY-MM-DD, in UTC).
//!
//! Only channels you're in, and never encrypted ones: the server can't read
//! those, so Darkmatter searches them on the device.

use axum::Json;
use axum::extract::{Path, State};
use mm_api::models::PostList;
use serde::Deserialize;

use crate::App;
use crate::auth::Me;
use crate::db::{self, POST_COLUMNS, PostRow};
use crate::error::{Error, Result};

#[derive(Debug, Default, PartialEq)]
struct Query {
    /// Words and phrases, each to match as a whole (or as a prefix with `*`).
    terms: Vec<String>,
    excluded: Vec<String>,
    from: Vec<String>,
    channels: Vec<String>,
    /// Unix ms bounds: created at or after `since`, before `until`.
    since: Option<i64>,
    until: Option<i64>,
}

fn parse(input: &str) -> Query {
    let mut q = Query::default();
    let mut rest = input.trim();
    while !rest.is_empty() {
        let negate = rest.starts_with('-');
        let body = if negate { &rest[1..] } else { rest };
        let (token, after, quoted) = if let Some(inner) = body.strip_prefix('"') {
            let end = inner.find('"').unwrap_or(inner.len());
            (&inner[..end], inner.get(end + 1..).unwrap_or(""), true)
        } else {
            let end = body.find(char::is_whitespace).unwrap_or(body.len());
            (&body[..end], &body[end..], false)
        };
        rest = after.trim_start();
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if !quoted && !negate && let Some((key, value)) = token.split_once(':') {
            let value = value.trim_start_matches(['@', '~']).to_lowercase();
            let day = || day_start(&value);
            match key.to_ascii_lowercase().as_str() {
                "from" if !value.is_empty() => q.from.push(value),
                "in" | "channel" if !value.is_empty() => q.channels.push(value),
                "before" => q.until = day().or(q.until),
                "after" => q.since = day().map(|d| d + DAY).or(q.since),
                "on" => {
                    if let Some(d) = day() {
                        (q.since, q.until) = (Some(d), Some(d + DAY));
                    }
                }
                _ => q.terms.push(token.into()),
            }
            continue;
        }
        if negate { q.excluded.push(token.into()) } else { q.terms.push(token.into()) }
    }
    q
}

const DAY: i64 = 86_400_000;

/// Midnight UTC at the start of a `YYYY-MM-DD` day, in Unix ms.
fn day_start(date: &str) -> Option<i64> {
    let mut parts = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Days from the civil calendar (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * DAY)
}

/// A term as an FTS5 string: quoted, so nothing in it is read as syntax,
/// with a trailing `*` kept as a prefix search.
fn fts_term(term: &str) -> Option<String> {
    let (word, prefix) = match term.strip_suffix('*') {
        Some(w) => (w, true),
        None => (term, false),
    };
    let word = word.trim();
    if word.is_empty() {
        return None;
    }
    Some(format!("\"{}\"{}", word.replace('"', "\"\""), if prefix { "*" } else { "" }))
}

/// The FTS5 MATCH expression, or `None` to search by filters alone.
fn fts_query(q: &Query, any: bool) -> Option<String> {
    let terms: Vec<String> = q.terms.iter().filter_map(|t| fts_term(t)).collect();
    if terms.is_empty() {
        return None;
    }
    let mut expr = format!("({})", terms.join(if any { " OR " } else { " AND " }));
    let excluded: Vec<String> = q.excluded.iter().filter_map(|t| fts_term(t)).collect();
    if !excluded.is_empty() {
        expr = format!("{expr} NOT ({})", excluded.join(" OR "));
    }
    Some(expr)
}

#[derive(Deserialize)]
pub struct Body {
    terms: String,
    #[serde(default)]
    is_or_search: bool,
    #[serde(default)]
    page: i64,
    per_page: Option<i64>,
}

enum Bind {
    Text(String),
    Int(i64),
}

pub async fn posts(State(app): State<App>, me: Me, Path(team): Path<String>, Json(body): Json<Body>) -> Result<Json<PostList>> {
    if !db::in_team(&app.db, &team, &me.user_id).await? {
        return Err(Error::forbidden());
    }
    let q = parse(&body.terms);
    let fts = fts_query(&q, body.is_or_search);
    if fts.is_none() && q.from.is_empty() && q.channels.is_empty() && q.since.is_none() && q.until.is_none() {
        return Ok(Json(PostList::default()));
    }

    let mut sql = format!(
        "SELECT {POST_COLUMNS} FROM posts p \
         JOIN channels c ON c.id = p.channel_id AND c.delete_at = 0 AND c.encrypted = 0 AND (c.team_id = ?1 OR c.team_id = '') \
         JOIN channel_members m ON m.channel_id = c.id AND m.user_id = ?2 \
         WHERE p.delete_at = 0 AND p.type = ''"
    );
    let mut binds = vec![Bind::Text(team), Bind::Text(me.user_id.clone())];
    let mut next = |sql: &mut String, cond: &str, bind: Bind| {
        binds.push(bind);
        sql.push_str(&cond.replace("?N", &format!("?{}", binds.len())));
    };
    if let Some(fts) = fts {
        next(&mut sql, " AND p.rowid IN (SELECT rowid FROM posts_fts WHERE posts_fts MATCH ?N)", Bind::Text(fts));
    } else if !q.excluded.is_empty() {
        // Exclusions with no words to search: drop posts containing them.
        let ex: Vec<String> = q.excluded.iter().filter_map(|t| fts_term(t)).collect();
        next(&mut sql, " AND p.rowid NOT IN (SELECT rowid FROM posts_fts WHERE posts_fts MATCH ?N)", Bind::Text(ex.join(" OR ")));
    }
    if !q.from.is_empty() {
        let names = serde_json::to_string(&q.from).unwrap_or_default();
        next(&mut sql, " AND p.user_id IN (SELECT u.id FROM users u JOIN json_each(?N) j ON u.username = j.value)", Bind::Text(names));
    }
    if !q.channels.is_empty() {
        let names = serde_json::to_string(&q.channels).unwrap_or_default();
        next(&mut sql, " AND c.name IN (SELECT value FROM json_each(?N))", Bind::Text(names));
    }
    if let Some(since) = q.since {
        next(&mut sql, " AND p.create_at >= ?N", Bind::Int(since));
    }
    if let Some(until) = q.until {
        next(&mut sql, " AND p.create_at < ?N", Bind::Int(until));
    }
    let per_page = body.per_page.unwrap_or(60).clamp(1, 200);
    next(&mut sql, " ORDER BY p.create_at DESC, p.id DESC LIMIT ?N", Bind::Int(per_page));
    next(&mut sql, " OFFSET ?N", Bind::Int(body.page.max(0) * per_page));

    let mut query = sqlx::query_as::<_, PostRow>(&sql);
    for bind in binds {
        query = match bind {
            Bind::Text(t) => query.bind(t),
            Bind::Int(i) => query.bind(i),
        };
    }
    let rows = match query.fetch_all(&app.db).await {
        Ok(rows) => rows,
        // A query FTS5 can't read (stray syntax): no results rather than an error.
        Err(sqlx::Error::Database(e)) if e.message().contains("fts5") => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    Ok(Json(super::posts::list(&app, rows).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mattermost_search_syntax() {
        let q = parse(r#"lunch "noon meeting" -cake from:@Bob in:~town-square caf*"#);
        assert_eq!(q.terms, ["lunch", "noon meeting", "caf*"]);
        assert_eq!((q.excluded, q.from, q.channels), (vec!["cake".to_string()], vec!["bob".to_string()], vec!["town-square".to_string()]));
        assert_eq!(fts_query(&parse(r#"a "b c" d*"#), false).unwrap(), r#"("a" AND "b c" AND "d"*)"#);
        assert_eq!(fts_query(&parse("a b -c"), true).unwrap(), r#"("a" OR "b") NOT ("c")"#);
        assert_eq!(fts_term(r#"a"b"#).unwrap(), r#""a""b""#);
        assert!(fts_query(&parse("from:bob"), false).is_none());
        assert_eq!(parse("time:12:30").terms, ["time:12:30"]);
    }

    #[test]
    fn dates_are_utc_days() {
        assert_eq!(day_start("1970-01-01"), Some(0));
        assert_eq!(day_start("2026-10-19"), Some(1_792_368_000_000));
        assert_eq!(day_start("2024-02-29"), Some(1_709_164_800_000));
        assert_eq!(day_start("2026-13-01"), None);
        assert_eq!(day_start("soon"), None);
        let q = parse("on:2026-10-19");
        assert_eq!((q.since, q.until), (Some(1_792_368_000_000), Some(1_792_368_000_000 + DAY)));
        assert_eq!(parse("after:2026-10-19").since, Some(1_792_368_000_000 + DAY));
        assert_eq!(parse("before:2026-10-19").until, Some(1_792_368_000_000));
    }
}
