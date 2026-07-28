# Stable unit identity, and unavailable chapters — design

Two independent fixes, both found from a real library on 2026-07-29.

**Goal A:** a chapter that the source does not offer for free stops being
reported as a failure.

**Goal B:** a chapter keeps its identity when the source changes its URLs, so
device downloads survive — and the ones already orphaned are recovered rather
than re-downloaded.

---

## What happened

A source changed its series slug hash (`…-f886a8af` → `…-059befe1`), so every
chapter URL changed at once. `sync_units` treats the URL as the chapter's
identity: every listed chapter looked new, every stored chapter looked stale.
The reconcile pass then merged each stale row into its same-numbered twin —
moving read marks, the reading journal and the downloaded files — and deleted
the old row.

Server-side that is correct and lossless. Client-side it is silent data loss:
the app keys device downloads by unit id (`chapters/<uuid>/` in the Android
shell, `yomu-device-chapters` in localStorage, page URLs in the service-worker
cache). Nothing tells a client that unit X became unit Y, so every device copy
became invisible — files still on disk, marks still counted in "chapters on
this device", every row rendered as not-downloaded.

Measured on the live library: **1132 chapters across 6 publications**, all
re-keyed at 2026-07-27 03:54:53 (UUIDv7 timestamps, all identical). The three
publications from a different source kept their original ids.

The premium finding came from the same library: a chapter whose page fetch
fails with `unexpected page structure: no page images matched …`. The served
HTML has zero `<img>` tags and a title ending `- Premium`. Neighbouring free
chapters match 18–22 images with the same selector, so the config is right and
the chapter is simply not free.

---

## A. Unavailable chapters

### A1. A distinct state, not a failure

Add `DownloadState::Unavailable { at, reason }`.

**Wire compatibility.** The 1.x wire is frozen and 1.x clients reject an
unknown `state`, so this must not serialize as `"state":"unavailable"`. It
serializes as a failure carrying a flag:

```json
{"state":"failed","at":"…","reason":"premium on this source","unavailable":true}
```

A 1.x client sees a failure, which is the honest degraded reading. A 2.x
client sees the distinct state. Deserialization maps `failed + unavailable` →
`Unavailable`, `failed` alone → `Failed`. Golden wire tests pin both
directions.

### A2. Detecting it

The source definition gains an optional key:

```toml
[pages]
image = "…"
unavailable = "<selector matching a paywall marker>"
```

If no page images match **and** `unavailable` matches, the source returns
`SourceError::Unavailable(reason)` instead of `SourceError::Parse`. Sources
without the key behave exactly as today. Site-specific selectors stay out of
this repo, as always.

### A3. Behaviour

- The downloader records `Unavailable` rather than `Failed`.
- Auto-download skips units in that state — no repeated attempts each sweep.
- "Retry all" on the Downloads page skips them; they are not failures and
  retrying accomplishes nothing until the chapter is freed.
- A single-chapter download still retries one on request: a chapter can stop
  being premium, and that is the user's call to make.

### A4. Presentation

The Downloads page gets its own group, **"Not available (n)"**, placed after
the server queues, with a `Dismiss` action. Rows are muted rather than red —
`--muted` text and border, no `--down`. In the chapter list, an unavailable
unit reads as an ordinary undownloaded chapter with a small muted marker; it
must not look broken, because nothing is.

---

## B. Stable unit identity

### B1. Re-key in place instead of replacing rows

The fix is to stop treating the URL as the identity.

In `sync_units`, before the upsert loop: compute the incoming keys that are
new and the stored rows whose keys fell out of the listing. For each new key
that matches exactly one stale row by number (or by title when unnumbered),
and where that stale row matches exactly one new key, `UPDATE` that row's
`source_key` to the new value. Then run the existing upsert, which now finds
the row by key and updates it in place.

The id never changes, so download state, files, read marks, the journal and
every client-side key survive untouched. The existing reconcile pass stays for
what it was actually written for — a genuine duplicate, where the same chapter
appears twice under two keys.

Matching must be unambiguous in both directions. Anything ambiguous falls
through to today's behaviour, which is lossless server-side.

### B2. Recovering what is already orphaned

B1 prevents recurrence; it cannot undo 2026-07-27. The old ids are gone from
`reading_units`, `read_units` and `progress_events`, so no mapping survives.

