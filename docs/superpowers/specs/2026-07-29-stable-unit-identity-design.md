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
- **B2 renaming a directory onto an existing one.** The rename refuses if the
  target exists.
- **A2 hiding a real breakage.** If a site changes its markup *and* the
  `unavailable` selector happens to match, a genuine parse failure would be
  reported as premium. The selector is opt-in per source and should be narrow;
  the reason string names the source's own wording so it is recognisable.
