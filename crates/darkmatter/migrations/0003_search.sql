-- Local search over what this device has decrypted (M2 step 8): the server
-- can't search encrypted conversations. One row per message, as last shown
-- (edits replace it); a deleted message goes with its texts. All of it is
-- inside the encrypted database.
CREATE TABLE indexed (
    id         INTEGER PRIMARY KEY,
    post_id    TEXT NOT NULL UNIQUE,
    channel_id TEXT NOT NULL,
    user_id    TEXT NOT NULL,
    root_id    TEXT NOT NULL,
    create_at  INTEGER NOT NULL,
    -- Of the text indexed, so showing it again doesn't rewrite it.
    hash       BLOB NOT NULL
);
CREATE INDEX indexed_time ON indexed (create_at);
-- rowid is indexed.id.
CREATE VIRTUAL TABLE search USING fts5(message, tokenize = 'unicode61 remove_diacritics 2');
