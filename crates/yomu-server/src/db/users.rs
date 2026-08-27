use chrono::{DateTime, Utc};
use uuid::Uuid;
use yomu_domain::User;

use super::*;

impl Db {
    pub async fn user_by_id(&self, id: Uuid) -> Result<User> {
        let row = sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?
            .ok_or(DbError::NotFound)?;
        User::try_from(row)
    }

    /// User for an OIDC subject, created or refreshed from the provider's
    /// claims. The username falls back to the subject on collision (two
    /// providers' users sharing a preferred_username).
    pub async fn upsert_oidc_user(
        &self,
        subject: &str,
        username: &str,
        display_name: &str,
    ) -> Result<User> {
        let existing: Option<String> = sqlx::query_scalar("SELECT id FROM users WHERE subject = ?")
            .bind(subject)
            .fetch_optional(&self.pool)
            .await?;
        let user = if let Some(id) = existing {
            let id = parse_uuid(id)?;
            sqlx::query("UPDATE users SET display_name = ? WHERE id = ?")
                .bind(display_name)
                .bind(id.to_string())
                .execute(&self.pool)
                .await?;
            self.user_by_id(id).await?
        } else {
            let id = Uuid::now_v7();
            let insert = |username: String| {
                sqlx::query(
                    "INSERT INTO users (id, subject, username, display_name, created_at)
                     VALUES (?, ?, ?, ?, ?)",
                )
                .bind(id.to_string())
                .bind(subject.to_string())
                .bind(username)
                .bind(display_name.to_string())
                .bind(Utc::now())
            };
            let result = insert(username.trim().to_lowercase())
                .execute(&self.pool)
                .await;
            match result {
                Ok(_) => self.user_by_id(id).await?,
                Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                    // A unique violation is either a concurrent first-login for
                    // this same subject (the winner's row exists — return it) or
                    // a preferred_username collision (retry qualified by subject).
                    if let Some(existing) =
                        sqlx::query_scalar::<_, String>("SELECT id FROM users WHERE subject = ?")
                            .bind(subject)
                            .fetch_optional(&self.pool)
                            .await?
                    {
                        self.user_by_id(parse_uuid(existing)?).await?
                    } else {
                        insert(format!("{}-{subject}", username.trim().to_lowercase()))
                            .execute(&self.pool)
                            .await?;
                        self.user_by_id(id).await?
                    }
                }
                Err(e) => return Err(e.into()),
            }
        };

        self.claim_shared_history_if_sole_oidc_user().await?;
        Ok(user)
    }

    /// Copy the pre-authentication shared account's journal and read marks to
    /// the sole OIDC account. There is no ambiguity while exactly one real
    /// account exists, and the singleton claim row makes this a one-time
    /// migration across repeated logins and restarts. The shared copy remains
    /// available if the server is ever returned to single-account mode.
    pub(super) async fn claim_shared_history_if_sole_oidc_user(&self) -> Result<()> {
        let Some(user_id) = sqlx::query_scalar::<_, String>(
            "SELECT MIN(id) FROM users WHERE subject IS NOT NULL HAVING COUNT(*) = 1",
        )
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(());
        };

        let mut tx = self.pool.begin().await?;
        let claimed = sqlx::query(
            "INSERT INTO shared_history_claim (id, user_id, claimed_at)
             SELECT 1, ?, ? WHERE NOT EXISTS (SELECT 1 FROM shared_history_claim)",
        )
        .bind(&user_id)
        .bind(Utc::now())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if claimed == 0 {
            tx.rollback().await?;
            return Ok(());
        }

        let marks = sqlx::query(
            "INSERT INTO read_units (user_id, unit_id, at)
             SELECT ?, unit_id, at FROM read_units
             WHERE user_id = '00000000-0000-0000-0000-000000000000'
             ON CONFLICT(user_id, unit_id) DO NOTHING",
        )
        .bind(&user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        let events = sqlx::query_as::<_, (String, String, i64, String, DateTime<Utc>)>(
            "SELECT publication_id, unit_id, page, device, at FROM progress_events
             WHERE user_id = '00000000-0000-0000-0000-000000000000'
             ORDER BY seq",
        )
        .fetch_all(&mut *tx)
        .await?;
        for (publication_id, unit_id, page, device, at) in &events {
            sqlx::query(
                "INSERT INTO progress_events
                 (id, publication_id, unit_id, page, device, at, user_id)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(publication_id)
            .bind(unit_id)
            .bind(page)
            .bind(device)
            .bind(at)
            .bind(&user_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        tracing::info!(
            user_id,
            progress_events = events.len(),
            read_marks = marks,
            "claimed single-account reading history"
        );
        Ok(())
    }

    pub async fn create_session(
        &self,
        token_hash: &str,
        user_id: Uuid,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at)
             VALUES (?, ?, ?, ?)",
        )
        .bind(token_hash)
        .bind(user_id.to_string())
        .bind(Utc::now())
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        // Opportunistic cleanup; logins are rare enough that this is free.
        sqlx::query("DELETE FROM sessions WHERE expires_at < ?")
            .bind(Utc::now())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Resolve a session token hash to its (non-expired) user.
    pub async fn user_by_session(&self, token_hash: &str) -> Result<User> {
        let row = sqlx::query_as::<_, UserRow>(
            "SELECT u.* FROM users u
             JOIN sessions s ON s.user_id = u.id
             WHERE s.token_hash = ? AND s.expires_at >= ?",
        )
        .bind(token_hash)
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(DbError::NotFound)?;
        User::try_from(row)
    }

    pub async fn delete_session(&self, token_hash: &str) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token_hash)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
