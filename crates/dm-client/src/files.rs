//! Sending and fetching files in encrypted channels (E2EE.md §7). The server
//! gets each file, and its thumbnail, as an anonymous blob; their keys go in
//! the post.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use dm_crypto::wire::FileKey;
use mm_api::Client;

/// The name and type every encrypted upload gets on the server.
const BLOB: &str = "blob";

/// A file to send, as the person picked it.
pub struct Outgoing {
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
    /// A picture's size, and the smaller copy to show in the conversation.
    pub width: u32,
    pub height: u32,
    pub thumb: Option<Thumb>,
}

pub struct Thumb {
    pub mime: String,
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Encrypts and uploads a file (and its thumbnail) to a channel, returning
/// what the post needs to say about it.
pub async fn upload(api: &Client, channel_id: &str, file: Outgoing) -> Result<FileKey, String> {
    let Outgoing { name, mime, bytes, width, height, thumb } = file;
    let thumb = match thumb {
        Some(t) => Some(Box::new(upload_one(api, channel_id, format!("thumbnail of {name}"), t.mime, t.bytes, t.width, t.height).await?)),
        None => None,
    };
    let mut key = upload_one(api, channel_id, name, mime, bytes, width, height).await?;
    key.thumb = thumb;
    Ok(key)
}

async fn upload_one(api: &Client, channel_id: &str, name: String, mime: String, bytes: Vec<u8>, width: u32, height: u32) -> Result<FileKey, String> {
    let size = bytes.len() as u64;
    // Encrypting a big file takes a moment: not on the async threads.
    let (secret, ct, sha256) = tokio::task::spawn_blocking(move || {
        let (secret, ct) = dm_crypto::file::encrypt(&bytes);
        let sha256 = dm_crypto::file::sha256_hex(&ct);
        (secret, ct, sha256)
    })
    .await
    .map_err(|e| e.to_string())?;
    let uploaded = api.upload_file(channel_id, BLOB, ct).await.map_err(|e| e.to_string())?;
    let id = uploaded.file_infos.into_iter().next().map(|f| f.id).ok_or("the server returned no file")?;
    Ok(FileKey { id, key: B64.encode(secret.as_bytes()), name, mime, size, sha256, width, height, thumb: None })
}

/// Fetches a file and decrypts it, checking it's the one that was sent.
pub async fn download(api: &Client, key: &FileKey) -> Result<Vec<u8>, String> {
    let ct = api.file(&key.id).await.map_err(|e| e.to_string())?;
    let key = key.clone();
    tokio::task::spawn_blocking(move || key.open(&ct)).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())
}
