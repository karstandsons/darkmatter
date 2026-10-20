//! Uploads and downloads.
//!
//! Files are stored as `files/<ab>/<id>` in the data folder, under random
//! ids, and always served as downloads that a browser won't run. Pictures in
//! channels that aren't encrypted get a thumbnail and a preview; encrypted
//! files are opaque to the server, and their apps make their own.

use std::path::{Path as FsPath, PathBuf};

use axum::Json;
use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use mm_api::models::{FileInfo, UploadResult};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;

use super::channels::readable;
use crate::auth::Me;
use crate::db;
use crate::error::{Error, Result};
use crate::{App, ids};

/// Files one upload can carry, and one post (Mattermost's limit).
pub const MAX_PER_POST: usize = 10;
const THUMB: (u32, u32) = (120, 100);
const PREVIEW_WIDTH: u32 = 1920;

#[derive(Clone, Copy)]
enum Kind {
    Original,
    Thumbnail,
    Preview,
}

fn file_path(app: &App, id: &str, kind: Kind) -> PathBuf {
    let suffix = match kind {
        Kind::Original => "",
        Kind::Thumbnail => "_thumb.jpg",
        Kind::Preview => "_preview.jpg",
    };
    app.config.data_dir.join("files").join(&id[..2]).join(format!("{id}{suffix}"))
}

#[derive(sqlx::FromRow)]
pub struct FileRow {
    id: String,
    user_id: String,
    channel_id: String,
    post_id: String,
    create_at: i64,
    delete_at: i64,
    name: String,
    extension: String,
    size: i64,
    mime_type: String,
    width: i64,
    height: i64,
    has_preview_image: bool,
}

const FILE_COLUMNS: &str =
    "id, user_id, channel_id, post_id, create_at, delete_at, name, extension, size, mime_type, width, height, has_preview_image";

impl From<FileRow> for FileInfo {
    fn from(r: FileRow) -> Self {
        FileInfo {
            id: r.id,
            user_id: r.user_id,
            post_id: r.post_id,
            channel_id: r.channel_id,
            create_at: r.create_at,
            name: r.name,
            extension: r.extension,
            size: r.size,
            mime_type: r.mime_type,
            width: r.width,
            height: r.height,
            has_preview_image: r.has_preview_image,
            mini_preview: None,
        }
    }
}

pub async fn file_row(app: &App, id: &str) -> Result<Option<FileRow>> {
    let sql = format!("SELECT {FILE_COLUMNS} FROM files WHERE id = ?");
    Ok(sqlx::query_as(&sql).bind(id).fetch_optional(&app.db).await?)
}

