# yomu core browser journeys

Fixture: `e2e/tests/seed.spec.ts`; source and IdP behavior live in `e2e/fixtures/server.mjs`.

## Authentication and account changes

1. Start signed out and enter the browser OIDC flow.
2. Sign in as Alice through the fixture IdP and observe Alice's account name.
3. Sign out and verify the anonymous shell returns.
4. Sign in as Bob and verify no Alice identity remains.

## Reading and offline library

1. As Alice, search the fixture source and track Fixture Farming.
2. Mark Chapter 1 read and verify its row state.
3. Download Chapter 1 to server and browser device storage.
4. Open it, advance a page, return, and verify Continue reading.
5. Remove the server copy while retaining the browser copy.
6. Request a Service Worker update and verify the active worker still controls the page.
7. Take Chromium offline, reload the publication, and open the device-saved chapter.

## Per-user state

1. As Bob, open the shared Fixture Farming publication and verify Alice's read mark is absent.
2. Switch back to Alice and verify her read mark remains.

All scenarios use the real yomu server, SQLite database, scraper, browser Service Worker, and fixture HTTP/IdP service. CI must not intercept yomu API calls with browser mocks.
