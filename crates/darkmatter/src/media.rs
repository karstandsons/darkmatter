//! In-memory cache of avatars and image previews, fetched once each, and
//! attachments: uploading them and fetching them, decrypted when they came
//! in an encrypted message.

use std::collections::HashMap;

use dm_crypto::wire::FileKey;

use iced::Task;
use iced::widget::image;
use mm_api::Client;

use crate::app::Message;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// User id + picture timestamp, so a changed avatar is refetched.
    Avatar(String, i64),
    Preview(String),
    /// An image at full size, for the image viewer.
    Full(String),
    /// A custom emoji's image, by emoji id.
    Emoji(String),
    /// A GIF, played from the original file (servers make no animated
    /// previews; Mattermost makes none at all for GIFs).
    Gif(String),
}

pub enum Media {
    Loading,
    /// With its size in pixels, read from the file's header, so it can be
    /// laid out without the renderer (which loads pictures in the
    /// background, and forgets the ones off screen).
    Ready(image::Handle, Option<(u32, u32)>),
    Animated(std::sync::Arc<crate::animated::Frames>),
    Failed,
}

#[derive(Default)]
pub struct Cache {
    items: HashMap<Key, Media>,
}

impl Cache {
    pub fn get(&self, key: &Key) -> Option<&image::Handle> {
        match self.items.get(key) {
            Some(Media::Ready(h, _)) => Some(h),
            Some(Media::Animated(f)) => Some(f.first()),
            _ => None,
        }
    }

    /// A picture's size in pixels, once loaded.
    pub fn size(&self, key: &Key) -> Option<(u32, u32)> {
        match self.items.get(key) {
            Some(Media::Ready(_, size)) => *size,
            Some(Media::Animated(f)) => Some(f.size()),
            _ => None,
        }
    }

    /// A GIF's frames, once decoded.
    pub fn frames(&self, key: &Key) -> Option<&crate::animated::Frames> {
        match self.items.get(key) {
            Some(Media::Animated(f)) => Some(f),
            _ => None,
        }
    }

    pub fn failed(&self, key: &Key) -> bool {
        matches!(self.items.get(key), Some(Media::Failed))
    }

    /// Start fetching every key not seen before.
    /// Encrypted files are decrypted with the keys `crypto` has from the
    /// messages it read.
    pub fn fetch(&mut self, client: &Client, crypto: &Shared, keys: impl IntoIterator<Item = Key>) -> Task<Message> {
        let mut tasks = Vec::new();
        for key in keys {
            if self.items.contains_key(&key) {
                continue;
            }
            self.items.insert(key.clone(), Media::Loading);
            let (client, crypto) = (client.clone(), crypto.clone());
            if let Key::Gif(id) = &key {
                let id = id.clone();
                tasks.push(Task::perform(
                    async move {
                        let bytes = download(&client, &crypto, &id).await;
                        // Decoding every frame takes a moment: not on the UI thread.
                        let frames = match bytes {
                            Ok(b) => tokio::task::spawn_blocking(move || crate::animated::Frames::decode(&b)).await.map_err(|e| e.to_string()).and_then(|r| r),
                            Err(e) => Err(e),
                        };
                        (key, frames.map(std::sync::Arc::new))
                    },
                    |(key, r)| Message::Animation(key, r),
                ));
                continue;
            }
            tasks.push(
                Task::perform(
                    async move {
                        let bytes = match &key {
                            Key::Avatar(id, _) => client.profile_image(id).await.map_err(|e| e.to_string()),
                            Key::Preview(id) => preview(&client, &crypto, id).await,
                            Key::Full(id) => download(&client, &crypto, id).await,
                            Key::Emoji(id) => client.emoji_image(id).await.map_err(|e| e.to_string()),
                            Key::Gif(_) => unreachable!("fetched above"),
                        };
                        (key, bytes)
                    },
                    |(key, r)| Message::Media(key, r),
                ));
        }
        Task::batch(tasks)
    }

    /// Drop the full-size images (the viewer closed).
    pub fn forget_full(&mut self) {
        self.items.retain(|k, _| !matches!(k, Key::Full(_)));
    }

    pub fn animation(&mut self, key: Key, result: Result<std::sync::Arc<crate::animated::Frames>, String>) {
        let media = match result {
            Ok(frames) => Media::Animated(frames),
            Err(e) => {
                tracing::warn!("media {key:?}: {e}");
                Media::Failed
            }
        };
        self.items.insert(key, media);
    }

    pub fn loaded(&mut self, key: Key, result: Result<Vec<u8>, String>) {
        let media = match result {
            Ok(bytes) => {
                let size = ::image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok());
                Media::Ready(image::Handle::from_bytes(bytes), size)
            }
            Err(e) => {
                tracing::warn!("media {key:?}: {e}");
                Media::Failed
            }
        };
        self.items.insert(key, media);
    }
}

