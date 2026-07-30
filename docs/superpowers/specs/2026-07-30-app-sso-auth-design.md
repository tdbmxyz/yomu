# Signing the app into an SSO'd yomu

The Android and desktop shells cannot sign in. Behind a forward-auth
proxy they cannot reach the server at all, because a webview cannot ride
the outpost's `SameSite=Lax` cookie and a login in the system browser
lands in the *browser's* cookie jar.

Underneath that sits a second, larger problem: **yomu's own `[auth]` does
not actually protect yomu.** This spec fixes both, in that order.

Companion: `docs/app-auth-playbook.md`, notes from doing the same work on
another app against the same authentik. Section 7 of it is a list of bugs
that shipped; four of the six recur identically here.

## What the audit found

Every one of the 32 API routes, classified by which extractor it takes
(`CurrentUser` rejects without a session; `OptionalUser` never rejects):

| Gated by `CurrentUser` | Open to anyone |
| --- | --- |
| every mutation, `progress/*`, `backup/*` | `GET /library` |
| | `GET /manga/{id}` (detail + chapters) |
| | `GET /manga/{id}/cover`, `/fingerprints` |
| | `GET /chapters/{id}/pages`, `/pages/{n}` |
| | `GET /sources`, `/search`, `/sources/{id}/browse`, `/covers` |
| | `GET /categories`, `/updates`, `/downloads` |

So with `[auth]` configured, an unauthenticated caller can enumerate the
library and read the page images — the content itself. Writes are safe;
reading is not. This is the same shape as the chaos finding in the
playbook's §4, and it is why requiring identity everywhere is a
prerequisite rather than a follow-up.

Separately, and outside this repo: zeus attaches the authentik
forward-auth middleware to chaos only, and its yomu module configures no
`[auth]`, so `resolve()` returns `SHARED_USER` for every caller. A
traefik middleware has been added as an immediate stopgap; it locks the
app out until this work lands, which is the trade being made.

## Decisions

- **The app is its own OIDC client.** PKCE against authentik as a public
  client, natively in the shell.
- **It exchanges the result for a yomu session**, rather than presenting
  the authentik token on every request.
- **The app self-configures** from the server's health response; nothing
  about the IdP is compiled into the APK.

## 1. Identity on every route

**Default-deny at the layer, not per handler.** Adding `CurrentUser` to
28 handlers would work today and rot tomorrow: the next route added is
open again, and no test can catch it, because axum exposes no way to
enumerate a `Router`'s routes. So the gate moves one level up.

A middleware wraps the whole `/api/v1` router:

- it resolves the session once and puts the `User` in request
  extensions, so `CurrentUser` becomes a cheap read rather than a second
  database hit;
- with no user it returns 401 — **unless** the path is allowlisted;
- for the two image routes it also accepts a media token (§3).

A new route is therefore protected by the fact that someone had to
*exempt* it, which is the property worth having. `OptionalUser`
disappears from `library::list`, `library::detail`, `downloads::list` and
`updates::list`: they used it to enrich a response while staying usable
signed-out, and signed-out is no longer a state the API serves.

Allowlist — reachable with no session, and the test names them
explicitly:

| Route | Why |
| --- | --- |
| `GET /health` | "is the server there, and how do I sign in?" must always answer |
| `GET /auth/me` | reports mode and the current user; answering it is how the app learns it is signed out |
| `GET /auth/login`, `GET /auth/callback` | the browser sign-in flow itself |
| `POST /auth/exchange` | the app's sign-in (new, §2) |

**The coverage test** drives a representative request for every route the
router serves against a server with `[auth]` configured and no
credentials, asserting 401 outside the allowlist. Because the gate is a
layer, this list existing is a convenience rather than the guarantee —
the guarantee is that a route must be named in `is_public()` to be
reachable, and `is_public()` is a single `match` with its own tests.

In single-account mode (`[auth]` absent) nothing changes: `resolve()`
returns `SHARED_USER` and every request succeeds as before. This section
is therefore inert for every existing deployment until `[auth]` is
configured.

## 2. The app's sign-in

The shell obtains an authentik access token, then trades it once:

```
POST /api/v1/auth/exchange   { "access_token": "..." }
  → introspect at authentik (RFC 7662) with yomu's confidential creds
  → require active == true
  → require client_id == [auth].app_client_id
  → upsert user by sub
  → mint a yomu session
  ← 200 { "token": "...", "expires_at": "..." }
```

Three properties this buys:

- **No JWT verification anywhere.** No JWKS fetch, no algorithm
  negotiation. The playbook's §9 trap — a provider signing HS256 and
  publishing an empty `jwks/` — cannot apply.
