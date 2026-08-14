# Signing a Leptos/Tauri app into an SSO'd server — playbook

Notes from making chaos's Android app sign in through authentik (2026-07-25 →
2026-07-30). Companion to `frontend-delivery-size.md`; same stack as yomu, so
the client half transfers almost verbatim.

Sections 1–6 are design. **Section 7 is the one to read twice** — every item in
it is a bug that shipped, reached a real phone, and took a round trip with the
user to diagnose. They are not hypothetical, and four of the six are Leptos or
Android behaviours that would recur identically in yomu.

**yomu's position:** the server half lives in `yomu-server/src/oidc.rs` and
`auth.rs`; the native half lives in `yomu-shell/src/auth.rs` and
`yomu-ui/src/auth.rs`. Yomu exchanges an authorization code + PKCE verifier
for its own opaque session (sha256 at rest), which native clients present as a
bearer while browsers use forward-auth/HttpOnly cookies. The implementation
follows the traps and rollout rules below; keep this document as the checklist
when changing it.

---

## 1. Pick where identity is checked — the two models differ downstream

**chaos:** the app holds an authentik access token and the server verifies that
JWT on every request (RS256 against a cached JWKS, `iss`/`aud`/`exp` strict).
No server-side session for the app at all.

**yomu:** OIDC identity is exchanged for yomu's *own* opaque session token,
which the app then presents as a bearer.

The yomu model is nicer for the client — the app carries one long-lived opaque
token, no refresh dance, no JWKS, no clock skew — but it needs a server
endpoint that turns a completed OIDC login into a session, and the app has to
end up holding that session token. Decide this **before** writing any client
code: it changes what the shell stores, whether refresh exists, and what the
"signed out" condition is.

Either way the rest of this document applies unchanged.

## 2. The app cannot borrow the browser's session

If the server sits behind a forward-auth proxy, the app cannot ride it:

- the outpost's cookie is `SameSite=Lax`, and the app's origin is
  `tauri://localhost` — it never rides along on a cross-origin call;
- a login performed in the system browser lands in the *browser's* cookie jar,
  not the WebView's.

So the flow must end in **a token the app holds**. Any design that hopes to
reuse a browser session is dead on arrival.

## 3. Make "you are not signed in" *readable*

The failure that started the whole project: a fresh install said "Cannot reach
the chaos server" when the server was perfectly reachable.

```
GET /api/v1/health   Origin: tauri://localhost
→ 302   (proxy redirect to the IdP, with CORS headers)
→ 302   auth.example.com — no CORS header for our origin
→ 200   login page
```

A WebView `fetch` follows redirects transparently, so the chain ends on an
origin that won't talk to us and the call fails as a **network error**. The app
literally cannot observe the 302. Unauthenticated is therefore
indistinguishable from unreachable unless you engineer a readable signal:

- **one endpoint reachable without auth** (`/api/v1/health`), so "is the server
  there?" always has an answer;
- **that endpoint advertises how to sign in** — issuer, client id, authorize
  URL. The app then self-configures from a server address alone, nothing about
  the IdP is compiled into the APK, and a server without SSO simply omits the
  block and the app skips sign-in entirely.

Then the gate has three states — **Ready / NeedsSignIn / Unreachable** — as a
pure function of (health response, token held, server-seen-before). Test that
function; it is the difference between a useful error and a lie.

## 4. Requiring auth everywhere is a *prerequisite*, not a follow-up

For the app's bearer to reach the server, its requests must bypass the proxy's
forward-auth (a traefik router matching `HeaderRegexp("Authorization", "^Bearer ")`,
plus an unauthenticated `/health`). The instant that exists, the proxy is no
longer protecting anything on that path.

chaos discovered at this point that **27 of its handlers had no auth at all** —
the proxy had been the only gate. Add a route-coverage test that hits every
route unauthenticated and asserts 401 except an explicit allowlist; it fails CI
when someone adds a route and forgets. Ours caught `POST /auth/logout`
immediately, which no one had thought about.

yomu is fine here: `resolve()` gates centrally, and with no `[auth]` configured
it deliberately resolves everyone to `SHARED_USER`. Just make sure the bypass
router and the auth requirement ship in the **same** deploy, server first.

