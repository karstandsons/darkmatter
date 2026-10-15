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

/// Download an attachment into the cache dir and open it with the desktop's default app.
pub async fn open_file(client: Client, file_id: String, name: String) -> Result<(), String> {
    // Only the final path component, so a crafted name can't escape the directory.
    let name = std::path::Path::new(&name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| file_id.clone());
    let dir = dirs::cache_dir().ok_or("no cache dir")?.join("mm-rust/files").join(&file_id);
    let path = dir.join(name);
    if !path.exists() {
        let bytes = client.file(&file_id).await.map_err(|e| e.to_string())?;
        tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
        tokio::fs::write(&path, bytes).await.map_err(|e| e.to_string())?;
    }
    std::process::Command::new("xdg-open").arg(&path).spawn().map_err(|e| e.to_string())?;
    Ok(())
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

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(12 * 1024), "12 KB");
        assert_eq!(human_size(3_565_158), "3.4 MB");
    }
}
