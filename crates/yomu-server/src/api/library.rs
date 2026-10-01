use axum::Json;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use uuid::Uuid;
use yomu_domain::{
    AddPublicationRequest, BookFormat, Kind, Origin, Publication, PublicationDetailResponse,
    PublicationEdition, PublicationLink, PublicationManifest, PublicationMetadata,
    PublicationWithLocator, RefreshResponse, RescanResponse, UpdatePublicationRequest,
};

use super::ApiError;
use crate::auth::{CurrentUser, OptionalUser};
use crate::state::AppState;
use crate::sync;

#[cfg(test)]
mod tests;

pub async fn add(
    State(state): State<AppState>,
    _user: CurrentUser,
    Json(req): Json<AddPublicationRequest>,
) -> Result<(StatusCode, Json<Publication>), ApiError> {
    let source = state.sources.get(&req.source_id).ok_or_else(|| {
        ApiError::Unprocessable(format!("source {:?} is not configured", req.source_id))
    })?;
    let details = source.manga(&req.source_key).await?;
    let publication = state
        .db
        .insert_publication(&req.source_id, &details, req.auto_download)
        .await?;

    if req.auto_download {
        let units = state.db.list_units(publication.id).await?;
        let ids: Vec<_> = units.iter().map(|c| c.id).collect();
        state.db.mark_pending(&ids).await?;
        state.download_notify.notify_one();
    }
    Ok((StatusCode::CREATED, Json(publication)))
}

/// The library is server-wide; reading positions are per user (absent when
/// signed out in OIDC mode).
pub async fn list(
    State(state): State<AppState>,
    OptionalUser(user): OptionalUser,
) -> Result<Json<Vec<PublicationWithLocator>>, ApiError> {
    // Three queries total, not 3N+1: unit rollups (counts + latest) and
    // per-publication locators come back grouped, keyed by publication id.
    let rollup_scope = user.as_ref().map(|u| u.id.to_string()).unwrap_or_default();
    let mut rollups = state.db.library_rollups(&rollup_scope).await?;
    let mut positions = match &user {
        Some(user) => state.db.latest_positions(user.id).await?,
        None => Default::default(),
    };

    let out = state
        .db
        .list_publications()
        .await?
        .into_iter()
        .map(|publication| {
            let rollup = rollups.remove(&publication.id).unwrap_or_default();
            let (locator, locator_unit_title) = match positions.remove(&publication.id) {
                Some((locator, title)) => (Some(locator), title),
                None => (None, None),
            };
            PublicationWithLocator {
                editions: PublicationEdition::from_publication(&publication)
                    .into_iter()
                    .collect(),
                locator,
                unit_count: rollup.unit_count,
                unread_count: rollup.unread_count,
                downloaded_count: rollup.downloaded_count,
                latest_unit_at: rollup.latest_unit_at,
                locator_unit_title,
                publication,
            }
        })
        .collect();
    Ok(Json(group_book_versions(out)))
}

/// One library card per work. Counts and locators come ONLY from the selected
/// version. Prefer a present, readable version, then the user's latest locator,
/// then EPUB over PDF. Missing/unsupported versions stay visible in the picker.
fn group_book_versions(entries: Vec<PublicationWithLocator>) -> Vec<PublicationWithLocator> {
    let mut works: std::collections::BTreeMap<Uuid, Vec<PublicationWithLocator>> =
        Default::default();
    for entry in entries {
        works
            .entry(entry.publication.work_id.unwrap_or(entry.publication.id))
            .or_default()
            .push(entry);
    }
    let mut out = Vec::new();
    for mut versions in works.into_values() {
        let title = versions
            .iter()
            .min_by_key(|v| (v.publication.book_format(), v.publication.id))
            .expect("nonempty group")
            .publication
            .title
            .clone();
        let mut editions: Vec<_> = versions
            .iter()
            .flat_map(|v| v.editions.iter().cloned())
            .collect();
        editions.sort_by_key(|e| (e.format, e.filename.clone(), e.id));
        versions.sort_by_key(|v| {
            (
                v.publication.missing_since.is_some(),
                !v.publication.book_format().is_none_or(BookFormat::readable),
                std::cmp::Reverse(v.locator.as_ref().map(|l| l.at)),
                v.publication.book_format(),
                v.publication.id,
            )
        });
        let mut selected = versions.remove(0);
        selected.publication.title = title;
        selected.editions = editions;
        out.push(selected);
    }
    out.sort_by_key(|v| (v.publication.title.to_lowercase(), v.publication.id));
    out
}

