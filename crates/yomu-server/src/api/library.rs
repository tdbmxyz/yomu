use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use uuid::Uuid;
use yomu_domain::{
    AddMangaRequest, Manga, MangaDetailResponse, MangaWithPosition, RefreshResponse,
    UpdateMangaRequest,
};

use super::ApiError;
use crate::auth::OptionalUser;
use crate::state::AppState;
use crate::sync;

pub async fn add(
    State(state): State<AppState>,
    Json(req): Json<AddMangaRequest>,
) -> Result<(StatusCode, Json<Manga>), ApiError> {
    let source = state.sources.get(&req.source_id).ok_or_else(|| {
        ApiError::Unprocessable(format!("source {:?} is not configured", req.source_id))
    })?;
    let details = source.manga(&req.source_key).await?;
    let manga = state
        .db
        .insert_manga(&req.source_id, &details, req.auto_download)
        .await?;

    if req.auto_download {
        let chapters = state.db.list_chapters(manga.id).await?;
        let ids: Vec<_> = chapters.iter().map(|c| c.id).collect();
        state.db.mark_pending(&ids).await?;
        state.download_notify.notify_one();
    }
    Ok((StatusCode::CREATED, Json(manga)))
}

/// The library is server-wide; reading positions are per user (absent when
/// signed out in OIDC mode).
pub async fn list(
    State(state): State<AppState>,
    OptionalUser(user): OptionalUser,
) -> Result<Json<Vec<MangaWithPosition>>, ApiError> {
    let mut out = Vec::new();
    for manga in state.db.list_manga().await? {
        let position = match &user {
            Some(user) => state.db.latest_position(user.id, manga.id).await?,
            None => None,
        };
        let chapters = state.db.list_chapters(manga.id).await?;
        let chapter_count = chapters.len() as u32;
        let read = match &user {
            Some(user) => state.db.read_ids(user.id, manga.id).await?,
            None => Default::default(),
        };
        let unread_count = chapters.iter().filter(|c| !read.contains(&c.id)).count() as u32;
        let position_chapter_title = position.as_ref().and_then(|p| {
            chapters
                .iter()
                .find(|c| c.id == p.chapter_id)
                .map(|c| c.title.clone())
        });
        out.push(MangaWithPosition {
            manga,
            position,
            chapter_count,
            unread_count,
            latest_chapter_at: chapters.iter().map(|c| c.fetched_at).max(),
            position_chapter_title,
        });
    }
    Ok(Json(out))
}

pub async fn detail(
    State(state): State<AppState>,
    OptionalUser(user): OptionalUser,
    Path(id): Path<Uuid>,
) -> Result<Json<MangaDetailResponse>, ApiError> {
    let manga = state.db.get_manga(id).await?;
    let mut chapters = state.db.list_chapters(id).await?;
    let position = match &user {
        Some(user) => state.db.latest_position(user.id, id).await?,
        None => None,
    };
    if let Some(user) = &user {
        let read = state.db.read_ids(user.id, id).await?;
        for chapter in &mut chapters {
            chapter.read = read.contains(&chapter.id);
        }
    }
    Ok(Json(MangaDetailResponse {
        manga,
        chapters,
        position,
    }))
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateMangaRequest>,
) -> Result<Json<Manga>, ApiError> {
    let mut manga = state.db.set_auto_download(id, req.auto_download).await?;
    if let Some(category) = &req.category {
        manga = state.db.set_category(id, category).await?;
    }
    Ok(Json(manga))
}

pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let chapter_ids: Vec<Uuid> = state
        .db
        .list_chapters(id)
        .await?
        .iter()
        .map(|c| c.id)
        .collect();
    state.db.delete_manga(id).await?;
    // Downloaded pages, cached cover and live page lists go with the manga.
    let _ = tokio::fs::remove_dir_all(state.config.data_dir.join(id.to_string())).await;
    let _ = remove_cover_cache(&state, id).await;
    state.live_pages.invalidate_many(&chapter_ids).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn refresh(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<RefreshResponse>, ApiError> {
    let manga = state.db.get_manga(id).await?;
    let new_chapters = sync::refresh_manga(&state, &manga).await?.len() as u32;
    Ok(Json(RefreshResponse {
        new_chapters,
        checked_at: chrono::Utc::now(),
    }))
}

/// Cover image, proxied from the source once and cached on disk (scan sites
/// often reject hotlinking, and the LAN client shouldn't need the site).
pub async fn cover(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let covers_dir = state.config.data_dir.join("covers");

    for ext in ["jpg", "png", "webp", "gif", "avif"] {
        let path = covers_dir.join(format!("{id}.{ext}"));
        if let Ok(bytes) = tokio::fs::read(&path).await {
            return Ok(cover_response(
                bytes,
                crate::downloader::content_type_for(&path),
            ));
        }
    }

    let manga = state.db.get_manga(id).await?;
    let cover_url = manga.cover_url.ok_or(ApiError::NotFound)?;
    let source = state
        .sources
        .get(&manga.source_id)
        .ok_or_else(|| ApiError::Unprocessable("source no longer configured".into()))?;
    let image = source.image(&cover_url).await?;

    let ext = crate::downloader::extension_for(&image.content_type, &cover_url);
    let _ = tokio::fs::create_dir_all(&covers_dir).await;
    let path = covers_dir.join(format!("{id}.{ext}"));
    let _ = tokio::fs::write(&path, &image.bytes).await;

    Ok(cover_response(
        image.bytes.to_vec(),
        crate::downloader::content_type_for(&path),
    ))
}

fn cover_response(bytes: Vec<u8>, content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            ),
        ],
        bytes,
    )
        .into_response()
}

async fn remove_cover_cache(state: &AppState, id: Uuid) -> std::io::Result<()> {
    let covers_dir = state.config.data_dir.join("covers");
    for ext in ["jpg", "png", "webp", "gif", "avif"] {
        let _ = tokio::fs::remove_file(covers_dir.join(format!("{id}.{ext}"))).await;
    }
    Ok(())
}
