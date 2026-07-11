-- Genres/tags per manga, scraped from source detail pages. Normalized so
-- the library can filter by genre; membership is replace-all on each
-- refresh (see db::manga::set_genres).
CREATE TABLE manga_genres (
    manga_id TEXT NOT NULL REFERENCES manga(id) ON DELETE CASCADE,
    genre    TEXT NOT NULL,
    PRIMARY KEY (manga_id, genre)
);
CREATE INDEX idx_manga_genres_genre ON manga_genres(genre);
