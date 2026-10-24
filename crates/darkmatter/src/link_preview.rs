//! Link previews made by the sender's app (axiond): a message's first link
//! is fetched as it's sent, and what the page says about itself goes with
//! the message (inside it, in an encrypted conversation). So nobody who
//! reads it fetches anything, and the server never sees the link. The
//! linked site sees the sender, as if they'd opened it. `[privacy]
//! link_previews = false` turns it off.
//!
//! Mattermost makes its own previews on the server; `from_mattermost`
//! reads those into the same shape.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use dm_crypto::wire::{LinkPreview, PREVIEW_IMAGE};

/// How long fetching a preview may take before the message goes without.
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// The most of a page read for its tags.
const MAX_PAGE: usize = 512 * 1024;
/// The most of a picture read.
const MAX_IMAGE: usize = 4 * 1024 * 1024;

/// The first link in a message that gets a preview: http(s), outside code.
pub fn first_link(message: &str) -> Option<String> {
    let mut in_block = false;
    for line in message.lines() {
        if line.trim_start().starts_with("```") {
            in_block = !in_block;
            continue;
        }
        if in_block {
            continue;
        }
        // Outside `inline code`.
        for (i, part) in line.split('`').enumerate() {
            if i % 2 == 1 {
                continue;
            }
            for word in part.split_whitespace() {
                let word = word.trim_start_matches(['<', '(']).trim_end_matches(['>', ')', '.', ',', '!', '?', ';', ':', '\'', '"']);
                if (word.starts_with("https://") || word.starts_with("http://")) && word.len() > 10 {
                    return Some(word.to_string());
                }
            }
        }
    }
    None
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        // Sites give their preview tags to link-preview fetchers.
        .user_agent("Mozilla/5.0 (compatible; Darkmatter link preview; +https://github.com/karstandsons/darkmatter)")
        .timeout(TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| e.to_string())
}

/// Reads at most `max` bytes of a response.
async fn read(mut resp: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        body.extend_from_slice(&chunk);
        if body.len() >= max {
            body.truncate(max);
            break;
        }
    }
    Ok(body)
}

/// Fetches `url`'s preview: YouTube and Vimeo through their oEmbed (their
/// pages may be a cookie-consent wall), anything else from its page's tags.
pub async fn fetch(url: &str) -> Result<LinkPreview, String> {
    let http = client()?;
    let (mut preview, image) = match oembed(url) {
        Some(endpoint) => {
            let resp = http.get(&endpoint).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?;
            let json: serde_json::Value = serde_json::from_slice(&read(resp, MAX_PAGE).await?).map_err(|e| e.to_string())?;
            let s = |k: &str| json.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let preview = LinkPreview { url: url.into(), site: s("provider_name"), title: s("title"), description: s("author_name"), image: String::new() };
            (preview, Some(s("thumbnail_url")).filter(|u| !u.is_empty()))
        }
        None => {
            let resp = http.get(url).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?;
            if !mime_is_html(&resp) {
                return Err("not a web page".into());
            }
            let page = read(resp, MAX_PAGE).await?;
            from_html(url, &String::from_utf8_lossy(&page))
        }
    };
    if preview.title.is_empty() && preview.description.is_empty() {
        return Err("the page says nothing about itself".into());
    }
    if let Some(src) = image
        && let Ok(jpeg) = picture(&http, &src).await
    {
        preview.image = B64.encode(jpeg);
    }
    Ok(preview)
}

fn mime_is_html(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|t| t.contains("html"))
}

/// oEmbed endpoints for the sites whose pages don't preview well.
fn oembed(url: &str) -> Option<String> {
    let host = url.split("://").nth(1)?.split(['/', '?', '#']).next()?.trim_start_matches("www.").trim_start_matches("m.");
    let endpoint = match host {
        "youtube.com" | "youtu.be" | "music.youtube.com" => "https://www.youtube.com/oembed",
        "vimeo.com" => "https://vimeo.com/api/oembed.json",
        _ => return None,
    };
    let encoded: String = url.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    Some(format!("{endpoint}?format=json&url={encoded}"))
}