## 5. Do the token exchange natively, never in the WebView

Two independent reasons:

- the IdP sends no `Access-Control-Allow-Origin` for `tauri://localhost` when
  the provider's redirect URI is a custom scheme, so a WebView-side exchange is
  simply blocked;
- the refresh token — the long-lived credential — then never touches WebView
  storage, which is neither durable nor private to your own code.

Shape that worked:

| command | does |
| --- | --- |
| `auth_start(issuer, client_id)` | generate PKCE verifier + state, store them, return the authorize URL |
| `auth_status()` | current token (refreshing if near expiry) **and** the last thing the flow did |
| `auth_sign_out()` | drop stored tokens |
| `finish(code, state)` (internal) | verify state, exchange, persist |

Store tokens with `tauri-plugin-store` (a file in the app data dir): it
survives app updates, unlike WebView `localStorage`. Mirror only the
short-lived access token into `localStorage` so the synchronous client builder
can read it; never mirror the refresh token.

PKCE: `S256 = base64url(sha256(verifier))`, verifier 43–128 chars. Test it
against the RFC 7636 appendix B vector — one assertion, and it either matches
the IdP's expectation exactly or it doesn't.

## 6. The callback scheme must agree in five places

`xyz.tdbm.<app>://auth/callback` appears in:

1. the IdP provider's redirect URI,
2. `REDIRECT_URI` in the shell's auth module,
3. `tauri.conf.json` → `plugins.deep-link.desktop.schemes`,
4. the Android manifest intent-filter (`scheme` + `host`, with `DEFAULT` and
   `BROWSABLE`),
5. the Linux `.desktop` entry's `MimeType=x-scheme-handler/…`.

A mismatch means the browser dead-ends and the app waits forever. Android needs
`android:launchMode="singleTask"` on the activity (Tauri's generated manifest
already has it). On the desktop, register `tauri-plugin-single-instance` first
in the builder, or the callback starts a second copy of the app.

## 7. The traps — all six of these actually happened

### 7.1 `use_context` is unavailable inside a spawned task (the expensive one)

Leptos context lookups need a reactive owner. A `spawn_local` loses it **as
soon as it awaits**. So this, inside a detached task, is broken:

```rust
// inside spawn_local, after an .await:
let client = use_client();          // use_context(...).expect(...)  →  PANICS
crate::auth::set_advertisement(x);  // use_context(...) → None, silently drops
```

Both failure modes bit us, and both were invisible:

- the **panic** killed the task that set the session after sign-in, so the app
  greeted its owner as "Hello stranger" until a restart happened to take a
  different path;
- the **silent `None`** meant the sign-in button read an empty advertisement
  signal and returned without a word — "the button does nothing", fixed only by
  killing the app so the value came from cache instead.

**Rule:** read context during component setup and pass what you need into the
task. Signals are `Copy` and owner-independent — pass those freely. Clients and
config are not; capture them first. A `ChaosClient` isn't `Copy` either, so
park it in a `StoredValue::new_local(...)` if a handler needs to stay `Copy`
for the view to reuse.

yomu has ~31 context reads and ~41 `spawn_local`s. Audit them: the pattern to
look for is a context read *after* an await, not merely inside a closure. The
correct examples in chaos are `offline.rs`'s retry handler and
`analytics.rs`'s `provide_overlay`, both of which capture the client up front —
`analytics` even parks it in a thread-local for later use from detached tasks.

### 7.2 A client cloned at startup never sees a token mirrored later

`App` clones its API client once at setup. If the token arrives afterwards (the
shell mirrors it a moment later), that client goes out anonymous forever.
Behind a proxy the resulting failure is *not* a recoverable 401 — it's the
redirect from §3, which arrives as a transport error, and transport-error
handling usually means "stay offline, keep cached data", so the sign-in appears
to have done nothing.

Rebuild the client with the current token at call time in any long-lived task.

### 7.3 A callback that arrives at startup is never read

`on_open_url` only fires for a *running* app. If the OS evicted the app while
the browser was in front — routine on Android — the URL is delivered as the
launch intent instead, and nothing looks at it. Symptom: sign-in only completes
after the user kills and reopens the app.

