//! Text helpers that don't depend on the UI.

use chrono::{DateTime, Local, TimeZone};

/// Normalize what the user typed as a server address. Without a scheme, local
/// addresses get `http://` (dev servers) and everything else `https://`, so a
/// password is never sent to a remote host in plain text by accident.
pub fn server_url(input: &str) -> String {
    let s = input.trim().trim_end_matches('/');
    if s.contains("://") {
        return s.to_string();
    }
    let host = s.split(['/', ':']).next().unwrap_or_default();
    let local = host == "localhost" || host.starts_with("127.") || s.starts_with("[::1]");
    format!("{}://{s}", if local { "http" } else { "https" })
}

/// Turn a Mattermost message into the markdown we render: emoji shortcodes,
/// bare URLs as links, `@mentions` as `mention:` links, and each newline a
/// line break (as Mattermost shows them; plain markdown joins the lines).
/// Code is left alone.
pub fn prepare(message: &str) -> String {
    let mut out = String::with_capacity(message.len() + 16);
    for (i, block) in message.split("```").enumerate() {
        if i > 0 {
            out.push_str("```");
        }
        if i % 2 == 1 {
            out.push_str(block);
            continue;
        }
        for (j, span) in block.split('`').enumerate() {
            if j > 0 {
                out.push('`');
            }
            if j % 2 == 1 {
                out.push_str(span);
            } else {
                out.push_str(&hard_breaks(&linkify(&emojify(span))));
            }
        }
    }
    out
}

/// End each line that has text with two spaces, markdown's line break.
fn hard_breaks(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut prev = '\n';
    for c in s.chars() {
        if c == '\n' && prev != '\n' {
            out.push_str("  ");
        }
        out.push(c);
        prev = c;
    }
    out
}

fn linkify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    let mut prev: Option<char> = None;
    while let Some(c) = rest.chars().next() {
        // `(` counts as a boundary for "(see https://…)", but not inside `[text](url)`.
        let at_boundary = prev.is_none_or(|p| p.is_whitespace() || (p == '(' && !out.ends_with("](")));
        if at_boundary && (rest.starts_with("https://") || rest.starts_with("http://")) {
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')']);
            out.push('<');
            out.push_str(url);
            out.push('>');
            rest = &rest[url.len()..];
            prev = url.chars().last();
            continue;
        }
        if at_boundary && c == '@' {
            let name_len = rest[1..]
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || "._-".contains(ch)))
                .unwrap_or(rest.len() - 1);
            let name = rest[1..1 + name_len].trim_end_matches(['.', '-', '_']);
            if !name.is_empty() {
                out.push_str(&format!("[@{name}](mention:{name})"));
                rest = &rest[1 + name.len()..];
                prev = name.chars().last();
                continue;
            }
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
        prev = Some(c);
    }
    out
}

/// Replace `:shortcode:` with the emoji, leaving unknown codes (custom emoji) as-is.
pub fn emojify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find(':') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find(':') {
            Some(end) if end > 0 && after[..end].chars().all(|c| c.is_ascii_alphanumeric() || "_+-".contains(c)) => {
                match emoji(&after[..end]) {
                    Some(e) => {
                        out.push_str(e);
                        rest = &after[end + 1..];
                    }
                    None => {
                        out.push(':');
                        rest = after;
                    }
                }
            }
            _ => {
                out.push(':');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// One piece of a message made only of emoji.
#[derive(Debug, PartialEq)]
pub enum EmojiToken<'a> {
    /// `:name:`, built in or custom.
    Code(&'a str),
    /// A Unicode emoji typed directly.
    Glyph(&'a str),
}

/// The emoji of a message that is nothing but emoji (and spaces), like
/// `:rustacean: :tada:`; `None` if it has any other text. Shown large, as
/// Mattermost does, and the only place custom emoji can be drawn as images.
pub fn emoji_only(message: &str) -> Option<Vec<EmojiToken<'_>>> {
    let mut out = Vec::new();
    for word in message.split_whitespace() {
        if let Some(inner) = word.strip_prefix(':').and_then(|w| w.strip_suffix(':')) {
            // Adjacent codes: `:a::b:`.
            for code in inner.split("::") {
                if code.is_empty() || !code.chars().all(|c| c.is_ascii_alphanumeric() || "_+-".contains(c)) {
                    return None;
                }
                out.push(EmojiToken::Code(code));
            }
        } else if emojis::get(word).is_some() {
            out.push(EmojiToken::Glyph(word));
        } else {
            return None;
        }
    }
    (!out.is_empty() && out.len() <= 12).then_some(out)
}

/// Mattermost emoji names are GitHub shortcodes, with a few aliases of its own.
pub fn emoji(name: &str) -> Option<&'static str> {
    let name = match name {
        "thumbsup" => "+1",
        "thumbsdown" => "-1",
        n => n,
    };
    emojis::get_by_shortcode(name).map(|e| e.as_str())
}

pub fn time_of_day(millis: i64) -> String {
    local(millis).map(|t| t.format("%H:%M").to_string()).unwrap_or_default()
}

/// "Today", "Yesterday" or a date, for separators between days.
pub fn day_label(millis: i64, now: DateTime<Local>) -> String {
    let Some(t) = local(millis) else { return String::new() };
    let days = (now.date_naive() - t.date_naive()).num_days();
    match days {
        0 => "Today".into(),
        1 => "Yesterday".into(),
        _ => t.format("%A, %B %-d, %Y").to_string(),
    }
}

pub fn same_day(a: i64, b: i64) -> bool {
    matches!((local(a), local(b)), (Some(a), Some(b)) if a.date_naive() == b.date_naive())
}

fn local(millis: i64) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(millis).single()
}