/// What a page says about itself: Open Graph tags, else its `<title>` and
/// description. Returns the preview and its picture's address.
pub fn from_html(url: &str, html: &str) -> (LinkPreview, Option<String>) {
    let head = &html[..html.find("</head>").unwrap_or(html.len())];
    let mut meta = std::collections::HashMap::new();
    for tag in head.split('<').skip(1).filter(|t| t.get(..5).is_some_and(|start| start.eq_ignore_ascii_case("meta "))) {
        let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
        let name = attr(tag, "property").or_else(|| attr(tag, "name")).map(|n| n.to_ascii_lowercase());
        if let (Some(name), Some(content)) = (name, attr(tag, "content")) {
            meta.entry(name).or_insert(content);
        }
    }
    let title_tag = head
        .to_ascii_lowercase()
        .find("<title")
        .and_then(|i| {
            let rest = &head[i..];
            let start = rest.find('>')? + 1;
            let end = rest[start..].to_ascii_lowercase().find("</title>")?;
            Some(entities(rest[start..start + end].trim()))
        })
        .unwrap_or_default();
    let get = |k: &str| meta.get(k).map(|v| entities(v)).unwrap_or_default();
    let title = Some(get("og:title")).filter(|t| !t.is_empty()).unwrap_or(title_tag);
    let description = Some(get("og:description")).filter(|t| !t.is_empty()).unwrap_or_else(|| get("description"));
    let host = url.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or_default().trim_start_matches("www.");
    let site = Some(get("og:site_name")).filter(|s| !s.is_empty()).unwrap_or_else(|| host.to_string());
    let image = Some(get("og:image:secure_url")).filter(|s| !s.is_empty()).or_else(|| Some(get("og:image")).filter(|s| !s.is_empty())).or_else(|| Some(get("twitter:image")).filter(|s| !s.is_empty()));
    let preview = LinkPreview { url: url.into(), site: clip(&site, 200), title: clip(&title, 300), description: clip(&description, 500), image: String::new() };
    (preview, image.map(|i| absolute(url, &i)))
}

fn clip(s: &str, max: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() <= max { s } else { format!("{}…", s.chars().take(max - 1).collect::<String>()) }
}

/// An attribute's value in a tag's text (`content="…"` or `'…'`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name).map(|i| i + from) {
        let before_ok = i == 0 || lower.as_bytes()[i - 1].is_ascii_whitespace();
        let rest = tag[i + name.len()..].trim_start();
        if before_ok && let Some(rest) = rest.strip_prefix('=') {
            let rest = rest.trim_start();
            let quote = rest.chars().next()?;
            if quote == '"' || quote == '\'' {
                return rest[1..].find(quote).map(|end| rest[1..1 + end].to_string());
            }
            return Some(rest.split_whitespace().next().unwrap_or_default().trim_end_matches('/').to_string());
        }
        from = i + name.len();
    }
    None
}

fn entities(s: &str) -> String {
    let mut out = s.replace("&quot;", "\"").replace("&#39;", "'").replace("&#x27;", "'").replace("&apos;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ");
    // Numeric ones (&#8217;).
    while let Some(i) = out.find("&#") {
        let Some(end) = out[i..].find(';').map(|e| e + i) else { break };
        let code = &out[i + 2..end];
        let ch = if let Some(hex) = code.strip_prefix(['x', 'X']) { u32::from_str_radix(hex, 16).ok() } else { code.parse().ok() }.and_then(char::from_u32);
        match ch {
            Some(c) => out.replace_range(i..=end, &c.to_string()),
            None => break,
        }
    }
    out.replace("&amp;", "&")
}

/// `src` as a full address, relative to `page`.
fn absolute(page: &str, src: &str) -> String {
    match reqwest::Url::parse(page).and_then(|base| base.join(src)) {
        Ok(u) => u.to_string(),
        Err(_) => src.to_string(),
    }
}