Also call `deep_link().get_current()` during setup and handle what it returns.

### 7.4 Timers are not a reliable way to notice the return

Polling for the token every 1.5 s is a fine backstop and a bad primary
mechanism: Android suspends WebView timers for a backgrounded app, and may kill
the process entirely. Re-check on **`visibilitychange`** — returning to the app
is itself the event you care about.

### 7.5 Silent early returns read as "the app is broken"

```rust
let Some(oidc) = advertisement.get_untracked().and_then(|a| a.oidc) else {
    return;   // user taps, nothing happens, no explanation
};
```

Any handler that can decline to act must say so on screen.

### 7.6 Build in on-device diagnostics before you need them

The user had no adb. We added a status string written by the shell at each step
("waiting for the browser to come back" → "callback received, exchanging it for
a token…" → "signed in" / "sign-in failed: …") and rendered it under the
sign-in button. That single line is what turned "it doesn't work" into a
one-round-trip diagnosis. Ship it from the start; it also stays useful in
production.

## 8. Session lifetime, especially offline

- An access token expiring **while offline must not sign the user out.** Only
  attempt a refresh when connectivity says Online.
- Clear the session on exactly three events: explicit sign-out, a refresh
  rejected with `invalid_grant` while online, or a 401 that survives one
  refresh attempt.
- Don't treat every API error as a session verdict — chaos originally dropped
  the session on *any* API error, which is part of why the app felt forgetful.
- On an offline boot, serve the cached user immediately; re-validate on the
  next Offline→Online transition.

## 9. authentik provider settings that matter

| setting | value | consequence if wrong |
| --- | --- | --- |
| Client type | **Public** | a secret in an APK is not a secret |
| PKCE | required, S256 | the only thing binding the code to your app |
| Signing key | **an RSA certificate** | proxy providers sign HS256 and publish an empty `jwks/` (`{}`) — local JWT verification then verifies nothing. Irrelevant if you exchange for your own session (yomu's model). |
| Scopes | `openid profile email` **+ `offline_access`** | without `offline_access` authentik issues **no refresh token**, and the user is silently signed out about an hour later — "it worked yesterday". The scope mapping must be assigned to the provider, not just requested. |
| Redirect URI | the custom scheme, exactly | browser dead-ends |

Verify before writing config: fetch `<issuer>/.well-known/openid-configuration`,
then its `jwks_uri`. A `{}` key set is the trap above.

## 10. Rollout order

1. Server: identity + auth on every route. Inert for existing clients.
2. Proxy: the bearer-bypass router, unauthenticated health route, and a
   narrowly matched bypass for the two image paths carrying `?mt=`. An `<img>`
   cannot send the bearer header; without that third router, forward-auth
   redirects every signed cover/page before yomu can validate its media token.
3. App.

Never 2 before 1 — that window leaves the API open. Keep the browser path on
forward-auth throughout; it means the web UI keeps working at every step and
you can roll back by deleting two routers.

---

## For yomu specifically

- **The server model is already chosen and implemented** (§1: exchange an
  authorization code + PKCE verifier for a yomu session), so there is no
  JWKS/refresh dance in the app. The shell stores the resulting opaque session
  and mirrors it into the WebView for `YomuClient`.
- **`allowed_origins` already exists** in the CORS layer — add the shell
  origins (`tauri://localhost`, `http://tauri.localhost`) there rather than
  reaching for a permissive layer.
- **The service worker cuts both ways.** `sw.js` caches API responses for
  offline reading; once responses are per-user, a cached response from one
  identity must not survive a sign-out. Clear the caches on sign-out the way
  chaos clears its `chaos-cache:` keys, and bump `CACHE` when the auth model
  lands.
- **Pull-to-refresh is the natural place for the 401 path.** `pull.rs` already
  triggers a refetch; that is exactly the moment to run "refresh the token
  once, retry, and only then conclude the session is gone" (§8). Wire the two
  together rather than building a second recovery path.
- **Audit for §7.1 before adding any of this.** The offline/sync code is the
  densest concentration of `spawn_local` in the codebase, and it is where a
  context read after an await will hurt most — the failure is a silent no-op or
  a dead task, never a compile error.
