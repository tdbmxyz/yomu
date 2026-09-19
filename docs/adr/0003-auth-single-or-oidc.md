# ADR 0003 — Auth: OIDC (authentik) or one shared account

Date: 2026-07-05 · Status: accepted, amended for the 2.x authentication model

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

Progress events and read marks are per-user. Library, downloads and categories
stay server-wide — chapters are downloaded once for the household. In OIDC mode
all content APIs require identity. Health/metrics and the sign-in surface are
public; the two media routes also accept short-lived signed media credentials.

## Consequences

- `GET /auth/me` never 401s: it reports `{mode, user?}` so the UI knows
  whether to render sign-in affordances at all.
- Offline work belongs to its originating account. Authentication and transient
  failures must not discard it or cause it to be replayed under another identity.
- The service worker must let `/api/**` navigations through to the network
  — the login/callback redirects would otherwise be answered with the app
  shell from cache.
- A former shared installation with exactly one OIDC account transfers the
  shared journal and read marks once, transactionally. Multiple accounts are
  ambiguous and do not trigger an automatic transfer.
- Native shells use authorization code + PKCE with a custom-scheme callback.
  Trusted proxy/native subjects are aliases of the same provider account. See
  [the maintenance checklist](../app-auth-playbook.md) for rollout constraints.
