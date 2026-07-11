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
        if let Some(id) = existing {
            let id = parse_uuid(id)?;
            sqlx::query("UPDATE users SET display_name = ? WHERE id = ?")
                .bind(display_name)
                .bind(id.to_string())
                .execute(&self.pool)
                .await?;
            return self.user_by_id(id).await;
        }

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
            Ok(_) => self.user_by_id(id).await,
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
                    return self.user_by_id(parse_uuid(existing)?).await;
                }
                insert(format!("{}-{subject}", username.trim().to_lowercase()))
                    .execute(&self.pool)
                    .await?;
                self.user_by_id(id).await
            }
            Err(e) => Err(e.into()),
        }
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
