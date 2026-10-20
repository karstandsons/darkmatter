-- A status someone set by hand: 'away', 'dnd' or 'offline'. Empty means
-- automatic: online while an app is connected, offline otherwise.
ALTER TABLE users ADD COLUMN status TEXT NOT NULL DEFAULT '';
