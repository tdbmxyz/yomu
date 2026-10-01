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

## EPUB and PDF publications

1. Scan small, generated EPUB/PDF fixtures from the real books folder (never
   commit the user's test books).
2. Verify EPUB and PDF share the Books shelf and open in their own navigators;
   EPUB follows the package spine, not filename or chapter-number sorting.
   Matching filename stems identify versions of one work (not metadata titles).
   Show one card with an EPUB/PDF/MOBI version picker; MOBI is explicitly not
   readable yet. Switching versions preserves separate locations and resumes
   the latest available readable version from the library. PDF first-page covers
   must decode, including for PDFs with no EPUB counterpart. Categories apply
   to the whole work; removing a version never removes its siblings.
3. Render EPUB text and illustrations inside an isolated paper/night surface,
   even under OS dark mode and conflicting publisher black/red text or backgrounds.
   Both palettes have readable neutral text; changing colors survives section changes
   and reloads without inverting illustrations.
   Preserve publisher typography, diagrams and callout structure; verify readable
   heading/body/link contrast. Jump to a section, scroll, leave, resume and reload
   without rewinding progression. Horizontal swipes turn sections; vertical touch
   scrolling and text selection must not turn sections; pinch adjusts text size.
4. Render actual PDF pages (including in browsers/WebViews without a built-in
   PDF plugin), advance and resume at the same page. Verify fit-width, fit-page,
   percentage zoom, zoom buttons and Ctrl-wheel. Zoom rerenders both canvas and
   selectable text, stays on the same page, and survives page turns/viewport resizing.
   Real touch input verifies swipe turns, pinch zoom and panning while zoomed;
   a pinch or cancelled/vertical swipe must never become a page turn.
5. Comic, EPUB and PDF readers share the overlay/header/back control. Viewport
   clicks/taps hide and restore book chrome without resizing the frame or
   rewinding text/page location. Text selection, double-click, links, swipes and
   pinch must not hide controls accidentally. Escape restores hidden chrome,
   and hidden controls are not focusable. Back always returns to the publication.
6. EPUB reading width offers narrow/comfortable/wide/full presets in reader
   options. Changing width preserves progression and typography; the device
   retains the choice across sections/reload. Presets remain within a phone's
   viewport without horizontal overflow.
7. Confirm resource routes require authentication/capability, reject archive
   traversal and do not permit publication scripts to access the app.
8. Keep comics' existing offline journeys unchanged. Document device saves for
   EPUB/PDF as unsupported until their resource graph has an atomic save path.

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
