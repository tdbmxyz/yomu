# yomu core browser journeys

Fixture: `e2e/tests/seed.spec.ts`; source and IdP behavior live in `e2e/fixtures/server.mjs`.

## Authentication and account changes

1. Start signed out and enter the browser OIDC flow.
2. Sign in as Alice through the fixture IdP and observe Alice's account name.
3. Sign out and verify the anonymous shell returns.
4. Sign in as Bob and verify no Alice identity remains.

## Reading and offline library

1. As Alice, search the fixture source and verify its proxied cover decodes.
2. Track Fixture Farming.
3. Mark Chapter 1 read and verify its row state.
4. Download Chapter 1 to server and browser device storage.
5. Open it, advance a page, return, and verify Continue reading.
6. Remove the server copy while retaining the browser copy.
7. Request a Service Worker update and verify the active worker still controls the page.
8. Take Chromium offline, reload the publication, and open the device-saved chapter.

## Per-user state

1. As Bob, open the shared Fixture Farming publication and verify Alice's read mark is absent.
2. Switch back to Alice and verify her read mark remains.

## Offline failure paths and upgrades

- Read offline, reconnect, and verify the server receives the queued position once.
- A 429 response must retain pending progress for retry; a read/unread change while
  an acknowledgement is delayed must retain the newer desired state.
- Switch Alice → Bob with pending offline work: Bob must neither see nor submit it.
  Returning to Alice restores her pending work. Logout purges private SW responses.
- Simulate Cache API quota rejection in the worker: device save must report failure,
  never a complete device mark. Interrupt a save and retry without a false completion.
- Replace the served worker bytes, wait for controllerchange, then reload a saved
  publication offline and verify actual images decode (not just reader controls).
- Existing unscoped storage is retained for explicit recovery, not auto-replayed
  into a freshly authenticated account.

All scenarios use the real yomu server, SQLite database, scraper, browser Service
Worker, and fixture HTTP/IdP service. No browser mocks of successful Yomu API calls.
The fixture-only proxy injects transient statuses/delays and worker revisions;
quota faults are injected into Cache.prototype.put in the real worker. Tests must
not reuse an existing server: occupied fixture ports are an error, not permission
to drive a possibly non-test instance.
