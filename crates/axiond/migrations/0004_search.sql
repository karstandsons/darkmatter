-- Full-text search over messages (FTS5), kept in step with `posts` by
-- triggers. Encrypted posts have no text, so there's nothing of them to index.

CREATE VIRTUAL TABLE posts_fts USING fts5(
    message,
    content = 'posts',
    content_rowid = 'rowid',
    tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TRIGGER posts_fts_insert AFTER INSERT ON posts BEGIN
    INSERT INTO posts_fts (rowid, message) VALUES (new.rowid, new.message);
END;

CREATE TRIGGER posts_fts_delete AFTER DELETE ON posts BEGIN
    INSERT INTO posts_fts (posts_fts, rowid, message) VALUES ('delete', old.rowid, old.message);
END;

CREATE TRIGGER posts_fts_update AFTER UPDATE OF message ON posts BEGIN
    INSERT INTO posts_fts (posts_fts, rowid, message) VALUES ('delete', old.rowid, old.message);
    INSERT INTO posts_fts (rowid, message) VALUES (new.rowid, new.message);
END;

INSERT INTO posts_fts (posts_fts) VALUES ('rebuild');
