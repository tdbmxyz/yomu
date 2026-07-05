# ADR 0003 — Auth: OIDC (authentik) or one shared account

Date: 2026-07-05 · Status: accepted

## Context

yomu is LAN-hosted for a household. Reading positions are personal — two
readers of the same series shouldn't fight over one "continue reading" —
but demanding accounts and passwords on a home server is friction nobody
asked for. An authentik instance may or may not exist next to yomu.

## Decision

The session layer is copied from chaos (opaque 244-bit tokens,
sha256-hashed in `sessions`, HttpOnly `yomu_session` cookie for browsers or
`Authorization: Bearer` for native clients, 90-day default expiry). The
identity layer differs from chaos in both directions:

- **No passwords at all.** yomu never stores credentials.
- **`[auth]` configured** → sign-in is an OIDC authorization-code flow with
  PKCE against the issuer (authentik). The callback exchanges the code and
  reads the **userinfo endpoint** (no JWT validation to get wrong; the
  claims come from the provider over TLS), upserts the user by `sub`, and
  mints a normal session. Discovery is fetched lazily so yomu boots while
  the IdP is down.
- **`[auth]` absent** → single-account mode: every request resolves to the
  seeded shared user (`everyone`, nil UUID). No login UI, no sessions —
  exactly the zero-friction behavior of a yomu without auth, and the mode
  the progress data migrates into.

What is per-user: **progress events only** (`progress_events.user_id`).
The library, downloads and categories stay server-wide — chapters are
downloaded once for the household. Browsing stays public in OIDC mode
(LAN posture, like chaos); only progress endpoints require a session, and
signed-out library views simply carry no positions.

## Consequences

- `GET /auth/me` never 401s: it reports `{mode, user?}` so the UI knows
  whether to render sign-in affordances at all.
- The offline outbox keeps events on 401/403 (signing in makes the same
  batch succeed) while still dropping genuinely poisonous 4xx batches.
- The service worker must let `/api/**` navigations through to the network
  — the login/callback redirects would otherwise be answered with the app
  shell from cache.
- Switching a server from single to OIDC mode keeps existing progress on
  the shared account; moving it to a personal account is a manual
  `UPDATE progress_events SET user_id = …` (documented, not automated).
- Native/Tauri shells can reuse everything with a loopback-redirect OIDC
  flow later; the bearer path through `request_token` is already there.
