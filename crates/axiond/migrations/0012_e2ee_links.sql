-- Linking a new device (E2EE.md §12): the new device's request, then the
-- approval another of the user's devices leaves for it. The server holds
-- public keys, a certificate and the identity key sealed to the new device.
CREATE TABLE e2ee_links (
    user_id       TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    device_id     TEXT NOT NULL,
    signature_key TEXT NOT NULL,
    link_key      TEXT NOT NULL,
    create_at     INTEGER NOT NULL,
    -- Set when approved.
    certificate   TEXT NOT NULL DEFAULT '',
    sealed        TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (user_id, device_id)
);
