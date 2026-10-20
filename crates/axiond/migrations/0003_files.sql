-- Uploaded files. The bytes live under the data folder by id, never by the
-- name someone gave them.

CREATE TABLE files (
    id                TEXT PRIMARY KEY,
    user_id           TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    channel_id        TEXT NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    -- Empty until a post uses the file.
    post_id           TEXT NOT NULL DEFAULT '',
    create_at         INTEGER NOT NULL,
    update_at         INTEGER NOT NULL,
    delete_at         INTEGER NOT NULL DEFAULT 0,
    name              TEXT NOT NULL,
    extension         TEXT NOT NULL DEFAULT '',
    size              INTEGER NOT NULL,
    mime_type         TEXT NOT NULL DEFAULT '',
    width             INTEGER NOT NULL DEFAULT 0,
    height            INTEGER NOT NULL DEFAULT 0,
    has_preview_image INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX files_post ON files (post_id) WHERE post_id != '';
