-- A server that switches from single-account mode to OIDC has one legacy
-- "Everyone" journal. The first real account claims a copy exactly once;
-- keeping this marker makes repeated logins/startups idempotent.
CREATE TABLE shared_history_claim (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    user_id    TEXT NOT NULL REFERENCES users(id),
    claimed_at TEXT NOT NULL
);
