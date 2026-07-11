use yomu_domain::Category;

use super::*;

impl Db {
    pub async fn list_categories(&self) -> Result<Vec<Category>> {
        let rows =
            sqlx::query_as::<_, CategoryRow>("SELECT * FROM categories ORDER BY position, id")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(Category::from).collect())
    }

    pub async fn set_category_update(&self, id: &str, update_enabled: bool) -> Result<Category> {
        let result = sqlx::query("UPDATE categories SET update_enabled = ? WHERE id = ?")
            .bind(update_enabled)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(DbError::NotFound);
        }
        let row = sqlx::query_as::<_, CategoryRow>("SELECT * FROM categories WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        Ok(Category::from(row))
    }
}
