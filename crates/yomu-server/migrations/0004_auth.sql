-- Users and sessions (same shape as chaos), plus per-user progress.
--
-- yomu has two auth modes: with `[auth]` configured, users sign in through
-- the OIDC provider (authentik); without it, every request acts as the
-- seeded shared account below — no login, shared tracking. Existing
-- progress events belong to that account either way.

CREATE TABLE users (
    id           TEXT PRIMARY KEY,
    -- OIDC `sub` claim; NULL for the built-in shared account.
    subject      TEXT UNIQUE,
    username     TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

-- The single-account mode identity (fixed nil UUID, see auth::SHARED_USER).
INSERT INTO users (id, subject, username, display_name, created_at) VALUES
    ('00000000-0000-0000-0000-000000000000', NULL, 'everyone', 'Everyone',
     '2026-07-05T00:00:00Z');

CREATE TABLE sessions (
    -- sha256 of the opaque token; the token itself never touches disk.
    token_hash TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
CREATE INDEX idx_sessions_user ON sessions(user_id);

-- Reading progress is per user. No REFERENCES clause: SQLite cannot
-- ALTER-ADD a foreign-key column with a non-NULL default; the value is
-- always written from an authenticated user server-side.
ALTER TABLE progress_events ADD COLUMN user_id TEXT NOT NULL
    DEFAULT '00000000-0000-0000-0000-000000000000';

DROP INDEX idx_progress_manga;
CREATE INDEX idx_progress_manga
    ON progress_events(manga_id, user_id, at DESC, id DESC);