/// Fit an image of `w`x`h` into the inline preview box, keeping its aspect ratio.
pub fn preview_size(w: i64, h: i64) -> (f32, f32) {
    const MAX_W: f32 = 400.0;
    const MAX_H: f32 = 300.0;
    if w <= 0 || h <= 0 {
        return (MAX_W, MAX_H * 0.6);
    }
    let (w, h) = (w as f32, h as f32);
    let scale = (MAX_W / w).min(MAX_H / h).min(1.0);
    (w * scale, h * scale)
}

/// "12 KB", "3.4 MB".
pub fn human_size(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    match b {
        b if b < 1024.0 => format!("{b} B"),
        b if b < 1024.0 * 1024.0 => format!("{:.0} KB", b / 1024.0),
        b if b < 1024.0 * 1024.0 * 1024.0 => format!("{:.1} MB", b / (1024.0 * 1024.0)),
        b => format!("{:.1} GB", b / (1024.0 * 1024.0 * 1024.0)),
    }
}

/// A file attached to the composer, uploaded as soon as it's added.
pub struct Pending {
    pub local_id: u64,
    /// The composer it's for: its draft key (the channel, or `thread:<root>`).
    pub key: String,
    pub name: String,
    pub state: Upload,
}

pub enum Upload {
    Uploading,
    Done(Uploaded),
    Failed(String),
}

/// An attachment on the server.
#[derive(Clone, Debug)]
pub struct Uploaded {
    pub id: String,
    /// In an encrypted channel: its key (and its thumbnail's), for the
    /// message to carry.
    pub e2ee: Option<FileKey>,
}

impl Uploaded {
    /// Every upload the message must attach.
    pub fn ids(&self) -> Vec<String> {
        match &self.e2ee {
            Some(k) => k.ids().map(String::from).collect(),
            None => vec![self.id.clone()],
        }
    }
}

/// Read a local file and upload it to a channel, encrypted if the channel
/// is.
pub async fn upload(client: Client, channel_id: String, path: std::path::PathBuf, encrypted: bool) -> Result<Uploaded, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
    if encrypted {
        let file = tokio::task::spawn_blocking(move || outgoing(name, bytes)).await.map_err(|e| e.to_string())?;
        let key = dm_client::files::upload(&client, &channel_id, file).await?;
        return Ok(Uploaded { id: key.id.clone(), e2ee: Some(key) });
    }
    let result = client.upload_file(&channel_id, &name, bytes).await.map_err(|e| e.to_string())?;
    let id = result.file_infos.into_iter().next().map(|f| f.id).ok_or("server returned no file")?;
    Ok(Uploaded { id, e2ee: None })
}

/// Thumbnails made here for encrypted pictures fit in this (the inline
/// preview is 400x300, so this is sharp at twice that).
const THUMB: (u32, u32) = (800, 600);

/// A file to encrypt: for a picture, its size and a JPEG thumbnail, since
/// the server can't make one.
fn outgoing(name: String, bytes: Vec<u8>) -> dm_client::files::Outgoing {
    let mut file = dm_client::files::Outgoing { mime: mime_for(&name).into(), name, bytes, width: 0, height: 0, thumb: None };
    let Ok(reader) = ::image::ImageReader::new(std::io::Cursor::new(&file.bytes)).with_guessed_format() else { return file };
    let mime = match reader.format() {
        Some(::image::ImageFormat::Png) => "image/png",
        Some(::image::ImageFormat::Jpeg) => "image/jpeg",
        Some(::image::ImageFormat::Gif) => "image/gif",
        Some(::image::ImageFormat::WebP) => "image/webp",
        _ => return file,
    };
    let mut reader = reader;
    let mut limits = ::image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let Ok(img) = reader.decode() else { return file };
    file.mime = mime.into();
    (file.width, file.height) = (img.width(), img.height());
    let small = img.thumbnail(THUMB.0, THUMB.1).to_rgb8();
    let mut jpeg = Vec::new();
    let encoder = ::image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 85);
    if small.write_with_encoder(encoder).is_ok() {
        file.thumb = Some(dm_client::files::Thumb { mime: "image/jpeg".into(), width: small.width(), height: small.height(), bytes: jpeg });
    }
    file
}

/// A file's type from its name, for what the server would have said.
fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
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

type Shared = crate::crypto::Shared;

/// An encrypted file's key, from the messages this account has read.
fn key_of(crypto: &Shared, id: &str) -> Option<FileKey> {
    crypto.as_ref()?.file_key(id)
}

/// An attachment's bytes, decrypted if it's an encrypted one.
pub async fn download(client: &Client, crypto: &Shared, id: &str) -> Result<Vec<u8>, String> {
    match key_of(crypto, id) {
        Some(key) => dm_client::files::download(client, &key).await,
        None => client.file(id).await.map_err(|e| e.to_string()),
    }
}