What does survive is the content. A device page is byte-identical to the
server's stored page — the shell writes exactly the bytes
`/chapters/{id}/pages/{n}` returned, and that endpoint serves the stored file.
Verified on the live server: page 0 of a downloaded chapter is 221 184 B and
hashes identically across requests.

So a chapter can be recognised by `(page_count, sha256 of page 0)`.

**Server:** `GET /api/v1/manga/{id}/fingerprints` returns, for every
downloaded unit of that publication, `{unit_id, page_count, page0_sha256}`.
Additive, so it does not disturb the frozen wire.

**Shell:** three commands — `device_list_chapters()`,
`device_chapter_fingerprint(chapter)` (page count plus the hash of the
lowest-numbered page file), and `device_rename_chapter(from, to)`.

**Client:** a `Recover device downloads` action in More. For every device mark
whose unit id is not a current unit of its publication, fingerprint the local
directory and match it against that publication's server fingerprints. A
unique match renames the directory and re-keys the mark; anything ambiguous or
unmatched is left alone and counted in the report.

The mark records the publication id, which did not change, so the search is
scoped to one publication rather than the whole library.

**Not covered: the browser tier.** Pages there live in the service-worker
cache keyed by page URL, which embeds the unit id. Re-keying them means
copying every cached response to a new URL, and the recovery path this is
written for is a phone. The browser case is left to re-download, and the
report says so rather than implying it was handled.

### B3. A cheaper fallback for next time

`DeviceMark` gains `number: Option<f64>` (serde-defaulted, so existing marks
keep loading). It is what a future recovery would match on before resorting to
hashes, and it costs nothing to record.

---

## Testing

- **A1:** golden wire tests — `Unavailable` round-trips through the 1.x shape;
  a 1.x `failed` payload without the flag still deserializes as `Failed`.
- **A2:** a fixture page with no images and a paywall marker yields
  `Unavailable`; the same page without the configured key yields `Parse`, so
  existing sources are unaffected.
- **A3:** retry-all skips unavailable units; auto-download skips them.
- **B1:** the regression this is written for — a listing where *every* key
  changed re-keys every row and changes no ids, preserving download state and
  read marks. Plus: ambiguous number matches fall through; a genuine duplicate
  still merges.
- **B2:** fingerprints only list downloaded units; matching is exact on
  `(page_count, hash)`; an ambiguous pair is reported, not guessed.

## Risks

- **B1 mis-matching two different chapters that share a number.** Guarded by
  requiring uniqueness in both directions; the fallback is today's behaviour.

  **Measured after implementation, and it is not free.** If a source drops
  chapter 5 and publishes a *different* chapter also numbered 5 (a side story,
  an omake), the row keeps its id, its `Downloaded` state and its stored
  pages, and now points at the new chapter — server and device both serve the
  wrong content, permanently, with nothing to trigger a re-download. Note the
  pre-fix code did the same substitution through the merge path; the only
  difference is that it changed the id, so a device lost its copy and
  re-downloaded correct pages. The fix therefore trades *self-healing by data
  loss* for *silent wrong content* in the mis-match case. That is the right
  trade for the failure this exists to stop — a whole-library re-key — but it
  is a trade, not a free win.

- **B1 can suppress a new-chapter notification in one shape.** Uniqueness is
  checked within the stale and fresh sets, not against rows that stayed in the
  listing. If a stale row shares a number with a row still listed *and* a fresh
  key carries that number, the stale row is re-keyed onto the fresh key and the
  chapter is not announced as new. Requires an exact number collision. Recorded
  rather than fixed: the pre-fix outcome (a third row, announced) is not
  obviously better.
- **B2 renaming a directory onto an existing one.** The rename refuses if the
  target exists.
- **A2 hiding a real breakage.** If a site changes its markup *and* the
  `unavailable` selector happens to match, a genuine parse failure would be
  reported as premium. The selector is opt-in per source and should be narrow;
  the reason string names the source's own wording so it is recognisable.

---

## Verified — 2026-07-29, against the live library

Measured on the running production server (read-only, `GET` only; the
production database was never opened). The headline question was how much of
the orphaned library the fingerprint recovery can actually re-key, since it
refuses to guess when two chapters share a fingerprint.

