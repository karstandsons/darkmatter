//! Custom emoji and profile pictures.
//!
//! Emoji are kept as uploaded (so GIFs stay animated) once they've been
//! checked to be a small picture. Profile pictures are redrawn as a 256-pixel
//! square PNG, which also drops anything hidden in the original (such as
//! where a photo was taken).

use std::path::PathBuf;

use axum::Json;
use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use mm_api::models::{CustomEmoji, User};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::Me;
use crate::db;
use crate::error::{Error, Result};
use crate::hub::Event;
use crate::{App, ids};

const MAX_EMOJI_BYTES: usize = 1024 * 1024;
const MAX_EMOJI_SIDE: u32 = 1024;
const MAX_PICTURE_BYTES: usize = 10 * 1024 * 1024;
const PICTURE_SIDE: u32 = 256;

/// Where an emoji is kept. Only well-formed ids get a path, so nothing like
/// `..%2F` can point outside the data folder.
fn emoji_path(app: &App, id: &str) -> Result<PathBuf> {
    if !ids::is_id(id) {
        return Err(Error::not_found("Emoji"));
    }
    Ok(app.config.data_dir.join("emoji").join(id))
}

/// Where someone's picture is kept, for well-formed ids only.
fn picture_path(app: &App, user_id: &str) -> Result<PathBuf> {
    if !ids::is_id(user_id) {
        return Err(Error::not_found("Picture"));
    }
    Ok(app.config.data_dir.join("pictures").join(format!("{user_id}.png")))
}

/// The picture's format, if it's one we accept and its size is sane.
fn check_picture(bytes: &[u8], max_side: u32) -> Option<image::ImageFormat> {
    let format = image::guess_format(bytes).ok()?;
    if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg | image::ImageFormat::Gif | image::ImageFormat::WebP) {
        return None;
    }
    let (w, h) = image::ImageReader::with_format(std::io::Cursor::new(bytes), format).into_dimensions().ok()?;
    (w > 0 && h > 0 && w <= max_side && h <= max_side).then_some(format)
}

fn mime(format: image::ImageFormat) -> &'static str {
    match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        _ => "image/webp",
    }
}

/// Emoji names: lowercase letters, digits, `-`, `_` and `+`.
pub fn check_emoji_name(name: &str) -> Result<()> {
    let ok = (1..=64).contains(&name.len()) && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_+-".contains(c));
    if ok { Ok(()) } else { Err(Error::bad_request("Emoji names use lowercase letters, digits, -, _ and +, up to 64.")) }
}

#[derive(sqlx::FromRow)]
struct EmojiRow {
    id: String,
    name: String,
    creator_id: String,
    delete_at: i64,
}

impl From<EmojiRow> for CustomEmoji {
    fn from(r: EmojiRow) -> Self {
        CustomEmoji { id: r.id, name: r.name, creator_id: r.creator_id, delete_at: r.delete_at }
    }
}

/// Adds an emoji; used by the API and by `axiond emoji add`.
pub async fn add_emoji(app: &App, creator_id: &str, name: &str, bytes: &[u8]) -> Result<CustomEmoji> {
    check_emoji_name(name)?;
    if bytes.len() > MAX_EMOJI_BYTES || check_picture(bytes, MAX_EMOJI_SIDE).is_none() {
        return Err(Error::bad_request("Emoji must be a PNG, JPEG, GIF or WebP picture, at most 1 MB and 1024 pixels a side."));
    }
    let taken: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM emoji WHERE name = ? AND delete_at = 0").bind(name).fetch_one(&app.db).await?;
    if taken > 0 {
        return Err(Error::bad_request(format!(":{name}: already exists.")));
    }
    let id = ids::new_id();
    let path = emoji_path(app, &id)?;
    write_file(&path, bytes).await?;
    let now = ids::now();
    sqlx::query("INSERT INTO emoji (id, creator_id, name, create_at, update_at) VALUES (?1, ?2, ?3, ?4, ?4)")
        .bind(&id)
        .bind(creator_id)
        .bind(name)
        .bind(now)
        .execute(&app.db)
        .await?;
    Ok(CustomEmoji { id, name: name.into(), creator_id: creator_id.into(), delete_at: 0 })
}

