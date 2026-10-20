-- Reactions, who each post mentions, and who follows which thread.

CREATE TABLE reactions (
    post_id    TEXT NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    user_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    emoji_name TEXT NOT NULL,
    create_at  INTEGER NOT NULL,
    PRIMARY KEY (post_id, user_id, emoji_name)
);

-- Kept so read state can be rewound ("mark as unread") and threads can
-- count unread mentions without reading message text again.
CREATE TABLE post_mentions (
    post_id TEXT NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (post_id, user_id)
);
CREATE INDEX post_mentions_user ON post_mentions (user_id);

-- Collapsed reply threads: a row once someone starts, replies to or is
-- mentioned in a thread. `following` goes to 0 when they unfollow, so a
-- later reply from someone else doesn't sign them up again.
CREATE TABLE thread_members (
    post_id     TEXT NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
    user_id     TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    following   INTEGER NOT NULL DEFAULT 1,
    last_viewed INTEGER NOT NULL DEFAULT 0,
    last_update INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (post_id, user_id)
);
CREATE INDEX thread_members_user ON thread_members (user_id, following);
