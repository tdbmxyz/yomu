# ADR 0002 — Reading progress is an append-only journal

Date: 2026-07-05 · Status: accepted

## Context

Progress must survive multiple clients, including a future offline one that
reads downloaded chapters without the server and reconciles later. Required
granularity is chapter + page, nothing finer.

## Decision

Progress is never stored as mutable state. Every position change appends a
`ProgressEvent { id: UUIDv7, manga, chapter, page, device, at }`. The
current position is derived: max `at`, event id as tie-break
(`yomu_domain::merge_position` is the canonical rule; SQL mirrors it).
Events are idempotent by id, so pushing a journal twice is harmless. UUIDv7
ids double as a stable sync cursor (`GET /progress/events?since=`).

## Consequences

- Offline merge needs no conflict UI: last-write-wins at page granularity is
  the product-correct answer, and the merge is associative/commutative.
- The journal grows forever; at one row per page turn this is negligible for
  a personal server (compaction can prune superseded events later without
  changing semantics).
- Server timestamps are used for online writes; offline devices stamp their
  own events. Clock skew between devices can only misorder events written
  while disconnected on several devices at once — acceptable for one reader.