/// A picture's preview: the server's, or the sender's encrypted thumbnail.
async fn preview(client: &Client, crypto: &Shared, id: &str) -> Result<Vec<u8>, String> {
    match key_of(crypto, id) {
        Some(key) => match &key.thumb {
            Some(thumb) => dm_client::files::download(client, thumb).await,
            None => dm_client::files::download(client, &key).await,
        },
        None => client.file_preview(id).await.map_err(|e| e.to_string()),
    }
}

/// What a message shows for an encrypted file: what the sender said about
/// it, not the server's anonymous blob.
pub fn file_info(key: &FileKey, post: &mm_api::models::Post) -> mm_api::models::FileInfo {
    mm_api::models::FileInfo {
        id: key.id.clone(),
        user_id: post.user_id.clone(),
        post_id: post.id.clone(),
        channel_id: post.channel_id.clone(),
        create_at: post.create_at,
        name: key.name.clone(),
        extension: key.name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default(),
        size: key.size as i64,
        mime_type: key.mime.clone(),
        width: i64::from(key.width),
        height: i64::from(key.height),
        has_preview_image: key.thumb.is_some(),
        mini_preview: None,
    }
}

/// Download an attachment into the cache dir and open it with the system's default app.
pub async fn open_file(client: Client, crypto: Shared, file_id: String, name: String) -> Result<(), String> {
    // Only the final path component, so a crafted name can't escape the directory.
    let name = std::path::Path::new(&name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| file_id.clone());
    let dir = dirs::cache_dir().ok_or("no cache dir")?.join("darkmatter/files").join(&file_id);
    let path = dir.join(name);
    if !path.exists() {
        let bytes = download(&client, &crypto, &file_id).await?;
        tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
        // Another app has to open it, so a decrypted file is written out
        // here, readable only by you.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await;
        }
        tokio::fs::write(&path, bytes).await.map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await;
        }
    }
    open::that_detached(&path).map_err(|e| e.to_string())?;
    Ok(())
}

/// The clipboard's image, saved as a PNG to attach, when the clipboard holds
/// an image and no text (text pastes into the message box as usual).
pub async fn paste_image() -> Result<Option<std::path::PathBuf>, String> {
    tokio::task::spawn_blocking(|| {
        let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        if clipboard.get_text().is_ok_and(|t| !t.trim().is_empty()) {
            return Ok(None);
        }
        let img = match clipboard.get_image() {
            Ok(img) => img,
            Err(arboard::Error::ContentNotAvailable) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let rgba = ::image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned()).ok_or("the clipboard image is malformed")?;
        let dir = dirs::cache_dir().ok_or("no cache directory")?.join("darkmatter").join("pasted");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("pasted-{}.png", chrono::Local::now().format("%Y%m%d-%H%M%S")));
        rgba.save_with_format(&path, ::image::ImageFormat::Png).map_err(|e| e.to_string())?;
        Ok(Some(path))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_keeps_aspect_and_never_upscales() {
        assert_eq!(preview_size(800, 600), (400.0, 300.0));
        assert_eq!(preview_size(1200, 300), (400.0, 100.0));
        assert_eq!(preview_size(100, 50), (100.0, 50.0));
        assert_eq!(preview_size(0, 0), (400.0, 180.0));
    }

    #[tokio::test]
    async fn upload_posts_multipart_and_returns_file_id() {
        use wiremock::matchers::{body_string_contains, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v4/files"))
            .and(header("Authorization", "Bearer t"))
            .and(body_string_contains("name=\"channel_id\""))
            .and(body_string_contains("c1"))
            .and(body_string_contains("filename=\"pic.png\""))
            .and(body_string_contains("PNGDATA"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({ "file_infos": [{ "id": "f9", "name": "pic.png" }] })),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pic.png");
        std::fs::write(&file, "PNGDATA").unwrap();
        let client = Client::new(&server.uri()).unwrap().with_token("t");
        assert_eq!(upload(client, "c1".into(), file, false).await.unwrap().id, "f9");

        let missing = upload(Client::new(&server.uri()).unwrap(), "c1".into(), dir.path().join("nope"), false).await;
        assert!(missing.is_err());
    }

    #[test]
    fn encrypted_pictures_get_a_thumbnail_made_here() {
        let mut png = Vec::new();
        ::image::RgbaImage::from_pixel(1600, 900, ::image::Rgba([10, 20, 30, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), ::image::ImageFormat::Png)
            .unwrap();
        // The name says nothing: the bytes decide.
        let f = outgoing("screenshot".into(), png);
        assert_eq!((f.mime.as_str(), f.width, f.height), ("image/png", 1600, 900));
        let thumb = f.thumb.unwrap();
        assert_eq!((thumb.mime.as_str(), thumb.width, thumb.height), ("image/jpeg", 800, 450));
        assert!(::image::load_from_memory(&thumb.bytes).is_ok());

        let f = outgoing("notes.txt".into(), b"not a picture".to_vec());
        assert_eq!((f.mime.as_str(), f.width, f.thumb.is_none()), ("text/plain", 0, true));
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(12 * 1024), "12 KB");
        assert_eq!(human_size(3_565_158), "3.4 MB");
    }
}
