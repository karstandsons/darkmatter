-- Custom emoji. The pictures are files under the data folder, by id.

CREATE TABLE emoji (
    id         TEXT PRIMARY KEY,
    creator_id TEXT NOT NULL,
    name       TEXT NOT NULL,
    create_at  INTEGER NOT NULL,
    update_at  INTEGER NOT NULL,
    delete_at  INTEGER NOT NULL DEFAULT 0
);
-- One live emoji per name; deleted ones keep theirs.
CREATE UNIQUE INDEX emoji_name ON emoji (name) WHERE delete_at = 0;
