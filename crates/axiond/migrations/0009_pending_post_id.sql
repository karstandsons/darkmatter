-- The app's own id for a message it sent. A retry with the same id gets the
-- message already written instead of a second copy.
ALTER TABLE posts ADD COLUMN pending_post_id TEXT NOT NULL DEFAULT '';
CREATE UNIQUE INDEX posts_pending ON posts (user_id, pending_post_id) WHERE pending_post_id != '';
