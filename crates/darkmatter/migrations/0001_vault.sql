-- One encrypted database (SQLCipher) per account, its key in the system
-- keychain. Freed pages are overwritten (secure_delete), so keys openmls
-- deletes for forward secrecy don't linger in the file.

-- Small values: this device's id and keys, the identity key's secret.
CREATE TABLE kv (
    k TEXT PRIMARY KEY,
    v BLOB NOT NULL
);

-- openmls's state for this device's groups, as its own key/value pairs.
CREATE TABLE mls (
    k BLOB PRIMARY KEY,
    v BLOB NOT NULL
);

-- Other people's identity keys as first seen (trust on first use), and
-- whether you verified them.
CREATE TABLE identities (
    user_id      TEXT PRIMARY KEY,
    identity_key TEXT NOT NULL,
    first_seen   INTEGER NOT NULL,
    verified     INTEGER NOT NULL DEFAULT 0
);

-- Messages written but not yet taken by the server, in order.
CREATE TABLE outbox (
    position        INTEGER PRIMARY KEY,
    pending_post_id TEXT NOT NULL UNIQUE,
    item            TEXT NOT NULL
);
