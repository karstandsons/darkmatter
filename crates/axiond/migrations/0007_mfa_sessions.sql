-- Two-factor login codes (TOTP), and which app each session is.

-- Base32 TOTP secret; set but inactive while someone is setting it up.
ALTER TABLE users ADD COLUMN mfa_secret TEXT NOT NULL DEFAULT '';
ALTER TABLE users ADD COLUMN mfa_active INTEGER NOT NULL DEFAULT 0;
-- The last time step a code was used for, so a code works only once.
ALTER TABLE users ADD COLUMN mfa_last_step INTEGER NOT NULL DEFAULT 0;

ALTER TABLE sessions ADD COLUMN user_agent TEXT NOT NULL DEFAULT '';