/// A preview's picture: fetched, shrunk to [`PREVIEW_IMAGE`] across, JPEG.
async fn picture(http: &reqwest::Client, src: &str) -> Result<Vec<u8>, String> {
    if !(src.starts_with("https://") || src.starts_with("http://")) {
        return Err("not a web address".into());
    }
    let resp = http.get(src).send().await.map_err(|e| e.to_string())?.error_for_status().map_err(|e| e.to_string())?;
    let bytes = read(resp, MAX_IMAGE).await?;
    tokio::task::spawn_blocking(move || {
        let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
        let img = if img.width() > PREVIEW_IMAGE { img.resize(PREVIEW_IMAGE, PREVIEW_IMAGE * 2, image::imageops::FilterType::Triangle) } else { img };
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 70).encode_image(&img.to_rgb8()).map_err(|e| e.to_string())?;
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Mattermost's own preview of a post's first link (`metadata.embeds`,
/// fetched by its server), with its picture's address.
pub fn from_mattermost(post: &mm_api::models::Post) -> Option<(LinkPreview, Option<String>)> {
    let embed = post.metadata.embeds.iter().find(|e| e.get("type").and_then(|t| t.as_str()) == Some("opengraph"))?;
    let data = embed.get("data")?;
    let s = |k: &str| data.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
    let url = embed.get("url").and_then(|u| u.as_str()).map(str::to_string).unwrap_or_else(|| s("url"));
    let image = data.get("images").and_then(|i| i.as_array()).and_then(|a| a.first()).and_then(|i| i.get("secure_url").and_then(|u| u.as_str()).filter(|u| !u.is_empty()).or_else(|| i.get("url").and_then(|u| u.as_str()))).map(str::to_string);
    let preview = LinkPreview { url, site: s("site_name"), title: s("title"), description: clip(&s("description"), 500), image: String::new() };
    (!preview.title.is_empty() || !preview.description.is_empty()).then_some((preview, image))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_link_outside_code() {
        assert_eq!(first_link("look (https://example.com/a?b=c).").as_deref(), Some("https://example.com/a?b=c"));
        assert_eq!(first_link("`https://inline.example` then https://real.example/x").as_deref(), Some("https://real.example/x"));
        assert_eq!(first_link("```\nhttps://in.block.example\n```\nnothing"), None);
        assert_eq!(first_link("ftp://no.example and http:// nothing"), None);
    }

    #[test]
    fn a_page_describes_itself() {
        let html = r#"<html><head><title>Fallback &amp; title</title>
            <meta property="og:title" content="It&#39;s a &quot;title&quot;">
            <meta name="description" content="plain description">
            <meta content="/img/card.png" property="og:image" />
            <meta property="og:site_name" content='Example Site'></head><body>…</body></html>"#;
        let (p, image) = from_html("https://www.example.com/page", html);
        assert_eq!(p.title, "It's a \"title\"");
        assert_eq!(p.description, "plain description");
        assert_eq!(p.site, "Example Site");
        assert_eq!(image.as_deref(), Some("https://www.example.com/img/card.png"));
        let (p, image) = from_html("https://www.plain.example/x", "<head><TITLE> Just a title </TITLE></head>");
        assert_eq!((p.title.as_str(), p.site.as_str(), image), ("Just a title", "plain.example", None));
    }

    #[test]
    fn youtube_and_vimeo_go_through_oembed() {
        assert!(oembed("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap().starts_with("https://www.youtube.com/oembed?format=json&url=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3DdQw4w9WgXcQ"));
        assert!(oembed("https://youtu.be/dQw4w9WgXcQ").is_some());
        assert!(oembed("https://vimeo.com/76979871").unwrap().starts_with("https://vimeo.com/api/oembed.json"));
        assert!(oembed("https://example.com/youtube.com").is_none());
    }

    /// Real sites (needs the internet): `cargo test -p darkmatter -- --ignored live`.
    #[tokio::test]
    #[ignore]
    async fn live_previews() {
        for url in ["https://www.youtube.com/watch?v=jNQXAC9IVRw", "https://www.rust-lang.org/", "https://github.com/rust-lang/rust"] {
            let p = fetch(url).await.unwrap_or_else(|e| panic!("{url}: {e}"));
            println!("{url}\n  site {:?}\n  title {:?}\n  description {:?}\n  picture {} bytes", p.site, p.title, p.description.chars().take(80).collect::<String>(), p.image.len() * 3 / 4);
            assert!(!p.title.is_empty() && !p.image.is_empty(), "{url}: {p:?}");
        }
    }

    #[test]
    fn mattermosts_own_previews_read_the_same() {
        let post: mm_api::models::Post = serde_json::from_value(serde_json::json!({
            "id": "p", "message": "see https://example.com",
            "metadata": { "embeds": [{ "type": "opengraph", "url": "https://example.com", "data": {
                "title": "Example", "description": "An example", "site_name": "Ex",
                "images": [{ "url": "http://example.com/i.png", "secure_url": "https://example.com/i.png" }] } }] }
        }))
        .unwrap();
        let (p, image) = from_mattermost(&post).unwrap();
        assert_eq!((p.title.as_str(), p.site.as_str(), p.url.as_str()), ("Example", "Ex", "https://example.com"));
        assert_eq!(image.as_deref(), Some("https://example.com/i.png"));
    }
}

/// Where a shown preview's picture comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Picture {
    /// Carried in the message (axiond), base64 JPEG.
    Inline(String),
    /// At an address (Mattermost's previews), fetched through the server.
    Remote(String),
}

/// A post's preview, wherever it came from: the sender's (`dm_preview`,
/// plain or read out of an encrypted message) or Mattermost's.
pub fn of(post: &mm_api::models::Post) -> Option<(LinkPreview, Option<Picture>)> {
    if let Some(p) = post.props.get(dm_client::PREVIEW_PROP).and_then(|p| serde_json::from_value::<LinkPreview>(p.clone()).ok()) {
        let picture = (!p.image.is_empty()).then(|| Picture::Inline(p.image.clone()));
        return Some((p, picture));
    }
    from_mattermost(post).map(|(p, image)| (p, image.map(Picture::Remote)))
}

/// The media key a post's preview picture is kept under.
pub fn key(post_id: &str, picture: &Picture) -> crate::media::Key {
    match picture {
        Picture::Inline(_) => crate::media::Key::LinkInline(post_id.to_string()),
        Picture::Remote(url) => crate::media::Key::Remote(url.clone()),
    }
}
