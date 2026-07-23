//! Server-side streamer: turns user-supplied comic files (CBZ archives,
//! image directories) in the configured books dir into library entries and
//! serves their pages. The scan half lands next; file resolution in `files`.

mod files;

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use yomu_domain::Origin;

pub use files::Streamer;

use crate::db::{Db, DbError};
use crate::notifier::Notifier;

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error(transparent)]
    Db(#[from] DbError),
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ScanOutcome {
    pub added: u32,
    /// Known publications that changed: new units found or path re-pointed.
    pub updated: u32,
    /// Publications newly flagged missing by this scan.
    pub missing: u32,
}

/// One full scan of the books dir: upsert publications and units, feed the
/// updates feed (and ntfy when a notifier is passed) for new units in known
/// publications, flag vanished files, self-heal unambiguous renames.
/// Never destructive: rows and progress always survive.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "streamer (2.x) scan; wired into AppState next")
)]
pub async fn scan(
    streamer: &Streamer,
    db: &Db,
    notifier: Option<&Notifier>,
) -> Result<ScanOutcome, ScanError> {
    let discovered = streamer.discover().await;
    let existing = db.list_local_publications().await?;
    let by_path: HashMap<&str, &yomu_domain::Publication> = existing
        .iter()
        .filter_map(|p| match &p.origin {
            Origin::LocalFile { path } => Some((path.as_str(), p)),
            Origin::Source { .. } => None,
        })
        .collect();

    let mut outcome = ScanOutcome::default();
    let discovered_paths: HashSet<&str> = discovered.iter().map(|d| d.path.as_str()).collect();
    let mut seen = HashSet::new();

    for found in &discovered {
        seen.insert(found.path.clone());
        if let Some(publication) = by_path.get(found.path.as_str()).copied() {
            let changed = sync_known(db, notifier, publication, found).await?;
            if changed {
                outcome.updated += 1;
            }
            continue;
        }

        // New path. Before inserting, an unambiguous title match against a
        // *missing* publication (already flagged, or vanishing in this very
        // scan) is the same book renamed on disk: re-point it so ids (and
        // progress) survive. Two candidates → never guess.
        let candidates: Vec<_> = existing
            .iter()
            .filter(|p| p.title == found.details.summary.title)
            .filter(|p| match &p.origin {
                Origin::LocalFile { path } => {
                    !discovered_paths.contains(path.as_str()) && !seen.contains(path)
                }
                Origin::Source { .. } => false,
            })
            .collect();
        match candidates.as_slice() {
            [only] => {
                db.repoint_local_publication(only.id, &found.path).await?;
                // sync_units re-keys units by number/title twin-matching.
                db.sync_units(only.id, &found.details.chapters).await?;
                db.update_local_metadata(
                    only.id,
                    found.details.description.as_deref(),
                    found.details.summary.cover_url.as_deref(),
                )
                .await?;
                if let Origin::LocalFile { path } = &only.origin {
                    seen.insert(path.clone());
                }
                outcome.updated += 1;
            }
            _ => match db
                .insert_local_publication(&found.path, &found.details)
                .await
            {
                Ok(_) => outcome.added += 1,
                Err(DbError::Constraint(err)) => {
                    tracing::warn!(path = %found.path, %err, "streamer: insert skipped");
                }
                Err(err) => return Err(err.into()),
            },
        }
    }

    // Anything known that the walk didn't see has vanished from disk.
    for publication in &existing {
        let Origin::LocalFile { path } = &publication.origin else {
            continue;
        };
        if !seen.contains(path) && publication.missing_since.is_none() {
            db.set_missing_since(publication.id, Some(Utc::now()))
                .await?;
            outcome.missing += 1;
            tracing::info!(title = %publication.title, "streamer: file missing, flagged");
        }
    }

    Ok(outcome)
}

