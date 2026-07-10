# Source catalog cache + cover proxy — design

## Problem

Every visit to a source's catalog (Sources tab) re-fetches the listing
pages from the site, and result covers are hotlinked by the browser
straight from the site's CDN. Repeat visits burn rate-limited requests
for content that rarely changes, are slow on the phone, and leak the
reader's IP to the CDN.

## Approach: stale-while-revalidate catalog, proxied covers

### Storage (migration 0008)

```sql
CREATE TABLE catalog_entries (
    source_id  TEXT NOT NULL,
    key        TEXT NOT NULL,       -- MangaSummary.key (page URL)
    title      TEXT NOT NULL,
    cover_url  TEXT,
    last_seen_at TEXT NOT NULL,
    PRIMARY KEY (source_id, key)
);
CREATE TABLE catalog_pages (
    source_id  TEXT NOT NULL,
    sort       TEXT NOT NULL,       -- "popular" | "latest"
    page       INTEGER NOT NULL,
    keys       TEXT NOT NULL,       -- JSON array of entry keys, listing order
    fetched_at TEXT NOT NULL,
    PRIMARY KEY (source_id, sort, page)
);
```

Every summary the server obtains from a source — browse pages *and*
search results — passes through `upsert_catalog_entries`: insert new
keys, update rows whose title/cover changed (the "diff"), refresh
`last_seen_at`. No eviction (a catalog row is ~200 bytes; YAGNI).

### Read path

`GET /sources/{id}/browse?sort&page`:

1. Cached page younger than the TTL → serve from SQLite, no source
   traffic.
2. Cached page older than the TTL → serve it immediately AND spawn a
   background revalidation (single-flight per (source, sort, page)):
   fetch the listing, upsert entries, rewrite `catalog_pages`. The next
   request sees fresh data.
3. Never-seen page → fetch live (as today), store, serve.

TTL from config: `[catalog] ttl_secs`, default 21600 (6h). `ttl_secs =
0` disables caching reads (always live) but still records entries.
Staleness decisions are a pure function (`CachePlan::for_age`) so the
serve/revalidate/live choice is unit-testable.

Search stays live (unbounded query space; caching buys little) but its
results feed the same upsert, warming titles/covers for the proxy.

### Cover proxy

`GET /api/v1/covers?src=<url>`: serves the image from
`<data_dir>/covers/by-url/<sha256(url)>.<ext>`, fetching it once
through the owning source's rate-limited client (same
fetch-then-disk-cache shape as the per-manga cover endpoint, plus
`Cache-Control: public, max-age=86400`). Only URLs present in
`catalog_entries` (or the manga table) are proxied — the server must
not be an open proxy. Lookup: exact match on `cover_url`.

The API layer rewrites `cover_url` in browse/search responses to the
proxy URL (relative, `/api/v1/covers?src=…`), so clients change
nothing; the reader's device only ever talks to the yomu server.

## Components

- `crates/yomu-server/src/catalog.rs` — upsert + page read/write +
  `CachePlan` + revalidation single-flight (a `Mutex<HashSet<…>>` of
  in-flight page keys on `AppState`).
- db.rs gains the two tables' queries (kept in db.rs, same as
  everything else).
- api/sources.rs: browse rewritten around the cache; search gains the
  upsert + cover rewrite; new `covers` handler.
- Config: `CatalogConfig { ttl_secs }`, default 21600.

## Error handling

- Revalidation failures warn and leave the stale page in place (it
  self-heals next time the source answers).
- Cover fetch failures return 502 without caching, so a transient site
  error doesn't pin a broken cover.

## Testing

- db/catalog unit tests: upsert diff semantics (new / changed /
  unchanged), page round-trip.
- `CachePlan` staleness unit tests (fresh / stale / unknown / ttl=0).
- API test with a stub source: second browse within TTL performs no
  source call (stub counts calls); stale page still serves while
  revalidating.
- Cover proxy: URL not in catalog → 404; cached second hit reads disk.
- Live E2E against real sources: browse a page twice, assert the second
  serves from cache (server log), covers load through the proxy.

## Rollout

Server-only. Default-on with the 6h TTL; no config change needed on
zeus.