pub async fn detail(
    State(state): State<AppState>,
    OptionalUser(user): OptionalUser,
    Path(id): Path<Uuid>,
) -> Result<Json<PublicationDetailResponse>, ApiError> {
    let publication = state.db.get_publication(id).await?;
    let mut units = state.db.list_units(id).await?;
    let locator = match &user {
        Some(user) => state.db.latest_position(user.id, id).await?,
        None => None,
    };
    if let Some(user) = &user {
        let read = state.db.read_ids(user.id, id).await?;
        for unit in &mut units {
            unit.read = read.contains(&unit.id);
        }
    }
    let editions = state
        .db
        .book_editions(&publication)
        .await?
        .iter()
        .filter_map(PublicationEdition::from_publication)
        .collect();
    Ok(Json(PublicationDetailResponse {
        publication,
        editions,
        units,
        locator,
    }))
}

pub async fn update(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdatePublicationRequest>,
) -> Result<Json<Publication>, ApiError> {
    let mut publication = state.db.set_auto_download(id, req.auto_download).await?;
    if let Some(category) = &req.category {
        publication = state.db.set_category(id, category).await?;
    }
    Ok(Json(publication))
}

pub async fn delete(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let unit_ids: Vec<Uuid> = state
        .db
        .list_units(id)
        .await?
        .iter()
        .map(|c| c.id)
        .collect();
    state.db.delete_publication(id).await?;
    // Downloaded pages, cached cover and live page lists go with the
    // publication.
    let _ = tokio::fs::remove_dir_all(state.config.data_dir.join(id.to_string())).await;
    let _ = remove_cover_cache(&state, id).await;
    state.live_pages.invalidate_many(&unit_ids).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn refresh(
    State(state): State<AppState>,
    _user: CurrentUser,
    Path(id): Path<Uuid>,
) -> Result<Json<RefreshResponse>, ApiError> {
    let publication = state.db.get_publication(id).await?;
    let new_units = match &publication.origin {
        // A LocalFile refresh is a targeted rescan, implemented as a full
        // scan filtered to this publication's outcome: a full disk scan is
        // cheap and reuses tested code.
        Origin::LocalFile { .. } => {
            let before = state.db.list_units(id).await?.len();
            crate::streamer::scan(&state.streamer, &state.db, None).await?;
            (state.db.list_units(id).await?.len().saturating_sub(before)) as u32
        }
        Origin::Source { .. } => {
            sync::refresh_publication(&state, &publication).await?.len() as u32
        }
    };
    Ok(Json(RefreshResponse {
        new_units,
        checked_at: chrono::Utc::now(),
    }))
}

/// Manual "Rescan files" from the More page.
pub async fn rescan(
    State(state): State<AppState>,
    _user: CurrentUser,
) -> Result<Json<RescanResponse>, ApiError> {
    // The background loop gates on this flag too; without the guard a manual
    // rescan on a disabled deployment would scan the default dir.
    if !state.config.books.enabled {
        return Err(ApiError::Unprocessable(
            "the books folder is disabled on this server".into(),
        ));
    }
    // Rescan announces new units (it acts as the local counterpart of the
    // periodic updater); per-publication refresh is interactive — the user is
    // looking at the result — so it passes no notifier.
    let notifier = crate::notifier::Notifier::new(state.config.notify.clone());
    let outcome = crate::streamer::scan(&state.streamer, &state.db, Some(&notifier)).await?;
    Ok(Json(RescanResponse {
        added: outcome.added,
        updated: outcome.updated,
        missing: outcome.missing,
    }))
}

/// Format-neutral reading order and supporting-resource graph for the
/// publication navigator.
pub async fn manifest(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let publication = state.db.get_publication(id).await?;
    let units = state.db.list_units(id).await?;
    let media_type = match publication.book_format() {
        None => {
            return Err(ApiError::Unprocessable(
                "Comics use the image-page API".into(),
            ));
        }
        Some(BookFormat::Epub) => "application/xhtml+xml",
        Some(BookFormat::Pdf) => "application/pdf",
        Some(_) => {
            return Err(ApiError::Unprocessable(
                "This book format is not readable yet; use an EPUB or PDF version".into(),
            ));
        }
    };
    let reading_order = units
        .into_iter()
        .map(|unit| PublicationLink {
            href: resource_href(&unit.source_key),
            media_type: media_type.into(),
            title: Some(unit.title),
            rel: None,
            unit_id: Some(unit.id),
        })
        .collect();
    let mut resources = match (&publication.origin, publication.book_format()) {
        (Origin::LocalFile { path }, Some(BookFormat::Epub)) => state
            .streamer
            .epub_resources(path)
            .await
            .map_err(super::error::local_file_err)?,
        _ => Vec::new(),
    };
    for link in &mut resources {
        link.href = resource_href(&link.href);
    }
    let manifest = PublicationManifest {
        context: "https://readium.org/webpub-manifest/context.jsonld".into(),
        metadata: PublicationMetadata {
            title: publication.title,
            kind: publication.kind,
        },
        links: vec![PublicationLink {
            href: format!("/api/v1/publications/{id}/manifest"),
            media_type: "application/webpub+json".into(),
            title: None,
            rel: Some("self".into()),
            unit_id: None,
        }],
        reading_order,
        resources,
    };
    Ok((
        [(header::CONTENT_TYPE, "application/webpub+json")],
        serde_json::to_vec(&manifest).map_err(|e| ApiError::Internal(e.to_string()))?,
    )
        .into_response())
}

fn resource_href(key: &str) -> String {
    let mut url = url::Url::parse("https://manifest.invalid/resources/-/").expect("static base");
    {
        let mut segments = url.path_segments_mut().expect("hierarchical base");
        segments.pop_if_empty();
        for part in key.split('/') {
            segments.push(part);
        }
    }
    url.path().trim_start_matches('/').to_string()
}

/// A resource from an EPUB container, or the original PDF. The capability is
/// a path segment (validated by the auth middleware) so relative EPUB assets
/// retain it automatically when the iframe requests images, fonts and CSS.
pub async fn resource(
    State(state): State<AppState>,
    Path((id, token, resource)): Path<(Uuid, String, String)>,
    request: Request,
) -> Result<Response, ApiError> {
    let publication = state.db.get_publication(id).await?;
    let format = publication.book_format().ok_or(ApiError::NotFound)?;
    let Origin::LocalFile { path } = publication.origin else {
        return Err(ApiError::NotFound);
    };
    if !matches!(publication.kind, Kind::Novels | Kind::Pdf) {
        return Err(ApiError::NotFound);
    }
    // Session-authenticated requests may use `-`; only an independently valid
    // path capability gets wildcard CORS for an opaque EPUB/PDF frame.
    let has_capability = token != "-" && state.media_key.verify(&token, 0).is_some();
    if format != BookFormat::Epub || resource == path {
        let path = state
            .streamer
            .book_path(&path, &resource)
            .map_err(super::error::local_file_err)?;
        let response = tower_http::services::ServeFile::new(path)
            .oneshot(request)
            .await
            .map_err(|error| ApiError::Internal(error.to_string()))?;
        let mut response = response.into_response();
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=3600"),
        );
        response.headers_mut().insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        );
        if format != BookFormat::Pdf {
            response.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment"),
            );
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
        }
        response.headers_mut().insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        allow_capability_cors(&mut response, has_capability);
        return Ok(response);
    }
    let data = state
        .streamer
        .publication_resource(&path, &resource)
        .await
        .map_err(super::error::local_file_err)?;
    let mut bytes = data.bytes.to_vec();
    let mut content_type = data.content_type;
    let nonce = crate::auth::new_token();
    let mut policy = "default-src 'none'; img-src 'self' data:; font-src 'self' data:; style-src 'self' 'unsafe-inline'; media-src 'self' data:;".to_string();
    let media_type = content_type
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or_default();
    if format == BookFormat::Epub
        && (media_type.eq_ignore_ascii_case("application/xhtml+xml")
            || media_type.eq_ignore_ascii_case("text/html"))
    {
        bytes = inject_epub_navigator(bytes, &nonce)?;
        policy.push_str(&format!(" script-src 'nonce-{nonce}'; sandbox allow-scripts; base-uri 'none'; form-action 'none';"));
        // HTML parsing is more tolerant of real-world EPUB markup than the
        // browser's XML parser, while the sandbox and CSP retain isolation.
        content_type = "text/html; charset=utf-8".into();
    }
    let content_type = HeaderValue::from_str(&content_type)
        .unwrap_or(HeaderValue::from_static("application/octet-stream"));
    let mut response = (
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=3600"),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_str(&policy).expect("generated CSP"),
            ),
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ),
        ],
        bytes,
    )
        .into_response();
    allow_capability_cors(&mut response, has_capability);
    Ok(response)
}

