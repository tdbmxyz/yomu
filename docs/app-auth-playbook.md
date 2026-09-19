# Authentication maintenance checklist

Yomu exchanges browser/native OIDC authorization codes + PKCE for opaque Yomu
sessions (SHA-256 at rest). Browsers use HttpOnly cookies; native apps keep the
session in the shell store and mirror it for bearer requests. There is no native
IdP refresh-token/JWKS flow. See [ARCHITECTURE.md](ARCHITECTURE.md) and ADR 0003.

## Deployment order

1. Deploy the server-side identity gate first. Its route-coverage tests must keep
   all content APIs protected in OIDC mode.
2. Configure the reverse proxy to pass health/sign-in routes and bearer requests
   to Yomu, plus narrowly matched publication-cover/unit-page URLs carrying `mt`.
   Yomu, not the proxy bypass, validates these credentials. Never bypass all media.
3. Update clients. Keep the existing browser forward-auth path working throughout.

A WebView cannot borrow the system browser's cookies. A proxy redirect to an IdP
without WebView CORS becomes a transport error, not an observable 401. Health must
remain reachable and advertise the native app sign-in configuration.

## Native callback and provider

- Use a public OIDC app client with PKCE S256; never embed a client secret in an APK.
- Keep the redirect `xyz.tdbm.yomu://auth/callback` consistent between provider,
  shell constant, Tauri deep-link config, Android intent filter, and desktop handler.
- Process both running-app callbacks and startup intents. Android uses singleTask;
  desktop single-instance handling must precede deep-link handling.
- Check auth on visibilitychange as well as a timer: Android suspends timers and
  may kill the process while the system browser is foregrounded.
- Retain useful on-device status/error messages; never log tokens, codes or verifiers.

## UI and offline invariants

- Read Leptos context during component setup, not after await in a detached task.
- Clients capture credentials. Rebuild/reload after token or identity changes so
  background synchronization does not continue with a previous session.
- Offline/transport failures alone are not proof of sign-out. Validate identity
  before reconnect synchronization; isolate browser caches and outboxes by owner.
- Never replay another account's pending work. Legacy data without a known owner
  requires explicit recovery, not automatic adoption on an OIDC installation.
- Test shared-account upgrades, existing sessions, proxy/native identity aliases,
  logout, offline boot, and account changes with pending work.

Provider subjects and proxy IDs are aliases only within the single configured
provider. Current reconciliation uses its unique normalized username and exact
historical collision suffixes; do not broaden this to fuzzy names or email guesses.
