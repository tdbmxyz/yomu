# ADR 0001 — Native server with declarative selector sources

Date: 2026-07-05 · Status: accepted

## Context

The chaos seed document (docs/manga-app.md there) suggested starting as a
facade over Suwayomi-Server to keep the Tachiyomi extension ecosystem. The
requirement changed: no extension system is wanted — just an easy way to
point yomu at reference scan sites and download chapters from them.

## Decision

yomu owns the whole stack: its own library database, its own downloader, and
its own source layer. A source is either

1. a **selector source** — one TOML file (`sources.d/*.toml`) describing a
   site with CSS selectors (`selector[@attr]` mini-syntax, URL templates,
   rate limit, optional Referer). Adding a site requires no code; or
2. (later) a **native source** implementing the same `Source` trait in Rust,
   for API-based sites.

## Consequences

- No Java service, no APK extensions, no Suwayomi protocol coupling.
- Scan sites change layouts; when selectors break, the failure is explicit
  (parse errors surface on refresh/download as 502s with a reason) and the
  fix is editing one TOML file. Definitions are validated at startup and a
  broken file refuses to load rather than silently disappearing.
- Legality/ToS of scraping specific sites is the operator's responsibility;
  yomu enforces per-source politeness (serialized requests + min delay).