/// Stable small hash used to give each user a consistent name color.
pub fn color_index(key: &str, n: usize) -> usize {
    let h = key.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    h as usize % n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emojify_known_and_unknown() {
        assert_eq!(emojify("hi :wave: there"), "hi 👋 there");
        assert_eq!(emojify(":thumbsup::+1:"), "👍👍");
        assert_eq!(emojify("custom :partyparrot: stays"), "custom :partyparrot: stays");
    }

    #[test]
    fn emoji_only_messages() {
        use EmojiToken::*;
        assert_eq!(emoji_only(":parrot:"), Some(vec![Code("parrot")]));
        assert_eq!(emoji_only(" :a::b:  🎉 "), Some(vec![Code("a"), Code("b"), Glyph("🎉")]));
        assert_eq!(emoji_only("hi :parrot:"), None);
        assert_eq!(emoji_only(":not code:"), None);
        assert_eq!(emoji_only(""), None);
        assert_eq!(emojify("time 10:30:00 ok"), "time 10:30:00 ok");
        assert_eq!(emojify("a: b :"), "a: b :");
        assert_eq!(emojify("::"), "::");
    }

    #[test]
    fn prepare_links_urls_and_mentions_outside_code() {
        assert_eq!(
            prepare("@alice see https://x.io/a, ok"),
            "[@alice](mention:alice) see <https://x.io/a>, ok"
        );
        assert_eq!(prepare("mail bob@example.com"), "mail bob@example.com");
        assert_eq!(prepare("hi @bob."), "hi [@bob](mention:bob).");
        assert_eq!(prepare("[docs](https://x.io)"), "[docs](https://x.io)");
        assert_eq!(prepare("`@alice :wave:` @bob"), "`@alice :wave:` [@bob](mention:bob)");
        assert_eq!(prepare("```\n@a https://x\n```\n:wave:"), "```\n@a https://x\n```\n👋");
        assert_eq!(prepare("lone @ sign"), "lone @ sign");
        assert_eq!(prepare("one\ntwo\n\nthree"), "one  \ntwo  \n\nthree", "newlines break lines");
        assert_eq!(prepare("```\na\nb\n```"), "```\na\nb\n```", "not in code");
    }

    #[test]
    fn server_url_picks_scheme() {
        assert_eq!(server_url("localhost:8065"), "http://localhost:8065");
        assert_eq!(server_url(" 127.0.0.1:8065/ "), "http://127.0.0.1:8065");
        assert_eq!(server_url("[::1]:8065"), "http://[::1]:8065");
        assert_eq!(server_url("chat.example.com"), "https://chat.example.com");
        assert_eq!(server_url("localhost.evil.com"), "https://localhost.evil.com");
        assert_eq!(server_url("http://intranet:8065/"), "http://intranet:8065");
    }

    #[test]
    fn color_index_is_stable_and_bounded() {
        assert_eq!(color_index("alice", 6), color_index("alice", 6));
        assert!(color_index("bob", 6) < 6);
    }
}