- **The audience is checked.** Introspection returns which client the
  token was issued to. Without that check, an access token minted for
  *any other application on the same authentik* could be traded for a
  yomu session. A userinfo call cannot detect this; introspection can.
- **No refresh dance.** The app then holds a 90-day opaque yomu session
  and presents it as `Authorization: Bearer`. `offline_access` is not
  needed on the provider, and the playbook's §8 — an access token
  expiring while offline signing the user out — largely evaporates.
  When the session does expire, the app runs the browser flow again.

New config in `[auth]`:

```toml
app_client_id = "..."   # the public provider the shell uses; when empty,
                        # /auth/exchange answers 404 and only the browser
                        # flow exists
```

`OidcRuntime::Discovery` gains `introspection_endpoint`. Introspection is
authenticated with the existing `client_id`/`client_secret`, so no new
secret is introduced.

**Failure modes are distinct**, because "I could not sign you in" and "the
IdP is down" need different user-facing text: inactive token → 401;
wrong `client_id` → 403 (this token is not for yomu); introspection
unreachable → 502.

## 3. Images, which cannot carry a header

`crates/yomu-ui/src/cover.rs:76` and `pages/reader/stages.rs:286` load
covers and pages as plain `<img src>`. An `<img>` sends no
`Authorization` header, and the shell's cookies never apply from
`tauri://localhost`. Requiring a session on those two routes blanks every
cover and every page in the app.

**Media tokens.** `GET /api/v1/auth/media-token` (authenticated) returns

```json
{ "token": "<user-id>.<expiry>.<hmac>", "expires_at": "..." }
```

- Stateless: HMAC-SHA256 over `user_id|expiry` with a key generated at
  server start. Restarting invalidates outstanding tokens, which is
  harmless — they last an hour and the client refetches.
- `GET /manga/{id}/cover` and `GET /chapters/{id}/pages/{n}` accept
  **either** a session (bearer or cookie) or `?mt=<token>`.
- The UI holds one and appends it; it refreshes on 401 and shortly before
  expiry. One code path on web and shell alike, rather than relying on
  same-origin cookies in one and not the other.

Rejected: fetching every page as a blob. It would rewrite the reader's
loading and preloading for 20 MB chapters, inside an auth change.

Everything else the UI loads goes through `yomu-client`, which can send a
header.

## 4. The advertisement, and the gate

`/health` gains one additive block (`skip_serializing_if`, so the frozen
1.x wire is unchanged for old clients):

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub auth: Option<AuthAdvertisement>,   // { issuer, app_client_id }
```

Present only when `[auth]` has both an issuer and an `app_client_id`. A
server without SSO omits it and the app never shows sign-in. Nothing
about authentik is compiled into the APK: the app self-configures from a
server address alone.

The gate is a pure function, tested:

```rust
pub enum Gate { Ready, NeedsSignIn, Unreachable }
pub fn gate(health: Option<&HealthResponse>, has_token: bool, seen_before: bool) -> Gate
```

- health answered, no auth block → `Ready`
- health answered, auth block, token held → `Ready`
- health answered, auth block, no token → `NeedsSignIn`
- health did not answer → `Unreachable`

This is what makes "not signed in" distinguishable from "server
unreachable" — the failure that started the playbook. Behind a
forward-auth proxy an unauthenticated call is a 302 to an origin that
sends no CORS header, which a webview reports as a *network error*, so
the app cannot see the redirect. `/health` bypassing the proxy is what
gives that question an answer at all.

## 5. `yomu-client` carries a session

The client has no notion of a token today. It gains:

- `YomuClient::with_token(base, Option<String>)` and `token()`;
- `Authorization: Bearer` on every request when a token is held;
- `?mt=` appended by `cover_url()` and `page_url()` when a media token is
  held;
- **rebuild at call time in long-lived tasks.** Playbook §7.2: a client
  cloned at startup never sees a token mirrored later, and behind a proxy
  the resulting failure is not a recoverable 401 but a transport error,
  which yomu's `offline::cached` reads as "go offline and serve cache" —
  so sign-in appears to have done nothing. `use_client()` reads the
  current token each time it is called, and every detached task
  (`pull.rs`, `notify.rs`, the flush effect) builds its client inside the
  task rather than capturing one.

## 6. The shell

Native, never in the webview (playbook §5). Commands:

| command | does |
| --- | --- |
| `auth_start(issuer, client_id)` | generate PKCE verifier + state, store them, return the authorize URL |
| `auth_status()` | the yomu session token held, plus a human-readable status line |
| `auth_sign_out()` | drop stored tokens |
| internal `finish(code, state)` | verify state, exchange at authentik, POST `/auth/exchange`, persist the yomu session |

- PKCE: `S256 = base64url(sha256(verifier))`, verifier 43–128 chars,
  tested against the RFC 7636 appendix B vector.
- Tokens live in `tauri-plugin-store` (a file in the app data dir), which
  survives app updates — unlike webview `localStorage`. Only the yomu
  session token is mirrored into `localStorage`, so the synchronous
  client builder can read it. The authentik tokens never are.
- Deep link `xyz.tdbm.yomu://auth/callback` must agree in five places:
  the authentik provider, the shell's `REDIRECT_URI`, `tauri.conf.json`
  → `plugins.deep-link.desktop.schemes`, the Android manifest
  intent-filter (`DEFAULT` + `BROWSABLE`), and the Linux `.desktop`
  `MimeType=x-scheme-handler/…`.
