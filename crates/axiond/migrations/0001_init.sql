-- Times are Unix milliseconds and ids are Mattermost-style 26-character
-- strings, so rows map one-to-one onto the API's models.

CREATE TABLE users (
    id                  TEXT PRIMARY KEY,
    create_at           INTEGER NOT NULL,
    update_at           INTEGER NOT NULL,
    delete_at           INTEGER NOT NULL DEFAULT 0,
    username            TEXT NOT NULL UNIQUE,
    email               TEXT NOT NULL UNIQUE,
    first_name          TEXT NOT NULL DEFAULT '',
    last_name           TEXT NOT NULL DEFAULT '',
    nickname            TEXT NOT NULL DEFAULT '',
    -- argon2id, PHC string format.
    password_hash       TEXT NOT NULL,
    roles               TEXT NOT NULL DEFAULT 'system_user',
    locale              TEXT NOT NULL DEFAULT 'en',
    last_picture_update INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE sessions (
    id               TEXT PRIMARY KEY,
    -- SHA-256 of the token; the token itself is never stored.
    token_hash       TEXT NOT NULL UNIQUE,
    user_id          TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- Reserved for end-to-end encryption: the device this session signs in.
    device_id        TEXT NOT NULL DEFAULT '',
    create_at        INTEGER NOT NULL,
    last_activity_at INTEGER NOT NULL,
    expires_at       INTEGER NOT NULL
);
CREATE INDEX sessions_user ON sessions (user_id);

CREATE TABLE teams (
    id           TEXT PRIMARY KEY,
    create_at    INTEGER NOT NULL,
    update_at    INTEGER NOT NULL,
    delete_at    INTEGER NOT NULL DEFAULT 0,
    name         TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    -- 'O' open, 'I' invite only.
    type         TEXT NOT NULL DEFAULT 'I'
);

CREATE TABLE team_members (
    team_id TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    roles   TEXT NOT NULL DEFAULT 'team_user',
    PRIMARY KEY (team_id, user_id)
);
CREATE INDEX team_members_user ON team_members (user_id);

CREATE TABLE channels (
    id                   TEXT PRIMARY KEY,
    -- Empty for direct and group messages, which belong to no team.
    team_id              TEXT NOT NULL DEFAULT '',
    -- 'O' public, 'P' private, 'D' direct, 'G' group.
    type                 TEXT NOT NULL,
    -- For direct messages: the two user ids, sorted, joined by '__'.
    name                 TEXT NOT NULL,
    display_name         TEXT NOT NULL DEFAULT '',
    header               TEXT NOT NULL DEFAULT '',
    purpose              TEXT NOT NULL DEFAULT '',
    create_at            INTEGER NOT NULL,
    update_at            INTEGER NOT NULL,
    delete_at            INTEGER NOT NULL DEFAULT 0,
    last_post_at         INTEGER NOT NULL DEFAULT 0,
    last_root_post_at    INTEGER NOT NULL DEFAULT 0,
    total_msg_count      INTEGER NOT NULL DEFAULT 0,
    total_msg_count_root INTEGER NOT NULL DEFAULT 0,
    creator_id           TEXT NOT NULL DEFAULT '',
    -- Reserved for end-to-end encryption (MLS).
    encrypted            INTEGER NOT NULL DEFAULT 0,
    UNIQUE (team_id, name)
);

CREATE TABLE channel_members (
    channel_id         TEXT NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    user_id            TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    roles              TEXT NOT NULL DEFAULT 'channel_user',
    last_viewed_at     INTEGER NOT NULL DEFAULT 0,
    -- The channel's message counts when the member last read it.
    msg_count          INTEGER NOT NULL DEFAULT 0,
    msg_count_root     INTEGER NOT NULL DEFAULT 0,
    mention_count      INTEGER NOT NULL DEFAULT 0,
    mention_count_root INTEGER NOT NULL DEFAULT 0,
    notify_props       TEXT NOT NULL DEFAULT '{}',
    last_update_at     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (channel_id, user_id)
);
CREATE INDEX channel_members_user ON channel_members (user_id);

CREATE TABLE posts (
    id        TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    user_id   TEXT NOT NULL,
    -- The thread's first post; empty for a post that starts one.
    root_id   TEXT NOT NULL DEFAULT '',
    create_at INTEGER NOT NULL,
    update_at INTEGER NOT NULL,
    edit_at   INTEGER NOT NULL DEFAULT 0,
    delete_at INTEGER NOT NULL DEFAULT 0,
    message   TEXT NOT NULL,
    type      TEXT NOT NULL DEFAULT '',
    props     TEXT NOT NULL DEFAULT '{}',
    file_ids  TEXT NOT NULL DEFAULT '[]',
    is_pinned INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX posts_channel ON posts (channel_id, create_at);
CREATE INDEX posts_channel_update ON posts (channel_id, update_at);
CREATE INDEX posts_root ON posts (root_id, create_at) WHERE root_id != '';
