-- The identity key backed up (E2EE.md §12), encrypted on the device under
-- a recovery key or a passphrase: one of each at most. Opaque here.
CREATE TABLE e2ee_backups (
    user_id   TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind      TEXT NOT NULL,
    data      TEXT NOT NULL,
    update_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, kind)
);
