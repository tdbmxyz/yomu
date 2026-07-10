# "Already in library" marks in browse/search — design

## Problem

Browsing or searching a source shows no hint that a title is already
tracked; adding it again just errors with a constraint violation.

## Approach

Server-side annotation — the server owns the library, so results carry
the answer instead of every client re-deriving it.

- `MangaSummary` (yomu-domain) gains
  `#[serde(default, skip_serializing_if = "Option::is_none")]
  pub in_library: Option<Uuid>` — the library manga id when the summary
  matches a tracked manga. Sources never set it (always `None` there);
  only the API layer fills it, matching on `(source_id, source_key)` —
  the exact strings the add flow stores, so no normalization drift.
- api/sources.rs: `search`, `search_all` and `browse` run their results
  through one `annotate(&db, source_id, &mut results)` helper backed by
  a single query (`SELECT id, source_key FROM manga WHERE source_id = ?`)
  per response.

## UI

`SummaryCard` (yomu-ui pages/search.rs), when `in_library` is set:

- a small accent `✓` tag on the cover corner (reuses the unread-badge
  styling with the accent palette),
- the card's primary action becomes "Open" (link to
  `/manga/{in_library}`); the add/track actions are hidden.

## Testing

- Server test: add a manga, then a stubbed search/browse response
  containing its key → `in_library` equals the manga id; other results
  unannotated.
- UI: behavior covered by the type change (card branches on the
  option); visual pass in the E2E run.

## Rollout

Ships with the same release as the catalog cache (annotation runs on
both cached and live paths). Old clients ignore the extra field.