/// The files of these posts, by post.
pub async fn of_posts(db: &db::Db, post_ids: &str) -> sqlx::Result<Vec<FileInfo>> {
    let sql = format!(
        "SELECT {FILE_COLUMNS} FROM files WHERE post_id IN (SELECT value FROM json_each(?1)) AND delete_at = 0 ORDER BY create_at, id"
    );
    let rows: Vec<FileRow> = sqlx::query_as(&sql).bind(post_ids).fetch_all(db).await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Checks that `ids` are this user's uploads to this channel, not yet in a post.
pub async fn check_unattached(app: &App, user_id: &str, channel_id: &str, ids: &[String]) -> Result<()> {
    if ids.len() > MAX_PER_POST {
        return Err(Error::bad_request(format!("A message can have at most {MAX_PER_POST} files.")));
    }
    for id in ids {
        let ok = file_row(app, id)
            .await?
            .is_some_and(|f| f.user_id == user_id && f.channel_id == channel_id && f.post_id.is_empty() && f.delete_at == 0);
        if !ok {
            return Err(Error::bad_request("One of the files isn't yours to attach here."));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct UploadQuery {
    channel_id: Option<String>,
}

/// `POST files` (multipart): a `channel_id` field (or query parameter) first,
/// then one or more `files`.
pub async fn upload(State(app): State<App>, me: Me, Query(q): Query<UploadQuery>, mut form: Multipart) -> Result<(StatusCode, Json<UploadResult>)> {
    let mut channel = None;
    if let Some(id) = q.channel_id {
        channel = Some(upload_channel(&app, &me, &id).await?);
    }
    let mut result = UploadResult::default();
    while let Some(mut field) = form.next_field().await.map_err(|e| Error::bad_request(format!("Couldn't read the upload: {e}")))? {
        match field.name() {
            Some("channel_id") => {
                let id = field.text().await.map_err(|_| Error::bad_request("Couldn't read the upload."))?;
                channel = Some(upload_channel(&app, &me, id.trim()).await?);
            }
            Some("client_ids") => {
                result.client_ids.push(field.text().await.map_err(|_| Error::bad_request("Couldn't read the upload."))?);
            }
            Some("files") => {
                let Some(channel) = &channel else { return Err(Error::bad_request("Say which channel before the files.")) };
                if result.file_infos.len() >= MAX_PER_POST {
                    return Err(Error::bad_request(format!("At most {MAX_PER_POST} files at once.")));
                }
                let name = clean_name(field.file_name().unwrap_or("file"));
                let id = ids::new_id();
                let path = file_path(&app, &id, Kind::Original);
                let size = save(&app, &path, &mut field).await?;
                let info = describe(&app, channel, &me.user_id, id, name, size, &path).await;
                match info {
                    Ok(info) => result.file_infos.push(info),
                    Err(e) => {
                        let _ = tokio::fs::remove_file(&path).await;
                        return Err(e);
                    }
                }
            }
            _ => {}
        }
    }
    if result.file_infos.is_empty() {
        return Err(Error::bad_request("There was no file in the upload."));
    }
    Ok((StatusCode::CREATED, Json(result)))
}

async fn upload_channel(app: &App, me: &Me, id: &str) -> Result<db::ChannelRow> {
    let channel = db::channel(&app.db, id).await?.ok_or_else(|| Error::not_found("Channel"))?;
    if db::member(&app.db, &channel.id, &me.user_id).await?.is_none() {
        return Err(Error::forbidden());
    }
    Ok(channel)
}

/// Streams one part to disk, stopping at the size limit.
async fn save(app: &App, path: &FsPath, field: &mut axum::extract::multipart::Field<'_>) -> Result<i64> {
    let limit = app.config.max_file_mb.saturating_mul(1024 * 1024);
    let dir = path.parent().ok_or_else(Error::internal)?;
    tokio::fs::create_dir_all(dir).await.map_err(io_error)?;
    let mut out = tokio::fs::File::create(path).await.map_err(io_error)?;
    let mut size: u64 = 0;
    loop {
        let chunk = match field.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => {
                drop(out);
                let _ = tokio::fs::remove_file(path).await;
                return Err(too_big(app));
            }
        };
        size += chunk.len() as u64;
        if size > limit {
            drop(out);
            let _ = tokio::fs::remove_file(path).await;
            return Err(too_big(app));
        }
        out.write_all(&chunk).await.map_err(io_error)?;
    }
    out.sync_all().await.map_err(io_error)?;
    Ok(size as i64)
}

fn too_big(app: &App) -> Error {
    Error {
        status: StatusCode::PAYLOAD_TOO_LARGE,
        id: "api.file.upload_file.too_large_detailed.app_error",
        message: format!("Files can be at most {} MB.", app.config.max_file_mb),
    }
}

fn io_error(e: std::io::Error) -> Error {
    tracing::error!("files: {e}");
    Error::internal()
}

/// Records the upload, with a thumbnail and preview if it's a picture in a
/// channel that isn't encrypted.
async fn describe(app: &App, channel: &db::ChannelRow, user_id: &str, id: String, name: String, size: i64, path: &FsPath) -> Result<FileInfo> {
    let extension = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).filter(|e| e.len() <= 10).unwrap_or_default();
    let mut mime_type = mime_for(&extension).to_string();
    let (mut width, mut height, mut has_preview) = (0, 0, false);
    if !channel.encrypted && size > 0 {
        let (src, thumb, preview) = (path.to_owned(), file_path(app, &id, Kind::Thumbnail), file_path(app, &id, Kind::Preview));
        let made = tokio::task::spawn_blocking(move || pictures(&src, &thumb, &preview)).await.map_err(|_| Error::internal())?;
        if let Some((w, h, mime)) = made {
            (width, height, has_preview, mime_type) = (w as i64, h as i64, true, mime.to_string());
        }
    }
    let now = ids::now();
    sqlx::query(
        "INSERT INTO files (id, user_id, channel_id, create_at, update_at, name, extension, size, mime_type, width, height, has_preview_image) \
         VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(&channel.id)
    .bind(now)
    .bind(&name)
    .bind(&extension)
    .bind(size)
    .bind(&mime_type)
    .bind(width)
    .bind(height)
    .bind(has_preview)
    .execute(&app.db)
    .await?;
    Ok(file_row(app, &id).await?.ok_or_else(Error::internal)?.into())
}

/// For a picture: its size and type, after writing a thumbnail and a preview.
/// `None` for anything that isn't a picture we can read safely.
fn pictures(src: &FsPath, thumb: &FsPath, preview: &FsPath) -> Option<(u32, u32, &'static str)> {
    let mut reader = image::ImageReader::open(src).ok()?.with_guessed_format().ok()?;
    let format = reader.format()?;
    let mime = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        _ => return None,
    };
    // Refuse decompression bombs: a small file claiming a huge picture.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let (w, h) = (img.width(), img.height());
    let write = |img: image::DynamicImage, to: &FsPath| img.to_rgb8().save_with_format(to, image::ImageFormat::Jpeg).ok();
    write(img.thumbnail(THUMB.0, THUMB.1), thumb)?;
    let preview_img = if w > PREVIEW_WIDTH { img.resize(PREVIEW_WIDTH, u32::MAX, image::imageops::FilterType::Triangle) } else { img };
    write(preview_img, preview)?;
    Some((w, h, mime))
}

/// A file name that's safe to show and to send back: no folders, no control
/// characters, not too long.
fn clean_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let name: String = base.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(255).collect();
    if name.is_empty() || name == "." || name == ".." { "file".into() } else { name }
}

fn mime_for(extension: &str) -> &'static str {
    match extension {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "log" | "md" => "text/plain",
        "zip" => "application/zip",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

/// The file, if `me` can see its channel.
async fn visible(app: &App, me: &Me, id: &str) -> Result<FileRow> {
    let file = file_row(app, id).await?.filter(|f| f.delete_at == 0).ok_or_else(|| Error::not_found("File"))?;
    if file.post_id.is_empty() {
        // Not posted yet: only for whoever uploaded it.
        if file.user_id != me.user_id {
            return Err(Error::not_found("File"));
        }
    } else {
        readable(app, me, &file.channel_id).await?;
    }
    Ok(file)
}

pub async fn download(State(app): State<App>, me: Me, Path(id): Path<String>) -> Result<Response> {
    let file = visible(&app, &me, &id).await?;
    serve(&file_path(&app, &file.id, Kind::Original), &file.mime_type, &file.name).await
}

pub async fn thumbnail(State(app): State<App>, me: Me, Path(id): Path<String>) -> Result<Response> {
    let file = visible(&app, &me, &id).await?;
    if !file.has_preview_image {
        return Err(Error::not_found("Thumbnail"));
    }
    serve(&file_path(&app, &file.id, Kind::Thumbnail), "image/jpeg", &file.name).await
}

pub async fn preview(State(app): State<App>, me: Me, Path(id): Path<String>) -> Result<Response> {
    let file = visible(&app, &me, &id).await?;
    if !file.has_preview_image {
        return Err(Error::not_found("Preview"));
    }
    serve(&file_path(&app, &file.id, Kind::Preview), "image/jpeg", &file.name).await
}

pub async fn info(State(app): State<App>, me: Me, Path(id): Path<String>) -> Result<Json<FileInfo>> {
    Ok(Json(visible(&app, &me, &id).await?.into()))
}

/// Streams a stored file as a download no browser will run.
async fn serve(path: &FsPath, mime: &str, name: &str) -> Result<Response> {
    let file = tokio::fs::File::open(path).await.map_err(|e| {
        tracing::error!("files: {}: {e}", path.display());
        Error::not_found("File")
    })?;
    let len = file.metadata().await.map_err(io_error)?.len();
    let mut headers = HeaderMap::new();
    // Pictures keep their type so apps can show them; anything else is bytes.
    let mime = if mime.starts_with("image/") && mime != "image/svg+xml" { mime } else { "application/octet-stream" };
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(mime).map_err(|_| Error::internal())?);
    headers.insert(header::CONTENT_LENGTH, len.into());
    let encoded = percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC);
    let disposition = format!("attachment; filename*=UTF-8''{encoded}");
    headers.insert(header::CONTENT_DISPOSITION, HeaderValue::from_str(&disposition).map_err(|_| Error::internal())?);
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; sandbox"));
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(file));
    let mut response = Response::new(body);
    *response.headers_mut() = headers;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_lose_folders_and_control_characters() {
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name("C:\\Users\\me\\cat.png"), "cat.png");
        assert_eq!(clean_name("a\nb\u{7}.txt"), "ab.txt");
        assert_eq!(clean_name(".."), "file");
        assert_eq!(clean_name(""), "file");
        assert_eq!(clean_name(&"x".repeat(300)).len(), 255);
    }
}