fn allow_capability_cors(response: &mut Response, has_capability: bool) {
    // A sandboxed EPUB has an opaque origin (Origin: null), including for
    // font fetches. A valid read-only capability is already the credential:
    // permit credential-free access without broadening cookie/API CORS.
    if has_capability {
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("accept-ranges, content-range, content-length"),
        );
    }
}

fn inject_epub_navigator(bytes: Vec<u8>, nonce: &str) -> Result<Vec<u8>, ApiError> {
    let mut html = String::from_utf8(bytes)
        .map_err(|_| ApiError::Unprocessable("EPUB document is not UTF-8".into()))?;
    let head = format!(
        "<style id=\"yomu-reader-style\">{}</style><script nonce=\"{nonce}\">{}\n{}</script>",
        include_str!("epub-reader.css"),
        include_str!("reader-interaction.js"),
        include_str!("epub-navigator.js"),
    );
    // Establish the reader layer and palette before publisher styles or the
    // first paint. Important reader colors must win even against publisher
    // !important rules, without stripping the book's typography or artwork.
    let lower = html.to_ascii_lowercase();
    if let Some(at) = lower
        .find("<head")
        .and_then(|at| lower[at..].find('>').map(|end| at + end + 1))
    {
        html.insert_str(at, &head);
    } else {
        html.insert_str(0, &head);
    }
    Ok(html.into_bytes())
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

    let mut publication = state.db.get_publication(id).await?;
    if publication.cover_url.is_none() {
        publication = state
            .db
            .book_editions(&publication)
            .await?
            .into_iter()
            .find(|version| version.cover_url.is_some() && version.missing_since.is_none())
            .ok_or(ApiError::NotFound)?;
    }
    let cover_url = publication.cover_url.ok_or(ApiError::NotFound)?;
    let image = match &publication.origin {
        Origin::LocalFile { .. } => state
            .streamer
            .image(&cover_url)
            .await
            .map_err(super::error::local_file_err)?,
        Origin::Source { source_id, .. } => {
            let source = state
                .sources
                .get(source_id)
                .ok_or_else(|| ApiError::Unprocessable("source no longer configured".into()))?;
            source.image(&cover_url).await?
        }
    };

    let ext = crate::downloader::extension_for(&image.content_type, &cover_url);
    let _ = tokio::fs::create_dir_all(&covers_dir).await;
    let path = covers_dir.join(format!("{id}.{ext}"));
    let _ = tokio::fs::write(&path, &image.bytes).await;

    Ok(cover_response(
        image.bytes.to_vec(),
        crate::downloader::content_type_for(&path),
    ))
}

pub(crate) fn cover_response(bytes: Vec<u8>, content_type: &'static str) -> Response {
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
