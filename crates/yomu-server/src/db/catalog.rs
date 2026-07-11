use chrono::{DateTime, Utc};
use yomu_domain::MangaSummary;

use super::*;

impl Db {
    /// Record summaries seen in a listing/search; the upsert keeps
    /// changed rows current and refreshes `last_seen_at` on the rest.
    pub async fn upsert_catalog_entries(
        &self,
        source_id: &str,
        items: &[MangaSummary],
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for item in items {
            sqlx::query(
                "INSERT INTO catalog_entries (source_id, key, title, cover_url, last_seen_at)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT (source_id, key) DO UPDATE SET
                     title = excluded.title,
                     cover_url = excluded.cover_url,
                     last_seen_at = excluded.last_seen_at",
            )
            .bind(source_id)
            .bind(&item.key)
            .bind(&item.title)
            .bind(item.cover_url.as_deref())
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn write_catalog_page(
        &self,
        source_id: &str,
        sort: &str,
        page: u32,
        keys: &[String],
        now: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO catalog_pages (source_id, sort, page, keys, fetched_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (source_id, sort, page) DO UPDATE SET
                 keys = excluded.keys, fetched_at = excluded.fetched_at",
        )
        .bind(source_id)
        .bind(sort)
        .bind(page)
        .bind(serde_json::to_string(keys).expect("string list serializes"))
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// A cached browse page, in listing order, with its fetch time.
    pub async fn read_catalog_page(
        &self,
        source_id: &str,
        sort: &str,
        page: u32,
    ) -> Result<Option<(Vec<MangaSummary>, DateTime<Utc>)>> {
        let Some((keys, fetched_at)) = sqlx::query_as::<_, (String, DateTime<Utc>)>(
            "SELECT keys, fetched_at FROM catalog_pages
             WHERE source_id = ? AND sort = ? AND page = ?",
        )
        .bind(source_id)
        .bind(sort)
        .bind(page)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };
        let keys: Vec<String> =
            serde_json::from_str(&keys).map_err(|e| DbError::Corrupt(e.to_string()))?;
        let mut items = Vec::with_capacity(keys.len());
        for key in &keys {
            let row = sqlx::query_as::<_, (String, Option<String>)>(
                "SELECT title, cover_url FROM catalog_entries
                 WHERE source_id = ? AND key = ?",
            )
            .bind(source_id)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
            if let Some((title, cover_url)) = row {
                items.push(MangaSummary {
                    key: key.clone(),
                    title,
                    cover_url,
                    in_library: None,
                });
            }
        }
        Ok(Some((items, fetched_at)))
    }

    /// Which source a cover URL belongs to — gate for the cover proxy
    /// (the server must not fetch arbitrary URLs).
    pub async fn catalog_source_for_cover(&self, cover_url: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT source_id FROM catalog_entries WHERE cover_url = ? LIMIT 1")
                .bind(cover_url)
                .fetch_optional(&self.pool)
                .await?,
        )
    }
}
