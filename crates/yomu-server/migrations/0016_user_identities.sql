-- One Authentik user can arrive through the reverse-proxy headers in the web
-- UI and through OIDC `sub` in a native client. Those identifiers are not
-- necessarily identical, so a user may own more than one trusted subject.
CREATE TABLE user_identities (
    subject    TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL
);

INSERT INTO user_identities (subject, user_id, created_at)
SELECT subject, id, created_at
FROM users
WHERE subject IS NOT NULL;

CREATE INDEX idx_user_identities_user ON user_identities(user_id);
