-- End-to-end encryption (docs/axiond/E2EE.md §5): everything here is
-- public or ciphertext. The server never holds a key that reads messages.

-- Each user's identity key (Ed25519, base64): it signs their devices'
-- certificates.
CREATE TABLE e2ee_identities (
    user_id      TEXT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    identity_key TEXT NOT NULL,
    create_at    INTEGER NOT NULL,
    update_at    INTEGER NOT NULL
);

-- A device's certificate (JSON), signed by its user's identity key. A
-- device is a signed-in session; when the session ends the device is
-- revoked and taken out of every group.
CREATE TABLE e2ee_devices (
    user_id       TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    device_id     TEXT NOT NULL,
    certificate   TEXT NOT NULL,
    signature_key TEXT NOT NULL,
    create_at     INTEGER NOT NULL,
    revoked_at    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, device_id)
);

-- Key packages others claim to add a device to a group: one-time ones, and
-- one last-resort package handed out when those run out.
CREATE TABLE e2ee_key_packages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     TEXT NOT NULL,
    device_id   TEXT NOT NULL,
    data        BLOB NOT NULL,
    last_resort INTEGER NOT NULL DEFAULT 0,
    create_at   INTEGER NOT NULL,
    FOREIGN KEY (user_id, device_id) REFERENCES e2ee_devices (user_id, device_id) ON DELETE CASCADE
);
CREATE INDEX e2ee_key_packages_device ON e2ee_key_packages (user_id, device_id, last_resort);

-- One MLS group per encrypted channel: its current epoch and the latest
-- GroupInfo (handed to channel members joining by external commit).
CREATE TABLE e2ee_groups (
    channel_id TEXT PRIMARY KEY REFERENCES channels (id) ON DELETE CASCADE,
    epoch      INTEGER NOT NULL,
    group_info BLOB NOT NULL,
    create_at  INTEGER NOT NULL,
    update_at  INTEGER NOT NULL
);

-- The group's handshake messages in order: commits from members and the
-- server's removal proposals. Devices fetch what they missed by `seq`.
CREATE TABLE e2ee_handshakes (
    seq       INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id TEXT NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    epoch     INTEGER NOT NULL,
    kind      TEXT NOT NULL,
    user_id   TEXT NOT NULL DEFAULT '',
    device_id TEXT NOT NULL DEFAULT '',
    data      BLOB NOT NULL,
    create_at INTEGER NOT NULL
);
CREATE INDEX e2ee_handshakes_channel ON e2ee_handshakes (channel_id, seq);

-- Welcomes waiting for the devices a commit added.
CREATE TABLE e2ee_welcomes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id    TEXT NOT NULL,
    device_id  TEXT NOT NULL,
    channel_id TEXT NOT NULL REFERENCES channels (id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    data       BLOB NOT NULL,
    create_at  INTEGER NOT NULL
);
CREATE INDEX e2ee_welcomes_device ON e2ee_welcomes (user_id, device_id);

-- openmls's own state for the groups (the public tree, the group context,
-- the server's pending proposals), as key/value pairs per group. The row
-- with group_id '' is the server's external-sender key.
CREATE TABLE e2ee_mls (
    group_id TEXT NOT NULL,
    k        BLOB NOT NULL,
    v        BLOB NOT NULL,
    PRIMARY KEY (group_id, k)
);
