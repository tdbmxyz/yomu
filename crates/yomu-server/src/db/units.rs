use chrono::Utc;
use uuid::Uuid;
use yomu_domain::{ChapterRef, ReadingUnit};

use super::*;

impl Db {
    /// Merge a fresh chapter listing from the source: new units are
    /// inserted, existing ones keep their id and download state. Returns
    /// the newly inserted units.
    pub async fn sync_units(
        &self,
        publication_id: Uuid,
        listing: &[ChapterRef],
    ) -> Result<UnitSync> {
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        let existing: std::collections::HashSet<String> = sqlx::query_scalar::<_, String>(
            "SELECT source_key FROM reading_units WHERE publication_id = ?",
        )
        .bind(publication_id.to_string())
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();

        // A scraped listing can contain the same chapter twice; keep the
        // first occurrence, otherwise the second upsert discards an id we
        // just recorded as new.
        let mut current_keys = std::collections::HashSet::new();
        let listing: Vec<&ChapterRef> = listing
            .iter()
            .filter(|c| current_keys.insert(c.key.clone()))
            .collect();

        let mut new_ids = Vec::new();
        for chapter in &listing {
            let id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO reading_units (id, publication_id, source_key, title, number, source_order,
                                       scanlator, fetched_at, published_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT (publication_id, source_key)
                 DO UPDATE SET title = excluded.title, number = excluded.number,
                               source_order = excluded.source_order,
                               published_at = COALESCE(excluded.published_at,
                                                       reading_units.published_at)",
            )
            .bind(id.to_string())
            .bind(publication_id.to_string())
            .bind(&chapter.key)
            .bind(&chapter.title)
            .bind(chapter.number)
            .bind(chapter.source_order)
            .bind(&chapter.scanlator)
            .bind(now)
            .bind(chapter.published_at)
            .execute(&mut *tx)
            .await?;
            if !existing.contains(&chapter.key) {
                new_ids.push(id);
            }
        }

        // Reconcile: a source can move a chapter to a new URL (a re-upload),
        // leaving the old row behind next to its twin — a duplicate the user
        // sees ("Episode 45" twice, different keys). The insert/update above
        // never removes it, so it accumulates across updater runs in a
        // long-lived library. A stale row (fell out of the listing) with a
        // recognizable twin still in it — same number, or same title when
        // unnumbered — is merged into the twin: read marks and the reading
        // journal follow, a downloaded copy is handed over, and the row goes
        // away. Stale rows without a twin are dropped only when nothing is
        // saved locally (state 'none'/'failed'), so downloaded content is
        // never discarded. Guarded by a non-empty listing so a transient
        // empty/failed scrape can't wipe a publication (selector sources already
        // error on an empty chapter list, but be defensive).
        let mut file_ops = Vec::new();
        let mut merged_twins = std::collections::HashSet::new();
        if !current_keys.is_empty() {
            type UnitMergeRow = (String, String, Option<f64>, String, String, Option<i64>);
            let rows: Vec<UnitMergeRow> = sqlx::query_as(
                "SELECT id, source_key, number, title, download_state, page_count
                 FROM reading_units WHERE publication_id = ?",
            )
            .bind(publication_id.to_string())
            .fetch_all(&mut *tx)
            .await?;
            let (stale, mut live): (Vec<_>, Vec<_>) = rows
                .into_iter()
                .partition(|(_, key, ..)| !current_keys.contains(key));

            for (id, _, number, title, state, page_count) in stale {
                let id = parse_uuid(id)?;
                let twins: Vec<usize> = live
                    .iter()
                    .enumerate()
                    .filter(|(_, (.., n, t, _, _))| match number {
                        Some(number) => *n == Some(number),
                        None => n.is_none() && *t == title,
                    })
                    .map(|(i, _)| i)
                    .collect();
                match twins.as_slice() {
                    &[i] => {
                        let twin_id = parse_uuid(live[i].0.clone())?;
                        // Read marks and the reading journal follow the twin
                        // (the journal's one exception to append-only: the
                        // chapter it points at is being replaced).
                        sqlx::query(
                            "INSERT OR IGNORE INTO read_units (user_id, unit_id, at)
                             SELECT user_id, ?, at FROM read_units WHERE unit_id = ?",
                        )
                        .bind(twin_id.to_string())
                        .bind(id.to_string())
                        .execute(&mut *tx)
                        .await?;
                        sqlx::query("UPDATE progress_events SET unit_id = ? WHERE unit_id = ?")
                            .bind(twin_id.to_string())
                            .bind(id.to_string())
                            .execute(&mut *tx)
                            .await?;
                        if state == "downloaded" && live[i].4 != "downloaded" {
                            sqlx::query(
                                "UPDATE reading_units
                                 SET download_state = 'downloaded', download_error = NULL,
                                     downloaded_at = (SELECT downloaded_at FROM reading_units WHERE id = ?),
                                     page_count = ?
                                 WHERE id = ?",
                            )
                            .bind(id.to_string())
                            .bind(page_count)
                            .bind(twin_id.to_string())
                            .execute(&mut *tx)
                            .await?;
                            live[i].4 = "downloaded".into();
                            file_ops.push(UnitFileOp::Rename {
                                from: id,
                                to: twin_id,
                            });
                        } else {
                            file_ops.push(UnitFileOp::Remove { unit: id });
                        }
                        sqlx::query("DELETE FROM reading_units WHERE id = ?")
                            .bind(id.to_string())
                            .execute(&mut *tx)
                            .await?;
                        merged_twins.insert(twin_id);
                    }
                    // No twin (or an ambiguous set): keep downloaded/in-flight
                    // rows, drop the rest.
                    _ if state == "none" || state == "failed" => {
                        sqlx::query("DELETE FROM reading_units WHERE id = ?")
                            .bind(id.to_string())
                            .execute(&mut *tx)
                            .await?;
                        file_ops.push(UnitFileOp::Remove { unit: id });
                    }
                    _ => {}
                }
            }
        }
        tx.commit().await?;

        let mut new_units = Vec::new();
        for id in new_ids {
            if !merged_twins.contains(&id) {
                new_units.push(self.get_unit(id).await?);
            }
        }
        Ok(UnitSync {
            new_units,
            file_ops,
        })
    }

    pub async fn get_unit(&self, id: Uuid) -> Result<ReadingUnit> {
        let row = sqlx::query_as::<_, UnitRow>("SELECT * FROM reading_units WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(DbError::NotFound)?;
        ReadingUnit::try_from(row)
    }

    /// Chapters in reading order: by number when present, source listing
    /// order (reversed, sources list newest first) as fallback.
    pub async fn list_units(&self, publication_id: Uuid) -> Result<Vec<ReadingUnit>> {
        let rows = sqlx::query_as::<_, UnitRow>(
            "SELECT * FROM reading_units WHERE publication_id = ?
             ORDER BY number IS NULL, number ASC, source_order DESC",
        )
        .bind(publication_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(ReadingUnit::try_from).collect()
    }

    pub async fn set_page_count(&self, id: Uuid, page_count: u32) -> Result<()> {
        sqlx::query("UPDATE reading_units SET page_count = ? WHERE id = ?")
            .bind(page_count)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
