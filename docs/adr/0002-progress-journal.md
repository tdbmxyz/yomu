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
Events are idempotent by id, so pushing a journal twice is harmless.

The sync cursor (`GET /progress/events?since=`) is the server-assigned
arrival sequence (`seq`, returned as `next_since`), **not** the event id.
Amended 2026-07-05: the original id cursor was unsound — ids are stamped by
the observing device at observation time, so a reconnecting offline device
pushes events that sort *before* cursors other clients already advanced
past, and those events would never be delivered to them. Arrival order
cannot skip a late push.

A push (`POST /progress/events`) is one transaction; events referencing
manga the server no longer knows are counted as `skipped` rather than
failing the batch — a permanently failing batch would wedge the client's
outbox behind one stale event forever.

## Consequences

- Offline merge needs no conflict UI: last-write-wins at page granularity is
  the product-correct answer, and the merge is associative/commutative.
- The journal grows forever; at one row per page turn this is negligible for
  a personal server (compaction can prune superseded events later without
  changing semantics).
- Server timestamps are used for online writes; offline devices stamp their
  own events. Clock skew between devices can only misorder events written
  while disconnected on several devices at once — acceptable for one reader.
