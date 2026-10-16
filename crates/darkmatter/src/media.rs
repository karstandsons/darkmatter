//! In-memory cache of avatars and image previews, fetched once each.

use std::collections::HashMap;

use iced::Task;
use iced::widget::image;
use mm_api::Client;

use crate::app::Message;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// User id + picture timestamp, so a changed avatar is refetched.
    Avatar(String, i64),
    Preview(String),
    /// A custom emoji's image, by emoji id.
    Emoji(String),
}

pub enum Media {
    Loading,
    Ready(image::Handle),
    Failed,
}

#[derive(Default)]
pub struct Cache {
    items: HashMap<Key, Media>,
}

impl Cache {
    pub fn get(&self, key: &Key) -> Option<&image::Handle> {
        match self.items.get(key) {
            Some(Media::Ready(h)) => Some(h),
            _ => None,
        }
    }

    pub fn failed(&self, key: &Key) -> bool {
        matches!(self.items.get(key), Some(Media::Failed))
    }

    /// Start fetching every key not seen before.
    pub fn fetch(&mut self, client: &Client, keys: impl IntoIterator<Item = Key>) -> Task<Message> {
        let mut tasks = Vec::new();
        for key in keys {
            if self.items.contains_key(&key) {
                continue;
            }
            self.items.insert(key.clone(), Media::Loading);
            let client = client.clone();
            tasks.push(
                Task::perform(
                    async move {
                        let bytes = match &key {
                            Key::Avatar(id, _) => client.profile_image(id).await,
                            Key::Preview(id) => client.file_preview(id).await,
                            Key::Emoji(id) => client.emoji_image(id).await,
                        };
                        (key, bytes.map_err(|e| e.to_string()))
                    },
                    |(key, r)| Message::Media(key, r),
                ));
        }
        Task::batch(tasks)
    }

    pub fn loaded(&mut self, key: Key, result: Result<Vec<u8>, String>) {
        let media = match result {
            Ok(bytes) => Media::Ready(image::Handle::from_bytes(bytes)),
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
    Done(String),
    Failed(String),
}

/// Read a local file and upload it to a channel; returns the server file id.
pub async fn upload(client: Client, channel_id: String, path: std::path::PathBuf) -> Result<String, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let bytes = tokio::fs::read(&path).await.map_err(|e| e.to_string())?;
    let result = client.upload_file(&channel_id, &name, bytes).await.map_err(|e| e.to_string())?;
    result.file_infos.into_iter().next().map(|f| f.id).ok_or_else(|| "server returned no file".into())
}

/// Download an attachment into the cache dir and open it with the system's default app.
pub async fn open_file(client: Client, file_id: String, name: String) -> Result<(), String> {
    // Only the final path component, so a crafted name can't escape the directory.
    let name = std::path::Path::new(&name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| file_id.clone());
    let dir = dirs::cache_dir().ok_or("no cache dir")?.join("darkmatter/files").join(&file_id);
    let path = dir.join(name);
    if !path.exists() {
        let bytes = client.file(&file_id).await.map_err(|e| e.to_string())?;
        tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
        tokio::fs::write(&path, bytes).await.map_err(|e| e.to_string())?;
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
        assert_eq!(upload(client, "c1".into(), file).await.unwrap(), "f9");

        let missing = upload(Client::new(&server.uri()).unwrap(), "c1".into(), dir.path().join("nope")).await;
        assert!(missing.is_err());
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(12 * 1024), "12 KB");
        assert_eq!(human_size(3_565_158), "3.4 MB");
    }
}
