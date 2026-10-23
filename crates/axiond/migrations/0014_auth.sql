-- Wrong passwords per account (or, for a login that matches no one, per
-- login string), kept across restarts so a restart doesn't reset a lockout.
CREATE TABLE login_failures (
    k TEXT PRIMARY KEY,
    count INTEGER NOT NULL,
    last_at INTEGER NOT NULL
);

-- Server-wide settings, e.g. the password hashing cost tuned at setup.
CREATE TABLE settings (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);

-- mfa_secret is now encrypted (`enc1:…`) under the key in secret.key; the
-- server encrypts any older plaintext one when it opens the database.