async fn write_file(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().ok_or_else(Error::internal)?;
    tokio::fs::create_dir_all(dir).await.map_err(io_error)?;
    // Write then rename, so a crash never leaves half a picture.
    let tmp = path.with_extension("part");
    tokio::fs::write(&tmp, bytes).await.map_err(io_error)?;
    tokio::fs::rename(&tmp, path).await.map_err(io_error)
}

fn io_error(e: std::io::Error) -> Error {
    tracing::error!("images: {e}");
    Error::internal()
}

/// Reads the named part of a form, up to `max` bytes.
async fn read_part(form: &mut Multipart, wanted: &str, max: usize, text: &mut Option<String>) -> Result<Vec<u8>> {
    let mut found = None;
    while let Some(field) = form.next_field().await.map_err(|_| Error::bad_request("Couldn't read the upload."))? {
        match field.name() {
            Some(name) if name == wanted => {
                let bytes = field.bytes().await.map_err(|_| Error::bad_request("The picture is too big."))?;
                if bytes.len() > max {
                    return Err(Error::bad_request("The picture is too big."));
                }
                found = Some(bytes.to_vec());
            }
            Some("emoji") => *text = Some(field.text().await.map_err(|_| Error::bad_request("Couldn't read the upload."))?),
            _ => {}
        }
    }
    found.ok_or_else(|| Error::bad_request("There was no picture in the upload."))
}

#[derive(Deserialize)]
struct EmojiForm {
    name: String,
}

/// `POST emoji`: multipart with `image` and `emoji` (`{"name": …}`).
pub async fn create(State(app): State<App>, me: Me, mut form: Multipart) -> Result<(StatusCode, Json<CustomEmoji>)> {
    let mut meta = None;
    let bytes = read_part(&mut form, "image", MAX_EMOJI_BYTES, &mut meta).await?;
    let meta: EmojiForm = serde_json::from_str(meta.as_deref().unwrap_or("{}")).map_err(|_| Error::bad_request("Name the emoji."))?;
    Ok((StatusCode::CREATED, Json(add_emoji(&app, &me.user_id, meta.name.trim(), &bytes).await?)))
}

#[derive(Deserialize)]
pub struct EmojiPage {
    page: Option<i64>,
    per_page: Option<i64>,
}

pub async fn list(State(app): State<App>, _: Me, Query(q): Query<EmojiPage>) -> Result<Json<Vec<CustomEmoji>>> {
    let per_page = q.per_page.unwrap_or(60).clamp(1, 200);
    let rows: Vec<EmojiRow> =
        sqlx::query_as("SELECT id, name, creator_id, delete_at FROM emoji WHERE delete_at = 0 ORDER BY name LIMIT ?1 OFFSET ?2")
            .bind(per_page)
            .bind(q.page.unwrap_or(0).max(0) * per_page)
            .fetch_all(&app.db)
            .await?;
    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

pub async fn by_name(State(app): State<App>, _: Me, Path(name): Path<String>) -> Result<Json<CustomEmoji>> {
    let row: Option<EmojiRow> = sqlx::query_as("SELECT id, name, creator_id, delete_at FROM emoji WHERE name = ? AND delete_at = 0")
        .bind(&name)
        .fetch_optional(&app.db)
        .await?;
    Ok(Json(row.ok_or_else(|| Error::not_found("Emoji"))?.into()))
}

pub async fn emoji_image(State(app): State<App>, _: Me, Path(id): Path<String>) -> Result<Response> {
    let path = emoji_path(&app, &id)?;
    let live: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM emoji WHERE id = ? AND delete_at = 0").bind(&id).fetch_one(&app.db).await?;
    if live == 0 {
        return Err(Error::not_found("Emoji"));
    }
    picture_response(tokio::fs::read(path).await.map_err(|_| Error::not_found("Emoji"))?)
}

pub async fn delete(State(app): State<App>, me: Me, Path(id): Path<String>) -> Result<Json<Value>> {
    let path = emoji_path(&app, &id)?;
    let creator: Option<String> = sqlx::query_scalar("SELECT creator_id FROM emoji WHERE id = ? AND delete_at = 0").bind(&id).fetch_optional(&app.db).await?;
    let creator = creator.ok_or_else(|| Error::not_found("Emoji"))?;
    if creator != me.user_id && !me.is_admin() {
        return Err(Error::forbidden());
    }
    sqlx::query("UPDATE emoji SET delete_at = ?1, update_at = ?1 WHERE id = ?2").bind(ids::now()).bind(&id).execute(&app.db).await?;
    let _ = tokio::fs::remove_file(path).await;
    super::ok()
}

/// A stored picture, inline but never run as anything else.
fn picture_response(bytes: Vec<u8>) -> Result<Response> {
    let format = image::guess_format(&bytes).map_err(|_| Error::internal())?;
    let mut response = Response::new(Body::from(bytes));
    let h = response.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime(format)));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; sandbox"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=86400"));
    Ok(response)
}