**Answer: 661 of 661 fingerprintable chapters are uniquely identified. Zero
ambiguous.**

### Method

The `/fingerprints` endpoint does not exist on the running server (see
"What could not be verified" below), so the fingerprint was recomputed the way
the endpoint computes it, from public read-only endpoints:

- `GET /api/v1/manga/{id}` — unit list, download states, unit-id timestamps;
- `GET /api/v1/chapters/{unit}/pages` — `page_count`, which for a downloaded
  unit is the number of page *files on disk*, the same value the endpoint
  reports;
- `GET /api/v1/chapters/{unit}/pages/0` — bytes piped straight into `sha256sum`
  and discarded.

One request every 200 ms, 661 chapters, nothing written.

Caveat on strength: this measured the *original* fingerprint,
`(page_count, sha256 of page 0)`. The endpoint has since been strengthened to
carry the last page's hash as well, so the real matcher is strictly more
discriminating than what was measured here. The numbers below are therefore a
lower bound on how well it separates chapters.

### Per publication

The six publications re-keyed on 2026-07-27 are confirmed from the API: every
one of their units carries a unit id minted within a six-second window on that
date, and they total exactly **1132** chapters. The three publications from
other sources kept their original, older ids — the re-key was one source's
doing.

| publication | re-keyed units | downloaded (server) | fingerprinted | unique | ambiguous |
| --- | --- | --- | --- | --- | --- |
| `019f3442…9ce0` | 178 | 178 | 178 | 178 | 0 |
| `019f90c6…8e78b` | 267 | 266 | 266 | 266 | 0 |
| `019f5108…6bb9` | 322 | 4 | 4 | 4 | 0 |
| `019f90c1…5e36` | 152 | 0 | 0 | — | — |
| `019f5109…6c54` | 111 | 111 | 111 | 111 | 0 |
| `019f90c2…5994` | 102 | 102 | 102 | 102 | 0 |
| **total** | **1132** | **661** | **661** | **661** | **0** |

(Three further publications, 1995 units from two other sources, were not
re-keyed and are listed here only to record that they were checked.)

### What the measurement says about the design

- **The refusal-to-guess clause never fires on this library.** Not one
  `(page_count, sha256)` pair repeats inside a publication. Stronger: all 661
  page-0 hashes are distinct *across all five publications together*, so the
  hash alone carries the identity and `page_count` is only corroboration.
- **`page_count` alone would have been useless.** 628 of the 661 chapters
  share their page count with at least one sibling (173/178, 250/266, 107/111,
  96/102, 2/4). Page counts cluster tightly — 9–74 pages in the widest
  publication, 17–19 in the narrowest. Matching on number-of-pages, or on
  chapter number, would have been a coin flip; hashing the content is what
  makes the recovery decidable.
- **The real ceiling is not ambiguity, it is server coverage.** 471 of the
  1132 re-keyed chapters (42%) are not downloaded *on the server*, so no
  fingerprint exists for them and a device copy of one of those cannot be
  matched — it is left alone, uncounted as ambiguous but equally unrecovered.
  One publication (`019f90c1…`, 152 units) has nothing downloaded at all and
  is entirely outside the recovery's reach. This is a coverage limit of the
  approach, not a defect: the server can only recognise content it still
  holds. Whether it bites depends on how much the *device* holds that the
  server does not, which cannot be seen from the server side.

### What could not be verified here

Stated plainly rather than left to inference:

- **The re-key fix (B1) cannot be exercised against production.** The running
  server predates this branch: `GET /api/v1/manga/{id}/fingerprints` returns
  the SPA shell with `200 text/html`, which is both proof that the endpoint is
  absent and a sighting of the unrouted-`/api` bug fixed on this branch. B1 is
  covered by the in-repo regression test (a listing where every key changes,
  asserting ids, download state and read marks survive), not by production.
- **The end-to-end recovery was not run.** It needs a device with orphaned
  `chapters/<uuid>/` directories; there is none here. What was verified is the
  server half of the match — that the fingerprints it would serve are unique —
  plus the pure matching function's unit tests.
- **`yomu-shell` is unbuilt.** It needs GTK, which this machine does not have;
  it is excluded from `just check`. The three device commands are correct by
  inspection only.
- **The browser tier remains uncovered by design**, as B2 states.
