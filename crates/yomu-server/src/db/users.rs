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

    /// User for a trusted identity-provider subject, created or refreshed from
    /// its claims. Authentik's proxy UID and OIDC `sub` can differ for the same
    /// person, so subjects are aliases: within the one configured provider, an
    /// exact normalized username joins another subject to the existing user.
    pub async fn upsert_oidc_user(
        &self,
        subject: &str,
        username: &str,
        display_name: &str,
    ) -> Result<User> {
        let username = username.trim().to_lowercase();
        if let Some(id) = self.identity_user_id(subject).await? {
            return self.refresh_user(id, display_name).await;
        }

        // Recover cleanly if a process stopped between creating an older user
        // row and registering its identity alias.
        if let Some(id) = sqlx::query_scalar::<_, String>("SELECT id FROM users WHERE subject = ?")
            .bind(subject)
            .fetch_optional(&self.pool)
            .await?
        {
            let id = parse_uuid(id)?;
            self.register_identity(subject, id).await?;
            return self.refresh_user(id, display_name).await;
        }

        // Authentik usernames are unique within the configured provider. This
        // is the bridge between its proxy UID and OIDC subject.
        if let Some(id) = sqlx::query_scalar::<_, String>("SELECT id FROM users WHERE username = ?")
            .bind(&username)
            .fetch_optional(&self.pool)
            .await?
        {
            let id = parse_uuid(id)?;
            self.register_identity(subject, id).await?;
            return self.refresh_user(id, display_name).await;
        }

        let id = Uuid::now_v7();
        let result = sqlx::query(
            "INSERT INTO users (id, subject, username, display_name, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(subject)
        .bind(&username)
        .bind(display_name)
        .bind(Utc::now())
        .execute(&self.pool)
        .await;
        match result {
            Ok(_) => self.register_identity(subject, id).await?,
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                // A concurrent request won either the subject or username
                // insert. Resolve it through the same trusted alias rules.
                let winner = if let Some(id) = self.identity_user_id(subject).await? {
                    id
                } else if let Some(id) = sqlx::query_scalar::<_, String>(
                    "SELECT id FROM users WHERE subject = ? OR username = ? LIMIT 1",
                )
                .bind(subject)
                .bind(&username)
                .fetch_optional(&self.pool)
                .await?
                {
                    parse_uuid(id)?
                } else {
                    return Err(DbError::Sqlx(sqlx::Error::Database(db)));
                };
                self.register_identity(subject, winner).await?;
                return self.refresh_user(winner, display_name).await;
            }
            Err(e) => return Err(e.into()),
        }

        let user = self.user_by_id(id).await?;
        self.claim_shared_history_if_sole_oidc_user().await?;
        Ok(user)
    }

    async fn identity_user_id(&self, subject: &str) -> Result<Option<Uuid>> {
        sqlx::query_scalar::<_, String>("SELECT user_id FROM user_identities WHERE subject = ?")
            .bind(subject)
            .fetch_optional(&self.pool)
            .await?
            .map(parse_uuid)
            .transpose()
    }

    async fn register_identity(&self, subject: &str, user_id: Uuid) -> Result<()> {
        sqlx::query(
            "INSERT INTO user_identities (subject, user_id, created_at)
             VALUES (?, ?, ?) ON CONFLICT(subject) DO NOTHING",
        )
        .bind(subject)
        .bind(user_id.to_string())
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn refresh_user(&self, id: Uuid, display_name: &str) -> Result<User> {
        sqlx::query("UPDATE users SET display_name = ? WHERE id = ?")
            .bind(display_name)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        let user = self.user_by_id(id).await?;
        self.claim_shared_history_if_sole_oidc_user().await?;
        Ok(user)
    }

    /// Repair subject-suffixed users created before identity aliases were
    /// supported. The suffix is an exact record of our old collision fallback,
    /// so this does not guess based on similar names.
    pub(super) async fn reconcile_qualified_users(&self) -> Result<()> {
        let duplicates = sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT duplicate.id, canonical.id, duplicate.username, canonical.username
             FROM users duplicate
             JOIN users canonical
               ON duplicate.username = canonical.username || '-' || duplicate.subject
             WHERE duplicate.subject IS NOT NULL
               AND canonical.subject IS NOT NULL
               AND duplicate.id != canonical.id",
        )
        .fetch_all(&self.pool)
        .await?;

        for (duplicate_id, canonical_id, duplicate_name, canonical_name) in duplicates {
            let mut tx = self.pool.begin().await?;
            let marks = sqlx::query(
                "INSERT INTO read_units (user_id, unit_id, at)
                 SELECT ?, unit_id, at FROM read_units WHERE user_id = ?
                 ON CONFLICT(user_id, unit_id) DO NOTHING",
            )
            .bind(&canonical_id)
            .bind(&duplicate_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            sqlx::query("DELETE FROM read_units WHERE user_id = ?")
                .bind(&duplicate_id)
                .execute(&mut *tx)
                .await?;
            let events = sqlx::query("UPDATE progress_events SET user_id = ? WHERE user_id = ?")
                .bind(&canonical_id)
                .bind(&duplicate_id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            sqlx::query("UPDATE sessions SET user_id = ? WHERE user_id = ?")
                .bind(&canonical_id)
                .bind(&duplicate_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE user_identities SET user_id = ? WHERE user_id = ?")
                .bind(&canonical_id)
                .bind(&duplicate_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE shared_history_claim SET user_id = ? WHERE user_id = ?")
                .bind(&canonical_id)
                .bind(&duplicate_id)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM users WHERE id = ?")
                .bind(&duplicate_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            tracing::info!(
                from = duplicate_name,
                to = canonical_name,
                progress_events = events,
                read_marks = marks,
                "unified identity-provider user aliases"
            );
        }
        Ok(())
    }

    /// Transfer the pre-authentication shared account's journal and read marks
    /// to the sole OIDC account. There is no ambiguity while exactly one real
    /// account exists, and the singleton claim row makes this a one-time
    /// migration across repeated logins and restarts.
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

        // This is a transfer, not a merge. Once both datasets are safely on
        // the authenticated user, clear the old shared journal in the same
        // transaction so there is never a partially migrated state.
        sqlx::query(
            "DELETE FROM progress_events
             WHERE user_id = '00000000-0000-0000-0000-000000000000'",
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM read_units
             WHERE user_id = '00000000-0000-0000-0000-000000000000'",
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        tracing::info!(
            user_id,
            progress_events = events.len(),
            read_marks = marks,
            "transferred single-account reading history"
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
