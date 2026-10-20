-- Invites, blocking, reports, and the admin audit log.

CREATE TABLE invites (
    id         TEXT PRIMARY KEY,
    -- SHA-256 of the invite code; the code itself is never stored.
    token_hash TEXT NOT NULL UNIQUE,
    created_by TEXT NOT NULL DEFAULT '',
    create_at  INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    uses_left  INTEGER NOT NULL,
    note       TEXT NOT NULL DEFAULT ''
);

CREATE TABLE blocks (
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    blocked_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    create_at  INTEGER NOT NULL,
    PRIMARY KEY (user_id, blocked_id)
);

CREATE TABLE reports (
    id          TEXT PRIMARY KEY,
    create_at   INTEGER NOT NULL,
    reporter_id TEXT NOT NULL,
    -- What's reported: a post, a person, or both.
    post_id     TEXT NOT NULL DEFAULT '',
    user_id     TEXT NOT NULL DEFAULT '',
    reason      TEXT NOT NULL,
    resolved_at INTEGER NOT NULL DEFAULT 0,
    resolved_by TEXT NOT NULL DEFAULT '',
    resolution  TEXT NOT NULL DEFAULT ''
);

-- What admins (and the command line) did. Never message text.
CREATE TABLE audit (
    id        TEXT PRIMARY KEY,
    create_at INTEGER NOT NULL,
    -- Empty for the `axiond` command on the server itself.
    actor_id  TEXT NOT NULL DEFAULT '',
    action    TEXT NOT NULL,
    target_id TEXT NOT NULL DEFAULT '',
    detail    TEXT NOT NULL DEFAULT ''
);
CREATE INDEX audit_time ON audit (create_at);
