# ADR-0004: Publication resources and format navigators

Status: accepted

## Context

The domain already distinguishes publications, reading units and locators, but
image-page enumeration alone cannot represent EPUB reflow or a PDF document.
Our structural reference is [Readium](https://github.com/readium), especially
its [architecture](https://readium.org/architecture/) and
[Web Publication Manifest](https://readium.org/webpub-manifest/).

## Decision

Keep publication opening/resource resolution in the server streamer. Expose a
small Web Publication Manifest-shaped contract: metadata, links, readingOrder
and supporting resources. Unit IDs are yomu extensions for journal identity.
This is not a claim of full Readium conformance or an embedded Readium toolkit.

Keep the reader host responsible for chrome, routing and persistence. Dispatch
comics to the existing image navigator, reflowable EPUB to an isolated document
navigator, and PDF to a locally packaged PDF.js navigator. A navigator reports
its displayed location; only then does the host record progress. Comics and
books share the shell, header/back control and immersive-mode lifecycle, not
format-specific rendering. EPUB/PDF frames report deliberate viewport taps through
a shared trusted interaction bridge; links, selection, drags and multi-touch are
not toggle intent. Chrome overlays the stable viewport and hidden controls are
removed from focus navigation; Escape restores them.

The library separates a work from its format-specific publications (versions).
All non-comic books share the Books shelf (`novels` remains the legacy wire name).
A nullable, additive `work_id` groups local versions by directory + case-folded
filename stem, never metadata title alone. Work IDs survive rescans, unambiguous
same-format renames and backups. Existing publication/unit IDs and journals are
not rewritten or merged. A work's library card chooses a present readable version,
then the user's latest locator, otherwise EPUB before PDF; its counts belong to
that version, not a sum of duplicate reading orders. Detail responses expose the
version list, and changing version never translates a locator. Categories apply
to the work; removal applies only to the selected version. MOBI/AZW3 can be
catalogued/downloaded but no navigator is advertised. Differently named books
currently need matching stems to be associated; manual linking is future work.

PDF covers use a lazy, bounded first-page Poppler subprocess (600px, two concurrent
workers, 20-second timeout and 8MiB output cap), then the usual private cover cache.
Poppler is an explicit runtime dependency of the Nix server package. Versions
without artwork can use a present sibling's cover. No user book or derived artwork
is checked into the repository.

EPUB units follow OPF spine order, not filename or comic chapter-number order.
EPUB 3 navigation labels and EPUB 2 NCX labels supplement document titles. Text
locations use resource-relative progression in [0, 1], not invented page numbers.
PDF locations remain 0-based pages, and rendering supplies the authoritative
page count if the scanner and renderer disagree. The existing journal merge
rule, per-user scope, backup/restore and offline outbox semantics remain intact;
progression is additive to the legacy wire shape.

EPUB documents run in a sandbox without same-origin access. The server sets CSP
to deny book scripts, remote assets, forms and embedded frames, allowing only a
fresh nonce-protected navigator bridge. The host validates message source against
the active frame. Media capabilities are short-lived, read-only path segments so
relative assets inherit authorization; no-referrer prevents leaking those URLs.
Capability resources permit credential-free CORS for opaque-frame fonts without
opening cookie-authenticated API CORS. Archive reads and traversal are bounded.
PDFs use streaming/range responses, not full-file response buffering. PDF.js
scripts, worker, fonts and decoder data are version-pinned, shipped locally with
license notices and loaded lazily; eval and XFA are disabled.

## Consequences and initial limits

- Standalone EPUB/PDF files directly under `books.dir` are supported. EPUB/PDF
  volumes nested in comic series folders are still reported as unsupported units.
- The EPUB navigator supports reflowable EPUB 2/3. Fixed-layout and encrypted or
  obfuscated EPUB resources are explicitly rejected. Encrypted PDFs are not supported.
- This slice provides section selection/links, text sizing, persistent paper/night
  palettes and EPUB reading-width presets, PDF page navigation/selectable text,
  fit-width/fit-page/percentage zoom
  and resume. Reader text colors/shadows are normalized independently of app/OS
  dark mode; publisher typography and illustration colors remain intact. An EPUB
  export may not contain the PDF's artwork (such as publisher callout icons); the
  reader does not substitute copied or invented book-specific art.
- Horizontal touch swipes navigate sections/fitted PDF pages. EPUB pinch changes
  text size; PDF pinch previews then rerasterizes at the new scale, with a bounded
  pixel budget. Native vertical scrolling and zoomed PDF panning remain available;
  multi-touch, cancelled gestures and interactive links must not become turns.
  Rich outlines/search, semantic text locators and persistent typography/zoom
  preferences are later work.
- Book resources and the PDF viewer bypass the comic Service Worker image-cache
  path. Verified EPUB/PDF device saves and offline dependency graphs are not yet
  implemented; the UI does not offer misleading comic download/save controls.
- Browser tests generate original small EPUB/PDF fixtures, including hostile EPUB
  script markup. Copyrighted manual-test books remain outside version control.
- Chromium rendering is covered by E2E tests. Native WebView rendering needs its
  own compatibility validation before treating it as equivalent browser coverage.
