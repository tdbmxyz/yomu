use axum::Json;
use axum::extract::{Path, State};
use yomu_domain::{Category, UpdateCategoryRequest};

use super::ApiError;
use crate::auth::CurrentUser;
use crate::state::AppState;

pub async fn list(State(state): State<AppState>) -> Result<Json<Vec<Category>>, ApiError> {
    Ok(Json(state.db.list_categories().await?))
}

pub async fn update(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<String>,
    Json(req): Json<UpdateCategoryRequest>,
) -> Result<Json<Category>, ApiError> {
    Ok(Json(
        state
            .db
            .set_category_update(&id, req.update_enabled)
            .await?,
    ))
}