/// Re-sync a known publication: new units feed the updates feed + ntfy
/// (the rescan is the local updater), a cleared missing flag heals.
async fn sync_known(
    db: &Db,
    notifier: Option<&Notifier>,
    publication: &yomu_domain::Publication,
    found: &files::Discovered,
) -> Result<bool, ScanError> {
    let sync = db
        .sync_units(publication.id, &found.details.chapters)
        .await?;
    db.update_local_metadata(
        publication.id,
        found.details.description.as_deref(),
        found.details.summary.cover_url.as_deref(),
    )
    .await?;
    db.set_genres(publication.id, &found.details.genres).await?;
    let mut changed = false;
    if publication.missing_since.is_some() {
        db.set_missing_since(publication.id, None).await?;
        changed = true;
    }
    if !sync.new_units.is_empty() {
        db.add_update(publication.id, &sync.new_units).await?;
        if let Some(notifier) = notifier {
            notifier
                .notify_new_units(&publication.title, &sync.new_units)
                .await;
        }
        changed = true;
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use yomu_domain::Origin;

    struct Fixture {
        root: std::path::PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!("yomu-scan-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn page(&self, rel: &str) {
            let path = self.root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"png").unwrap();
        }

        fn cbz(&self, rel: &str, entries: &[&str]) {
            let path = self.root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let file = std::fs::File::create(path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options: zip::write::SimpleFileOptions = Default::default();
            for entry in entries {
                use std::io::Write;
                zip.start_file(*entry, options).unwrap();
                zip.write_all(b"png").unwrap();
            }
            zip.finish().unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn scan_discovers_series_root_cbz_and_skips_unsupported() {
        let fx = Fixture::new("discover");
        fx.page("Solo Farming/Chapter 1/001.png");
        fx.page("Solo Farming/Chapter 1/002.png");
        fx.cbz("Solo Farming/Chapter 2.cbz", &["001.png"]);
        fx.page("Solo Farming/cover.png");
        fx.cbz("One Shot.cbz", &["p1.png", "p2.png"]);
        fx.page("Loose Pages/001.png");
        std::fs::write(fx.root.join("novel.epub"), b"nope").unwrap();
        std::fs::write(fx.root.join("broken.cbz"), b"not a zip").unwrap();

        let db = Db::in_memory().await.unwrap();
        let streamer = Streamer::new(fx.root.clone());
        let outcome = scan(&streamer, &db, None).await.unwrap();
        // Solo Farming + One Shot + Loose Pages; epub skipped, corrupt cbz
        // skipped with a warning, neither aborts the scan.
        assert_eq!((outcome.added, outcome.missing), (3, 0));

        let pubs = db.list_local_publications().await.unwrap();
        assert_eq!(pubs.len(), 3);
        let solo = pubs.iter().find(|p| p.title == "Solo Farming").unwrap();
        assert_eq!(
            solo.origin,
            Origin::LocalFile {
                path: "Solo Farming".into()
            }
        );
        assert_eq!(db.list_units(solo.id).await.unwrap().len(), 2);
        let one_shot = pubs.iter().find(|p| p.title == "One Shot").unwrap();
        assert_eq!(db.list_units(one_shot.id).await.unwrap().len(), 1);

        // Idempotent: nothing new the second time.
        let again = scan(&streamer, &db, None).await.unwrap();
        assert_eq!((again.added, again.updated, again.missing), (0, 0, 0));
    }

    #[tokio::test]
    async fn new_units_in_known_publications_feed_updates() {
        let fx = Fixture::new("updates");
        fx.page("Series/Chapter 1/001.png");
        let db = Db::in_memory().await.unwrap();
        let streamer = Streamer::new(fx.root.clone());
        scan(&streamer, &db, None).await.unwrap();
        // The initial add must NOT announce a backlog.
        assert!(
            db.updates_since(chrono::DateTime::<chrono::Utc>::MIN_UTC, 100)
                .await
                .unwrap()
                .is_empty()
        );

        fx.page("Series/Chapter 2/001.png");
        let outcome = scan(&streamer, &db, None).await.unwrap();
        assert_eq!(outcome.updated, 1);
        let updates = db
            .updates_since(chrono::DateTime::<chrono::Utc>::MIN_UTC, 100)
            .await
            .unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].unit_count, 1);
    }

    #[tokio::test]
    async fn vanished_files_flag_missing_and_reappearing_clears() {
        let fx = Fixture::new("missing");
        fx.page("Series/Chapter 1/001.png");
        let db = Db::in_memory().await.unwrap();
        let streamer = Streamer::new(fx.root.clone());
        scan(&streamer, &db, None).await.unwrap();

        std::fs::rename(fx.root.join("Series"), fx.root.join(".hidden-away")).unwrap();
        let outcome = scan(&streamer, &db, None).await.unwrap();
        assert_eq!(outcome.missing, 1);
        let p = &db.list_local_publications().await.unwrap()[0];
        assert!(p.missing_since.is_some());
        // Progress-carrying row survives; re-flagging is not double-counted.
        assert_eq!(scan(&streamer, &db, None).await.unwrap().missing, 0);

        std::fs::rename(fx.root.join(".hidden-away"), fx.root.join("Series")).unwrap();
        scan(&streamer, &db, None).await.unwrap();
        assert!(
            db.list_local_publications().await.unwrap()[0]
                .missing_since
                .is_none()
        );
    }

    #[tokio::test]
    async fn rename_self_heals_by_unique_title_only() {
        let fx = Fixture::new("heal");
        fx.page("Old Name/Chapter 1/001.png");
        std::fs::write(
            fx.root.join("Old Name/details.json"),
            br#"{"title": "Kept Title"}"#,
        )
        .unwrap();
        let db = Db::in_memory().await.unwrap();
        let streamer = Streamer::new(fx.root.clone());
        scan(&streamer, &db, None).await.unwrap();
        let original = db.list_local_publications().await.unwrap()[0].clone();

        // Rename the dir, keep the details title: unique missing-title match.
        std::fs::rename(fx.root.join("Old Name"), fx.root.join("New Name")).unwrap();
        std::fs::write(
            fx.root.join("New Name/details.json"),
            br#"{"title": "Kept Title"}"#,
        )
        .unwrap();
        let outcome = scan(&streamer, &db, None).await.unwrap();
        assert_eq!((outcome.added, outcome.updated, outcome.missing), (0, 1, 0));
        let healed = db.list_local_publications().await.unwrap();
        assert_eq!(healed.len(), 1, "re-pointed, not duplicated");
        assert_eq!(healed[0].id, original.id, "id (and thus progress) survives");
        assert_eq!(
            healed[0].origin,
            Origin::LocalFile {
                path: "New Name".into()
            }
        );
        assert!(healed[0].missing_since.is_none());
    }

    #[tokio::test]
    async fn ambiguous_title_match_never_guesses() {
        let fx = Fixture::new("ambiguous");
        fx.page("A/Chapter 1/001.png");
        fx.page("B/Chapter 1/001.png");
        std::fs::write(fx.root.join("A/details.json"), br#"{"title": "Same"}"#).unwrap();
        std::fs::write(fx.root.join("B/details.json"), br#"{"title": "Same"}"#).unwrap();
        let db = Db::in_memory().await.unwrap();
        let streamer = Streamer::new(fx.root.clone());
        scan(&streamer, &db, None).await.unwrap();

        std::fs::rename(fx.root.join("A"), fx.root.join(".gone-a")).unwrap();
        std::fs::rename(fx.root.join("B"), fx.root.join(".gone-b")).unwrap();
        scan(&streamer, &db, None).await.unwrap();
        fx.page("C/Chapter 1/001.png");
        std::fs::write(fx.root.join("C/details.json"), br#"{"title": "Same"}"#).unwrap();
        let outcome = scan(&streamer, &db, None).await.unwrap();
        // Two missing candidates share the title: C is a NEW publication and
        // both stay flagged.
        assert_eq!(outcome.added, 1);
        let pubs = db.list_local_publications().await.unwrap();
        assert_eq!(pubs.len(), 3);
        assert_eq!(pubs.iter().filter(|p| p.missing_since.is_some()).count(), 2);
    }
}
