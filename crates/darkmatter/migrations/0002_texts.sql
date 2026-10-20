-- Decrypted messages, by post id (or `pending:<id>` for this device's own
-- before the server numbers them). MLS can decrypt a message only once, so
-- this is the only copy of the text on this device. Local search (M2 step 8)
-- reads it too.
CREATE TABLE texts (
    k          TEXT PRIMARY KEY,
    channel_id TEXT NOT NULL,
    content    TEXT NOT NULL,
    create_at  INTEGER NOT NULL
);
CREATE INDEX texts_channel ON texts (channel_id);
