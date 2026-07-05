use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use yomu_domain::ApiErrorBody;

use crate::db::DbError;
use crate::sync::SyncError;
use yomu_source::SourceError;

#[derive(Debug)]
pub enum ApiError {
    NotFound,
    /// No (valid) session on a per-user endpoint in OIDC mode.
    Unauthorized,
    Unprocessable(String),
    /// The upstream scan site failed or changed layout.
    UpstreamFailed(String),
    Internal(String),
}

impl From<DbError> for ApiError {
    fn from(err: DbError) -> Self {
        match err {
            DbError::NotFound => ApiError::NotFound,
            DbError::Constraint(msg) => ApiError::Unprocessable(msg),
            DbError::Corrupt(_) | DbError::Sqlx(_) | DbError::Migrate(_) => {
                tracing::error!(error = %err, "database error");
                ApiError::Internal("internal error".into())
            }
        }
    }
}

impl From<SourceError> for ApiError {
    fn from(err: SourceError) -> Self {
        ApiError::UpstreamFailed(err.to_string())
    }
}

impl From<SyncError> for ApiError {
    fn from(err: SyncError) -> Self {
        match err {
            SyncError::UnknownSource(id) => {
                ApiError::Unprocessable(format!("source {id:?} is not configured"))
            }
            SyncError::Source(e) => e.into(),
            SyncError::Db(e) => e.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            ApiError::Unprocessable(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg),
            ApiError::UpstreamFailed(msg) => (StatusCode::BAD_GATEWAY, msg),
            ApiError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };
        (status, Json(ApiErrorBody { message })).into_response()
    }
}
