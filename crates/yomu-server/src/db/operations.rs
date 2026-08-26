//! Operational SQLite probes and maintenance. These stay in the DB layer so
//! HTTP readiness and the background task exercise the same pool and options
//! as normal application writes.

use chrono::Utc;

use super::{Db, Result};

impl Db {
    /// Prove that the main SQLite database can acquire a writer and modify a
    /// transaction. The transaction is rolled back, so readiness never
    /// changes the probe row or application data.
    pub async fn probe_write(&self) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO readiness_probe (id, checked_at) VALUES (1, ?)
             ON CONFLICT(id) DO UPDATE SET checked_at=excluded.checked_at",
        )
        .bind(Utc::now())
        .execute(&mut *tx)
        .await?;
        tx.rollback().await?;
        Ok(())
    }

    /// SQLite's own consistency check. Intended for an operator-triggered
    /// maintenance command, not a per-request health probe on a large DB.
    pub async fn integrity_check(&self) -> Result<String> {
        Ok(sqlx::query_scalar("PRAGMA integrity_check")
            .fetch_one(&self.pool)
            .await?)
    }

    /// Ask SQLite to checkpoint frames that are not held by active readers.
    /// PASSIVE never blocks application traffic waiting for readers.
    pub async fn checkpoint_wal(&self) -> Result<()> {
        let _: (i64, i64, i64) = sqlx::query_as("PRAGMA wal_checkpoint(PASSIVE)")
            .fetch_one(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn cleanup_expired_sessions(&self) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM sessions WHERE expires_at < ?")
            .bind(Utc::now())
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    pub fn pool_size(&self) -> u32 {
        self.pool.size()
    }

    pub fn pool_idle(&self) -> usize {
        self.pool.num_idle()
    }
}