/// Sets someone's picture; used by the API and by `axiond user picture`.
pub async fn set_picture(app: &App, user_id: &str, bytes: Vec<u8>) -> Result<()> {
    if bytes.len() > MAX_PICTURE_BYTES || check_picture(&bytes, 16_384).is_none() {
        return Err(Error::bad_request("Pictures must be PNG, JPEG, GIF or WebP, at most 10 MB."));
    }
    let path = picture_path(app, user_id)?;
    let png = tokio::task::spawn_blocking(move || square_png(&bytes)).await.map_err(|_| Error::internal())?;
    let png = png.ok_or_else(|| Error::bad_request("Couldn't read that picture."))?;
    write_file(&path, &png).await?;
    picture_changed(app, user_id).await
}

/// The middle square of a picture, at 256 pixels, as PNG.
fn square_png(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let side = img.width().min(img.height());
    let square = img.crop_imm((img.width() - side) / 2, (img.height() - side) / 2, side, side);
    let small = square.resize_exact(PICTURE_SIDE, PICTURE_SIDE, image::imageops::FilterType::Lanczos3);
    let mut out = std::io::Cursor::new(Vec::new());
    small.to_rgba8().write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// Bumps `last_picture_update` so apps refetch, and tells everyone connected.
async fn picture_changed(app: &App, user_id: &str) -> Result<()> {
    sqlx::query("UPDATE users SET last_picture_update = ?1, update_at = ?1 WHERE id = ?2").bind(ids::now()).bind(user_id).execute(&app.db).await?;
    let user: User = db::user(&app.db, user_id, "").await?;
    let everyone: Vec<String> = sqlx::query_scalar("SELECT id FROM users WHERE delete_at = 0").fetch_all(&app.db).await?;
    app.hub.send(&everyone, &Event::new("user_updated", json!({ "user": user })));
    Ok(())
}

fn may_change(me: &Me, user_id: &str) -> Result<()> {
    if me.user_id == user_id || me.is_admin() { Ok(()) } else { Err(Error::forbidden()) }
}

/// `POST users/{id}/image`: multipart with `image`.
pub async fn upload_picture(State(app): State<App>, me: Me, Path(user_id): Path<String>, mut form: Multipart) -> Result<Json<Value>> {
    may_change(&me, &user_id)?;
    let bytes = read_part(&mut form, "image", MAX_PICTURE_BYTES, &mut None).await?;
    set_picture(&app, &user_id, bytes).await?;
    super::ok()
}

pub async fn remove_picture(State(app): State<App>, me: Me, Path(user_id): Path<String>) -> Result<Json<Value>> {
    may_change(&me, &user_id)?;
    let _ = tokio::fs::remove_file(picture_path(&app, &user_id)?).await;
    picture_changed(&app, &user_id).await?;
    super::ok()
}

/// Someone's picture, or 404 so apps draw their initials.
pub async fn picture(State(app): State<App>, _: Me, Path(user_id): Path<String>) -> Result<Response> {
    let bytes = tokio::fs::read(picture_path(&app, &user_id)?).await.map_err(|_| Error::not_found("Picture"))?;
    picture_response(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_small_real_pictures() {
        let mut png = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(4, 2).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let png = png.into_inner();
        assert_eq!(check_picture(&png, 16), Some(image::ImageFormat::Png));
        assert_eq!(check_picture(&png, 3), None);
        assert_eq!(check_picture(b"<svg onload=alert(1)>", 16), None);
        assert_eq!(check_picture(b"GIF89a but not really", 16), None);
        let square = image::load_from_memory(&square_png(&png).unwrap()).unwrap();
        assert_eq!((square.width(), square.height()), (PICTURE_SIDE, PICTURE_SIDE));
    }
}
