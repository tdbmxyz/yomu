# Genres + In-Library Search & Filtering — Design

**Goal:** Scrape genres from source detail pages, store them, and let the user search and filter their library by title and genre.

## Data model

Migration `0009_genres.sql`:

```sql
CREATE TABLE manga_genres (
    manga_id TEXT NOT NULL REFERENCES manga(id) ON DELETE CASCADE,
    genre    TEXT NOT NULL,
    PRIMARY KEY (manga_id, genre)
);
CREATE INDEX idx_manga_genres_genre ON manga_genres(genre);
```

## Domain (`yomu-domain`)

- `MangaDetails` gains `genres: Vec<String>` (scraped; `#[serde(default)]`).
- `Manga` gains `genres: Vec<String>` (stored; `#[serde(default)]`), flowing through `MangaWithPosition` and `MangaDetailResponse` unchanged.

## Scraping (`yomu-source`)

- Selector spec: optional `[manga] genres` rule (list selector). `SelectorSource::parse_manga` collects trimmed, non-empty, de-duplicated text of every matched element. Sources without the rule yield `vec![]`.
- `LocalSource`: optional `genres: Vec<String>` in `details.json` (`Details` struct), default empty.

## Server (`yomu-server`)

- `db/manga.rs`:
  - `set_genres(tx-or-pool, manga_id, &[String])` — replace-all (delete then insert) the manga's genre rows.
  - `genres_for(manga_id) -> Vec<String>` — one manga's genres (used by `get_manga`).
  - `genres_by_manga() -> HashMap<Uuid, Vec<String>>` — all genres grouped, one query, for `library::list`.
  - `get_manga` / `list_manga` results carry genres. `insert_manga` writes genres inside its existing transaction.
- `sync::refresh_manga` calls `set_genres` with the freshly-fetched `details.genres`, so genres refresh alongside chapters.
- `library::list` calls `genres_by_manga()` once and attaches per manga (query count stays flat).

## UI (`yomu-ui`, client-side)

`pages/library.rs`, above the category tabs:
- A title search `<input>` bound to a signal; filters case-insensitively by substring.
- Genre chips built from the union of genres across the loaded library; clicking toggles one active genre.
- Active genre AND search text AND the existing category tab compose as filters, all in-memory over the already-loaded `MangaWithPosition` list. No API changes; stays offline-friendly.

## Testing

- `db` test: insert a manga with genres, assert `genres_for` and `genres_by_manga`; assert `set_genres` replace semantics; assert `list_manga` carries genres.
- `selector` test: genre extraction from a detail-page fixture (incl. dedupe/trim).
- Extend the existing library lifecycle test to assert genres survive a round-trip.

## Notes / scope

- Genres populate once a source's TOML declares the `genres` selector and the manga is added or refreshed; pre-existing entries stay genre-less until refreshed. Expected.
- Genres are stored verbatim as scraped (no case normalization), matching how sources print them.
</content>
