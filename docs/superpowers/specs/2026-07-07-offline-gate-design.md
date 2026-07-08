# Offline server gate — design

## Problem

On boot, `ServerGate` (crates/yomu-ui/src/lib.rs) health-checks the
configured server. When the check fails it only proceeds to the cached UI
if `navigator.onLine` is `false`. In the installed app away from home the
device *has* connectivity (mobile data) — `navigator.onLine` is `true` —
but there is no route to the self-hosted server, so the gate shows the
"cannot reach server" connect form instead of the reader's downloaded
library. Offline reading is expected behaviour and must not be blocked.

Covers are explicitly **out of scope**: they load fine in the shell, and
browser-offline being limited is acceptable.

## Approach (A)

Decide the gate by *"have we ever reached this server address?"* rather
than by `navigator.onLine`:

- health OK → record this address as seen, open the gate (Ready).
- health fails **and** this address was seen before → open the gate
  (Ready). The app is simply offline; individual pages already fall back
  to their last-known-good caches and device-saved chapters.
- health fails **and** this address was never seen → show the connect
  form (genuine first-run or a wrong address).

`navigator.onLine` is dropped from the decision. The "Continue anyway"
button stays for the first-run form; the connect form also remains
reachable from the More page.

## Components

- `offline::mark_server_seen(base: &str)` / `offline::server_seen(base:
  &str) -> bool`, backed by a localStorage set of base URLs
  (`yomu-servers-seen`). Scoping by base URL means pointing the app at a
  new address correctly re-arms the first-run form for that address.
- `ServerGate` gains an `Offline` gate state (distinct from `Ready`) so
  the app renders normally *and* a subtle, non-blocking "offline"
  indicator is shown. The indicator reuses existing muted/status styling;
  it does not block interaction and disappears once a later health check
  or reconnect succeeds.

## Data flow

Boot: `client.health()` →
- `Ok` ⇒ `mark_server_seen(base)`, state `Ready`.
- `Err` ⇒ `server_seen(base)` ? state `Offline` : state `Unreachable`.

`Offline` and `Ready` both render `children()`; `Offline` additionally
mounts the indicator. `Unreachable` renders the existing connect form
(`ConnectForm`, already extracted).

## Testing

- Headless: fresh localStorage + unreachable server ⇒ connect form
  (first-run). Then mark seen ⇒ reload against unreachable server ⇒ app
  renders (Library reachable) with the offline indicator, no connect
  wall. Healthy server ⇒ normal, no indicator, address recorded.

## Out of scope

Cover caching; changing the More-page connect form; the service worker.
