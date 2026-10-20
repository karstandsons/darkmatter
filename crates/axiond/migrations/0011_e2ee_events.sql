-- Encrypted events on a post (E2EE.md §8): reactions, for now. The server
-- knows who sent one and to which post, not what it says.
CREATE TABLE e2ee_events (
    id TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL,
    post_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    -- The sender's own id for it, in its authenticated data; a retry with
    -- the same id gets the event already stored.
    client_id TEXT NOT NULL,
    create_at INTEGER NOT NULL,
    epoch INTEGER NOT NULL,
    ct TEXT NOT NULL
);
CREATE INDEX e2ee_events_post ON e2ee_events (post_id, create_at);
CREATE UNIQUE INDEX e2ee_events_client ON e2ee_events (user_id, client_id);