- **A callback that arrives at startup must be read** (§7.3):
  `on_open_url` only fires for a *running* app, and Android routinely
  evicts the app while the browser is in front. `deep_link().get_current()`
  is checked during setup too.
- `tauri-plugin-single-instance` is registered first on desktop, or the
  callback starts a second copy.

## 7. The UI

- A sign-in screen for `Gate::NeedsSignIn`: one button, plus **the status
  line the shell writes** ("waiting for the browser to come back" →
  "callback received, exchanging it…" → "signed in" / "sign-in failed:
  …"). Playbook §7.6: shipped from the start, it is what turns "it
  doesn't work" into a one-round-trip diagnosis, and it stays useful in
  production.
- **No silent early returns** (§7.5). Any handler that can decline to act
  says so on screen.
- **Re-check on `visibilitychange`**, not only on a timer (§7.4):
  returning to the app *is* the event, and Android suspends webview
  timers for a backgrounded app. A slow poll stays as a backstop.
- Sign-out clears the session, the caches from
  `2026-07-30-list-keep-alive-design.md`, and the service worker's
  response cache — a cached response from one identity must not survive
  a sign-out. `CACHE` in `sw.js` is bumped.
- Session lifetime (§8): only sign out on an explicit sign-out, or a 401
  that survives while genuinely Online. A 401 while offline, or any
  transport error, is never a session verdict.

## 8. The §7.1 audit

Leptos context lookups need a reactive owner, and a `spawn_local` loses
it as soon as it awaits. A `use_context` after an await panics (for
`expect`) or silently returns `None`. Both shipped in chaos; both were
invisible.

yomu has ~31 context reads and ~41 `spawn_local`s. Two were already
found and fixed while building the list caches. Every one gets checked
for the specific pattern — *a context read after an await* — and fixed by
capturing during component setup. Signals are `Copy` and owner
independent; clients and config are not.

## 9. Rollout

1. **Server** (§1–§4). Inert until `[auth]` is configured; deploy first.
2. **authentik**: a second provider — OAuth2/OIDC, client type
   **Public**, PKCE **required (S256)**, redirect URI exactly
   `xyz.tdbm.yomu://auth/callback`, scopes `openid profile`. Note its
   client id.
3. **zeus**: `[auth]` gets `issuer`, `client_id`/`client_secret` (the
   existing confidential provider) and the new `app_client_id`.
4. **traefik**: a bearer-bypass router
   (`HeaderRegexp("Authorization", "^Bearer ")`) plus unauthenticated
   `/api/v1/health`, mirroring chaos's. **Never before step 1 ships** —
   that window leaves the API open. The browser path stays on
   forward-auth throughout, so the web UI keeps working at every step and
   a rollback is deleting two routers.
5. **App**.

## Testing

Server, all unit or router-level:

- the route-coverage test (§1), which fails when a new route skips auth;
- `/auth/exchange`: inactive token → 401; token issued to another client
  → 403; introspection unreachable → 502; success mints a session whose
  token authenticates a subsequent request;
- media tokens: a valid one opens a page image; expired, tampered, and
  wrong-user tokens do not; a media token does **not** authenticate any
  other route;
- the health advertisement appears only with both issuer and
  `app_client_id`, and old clients still parse the response.

Client and UI:

- `gate()` over its four cases;
- the client sends the bearer when it holds one and omits it otherwise;
- `cover_url`/`page_url` append `?mt=` only when a media token is held.

Shell:

- PKCE challenge against the RFC 7636 appendix B vector;
- the state check rejects a callback whose state was never issued.

## Out of scope

- **Multi-user yomu.** Sessions already carry a user id and progress is
  per-user, but nothing in the UI presents users, and this changes none
  of that.
- **Sign-in on the web UI.** The browser flow exists today and keeps
  working; behind forward-auth the proxy signs the browser in before
  yomu ever sees the request.
- **Rotating the media-token key across restarts.** A restart invalidates
  outstanding tokens; clients refetch. Persisting it buys nothing here.
