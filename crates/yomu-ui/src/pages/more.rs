//! More: theme picker, account, server details, backup/restore.

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos::wasm_bindgen::{JsCast, JsValue};

use crate::offline::{self, Theme};
use crate::use_client;

/// Trigger a browser download of `json` as `filename`.
fn download_json(filename: &str, json: &str) -> Result<(), JsValue> {
    let parts = js_sys::Array::new();
    parts.push(&JsValue::from_str(json));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("application/json");
    let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;
    let anchor = document()
        .create_element("a")?
        .dyn_into::<web_sys::HtmlAnchorElement>()?;
    anchor.set_href(&url);
    anchor.set_download(filename);
    anchor.click();
    web_sys::Url::revoke_object_url(&url)?;
    Ok(())
}

/// The browser tier cannot be repaired from here, and the report must not
/// pretend otherwise: pages saved in a browser live in the service worker's
/// cache under the page URL, and that URL contains the chapter id. Moving
/// them would mean copying every cached response to a new URL.
const BROWSER_CAVEAT: &str = "Chapters saved in a browser are not covered: \
    there each page is cached under its own address, which contains the \
    chapter id, so those have to be downloaded again.";

/// Find device downloads whose chapter id no longer exists, recognise them
/// by content, and re-key both the files and the mark.
///
/// The mark records which title a chapter belongs to and that id did not
/// change, so each orphan is only ever compared against the fingerprints of
/// its own publication. Nothing is guessed: a directory that matches two
/// current chapters, or none, is left exactly where it is and counted.
async fn recover_device_downloads(client: &yomu_client::YomuClient) -> String {
    use std::collections::{BTreeMap, BTreeSet};

    if !offline::shell_available() {
        return format!("Nothing to do here. {BROWSER_CAVEAT}");
    }
    let mut stored: BTreeSet<String> = match offline::shell_list_chapters().await {
        Ok(names) => names.into_iter().collect(),
        Err(err) => return format!("Could not read this device's storage: {err}"),
    };

    let marks = offline::device_chapters();
    let mut by_publication: BTreeMap<uuid::Uuid, Vec<uuid::Uuid>> = BTreeMap::new();
    let mut legacy = 0;
    for (unit, mark) in &marks {
        // A mark from before the manga id was recorded cannot be scoped to
        // a publication, and searching the whole library on content is not
        // a search, it is a coincidence waiting to happen. Counted so the
        // report still accounts for every chapter this device holds.
        if mark.manga.is_nil() {
            legacy += 1;
        } else {
            by_publication.entry(mark.manga).or_default().push(*unit);
        }
    }

    let (mut renamed, mut repaired, mut ambiguous, mut unmatched, mut unreachable) =
        (0, 0, 0, 0, 0);
    let mut failed = 0;
    let mut first_failure: Option<String> = None;
    for (publication, units) in by_publication {
        let Ok(detail) = client.publication(publication).await else {
            unreachable += 1;
            continue;
        };
        let current: BTreeMap<uuid::Uuid, Option<f64>> =
            detail.units.iter().map(|u| (u.id, u.number)).collect();

        // Before anything else: repair what a previous run left half-done.
        // A recovery is two writes — rename the directory, then re-key the
        // mark — and a device killed in between keeps the files under the
        // new id with the mark still on the old one. That mark can never be
        // matched by content again (its directory is gone), so without this
        // the chapter stays invisible and has to be downloaded a second
        // time. See `offline::plan_mark_repair` for the pairing rule.
        let stale: Vec<(uuid::Uuid, u32)> = units
            .iter()
            .filter(|unit| !current.contains_key(unit) && !stored.contains(&unit.to_string()))
            .filter_map(|unit| marks.get(unit).map(|mark| (*unit, mark.pages)))
            .collect();
        let mut repaired_marks: BTreeSet<uuid::Uuid> = BTreeSet::new();
        if !stale.is_empty() {
            let mut landed: Vec<(uuid::Uuid, u32)> = Vec::new();
            for unit in current.keys() {
                if stored.contains(&unit.to_string())
                    && !marks.contains_key(unit)
                    && let Ok(fingerprint) = offline::shell_chapter_fingerprint(*unit).await
                {
                    landed.push((*unit, fingerprint.page_count));
                }
            }
            for (from, to) in offline::plan_mark_repair(&stale, &landed) {
                offline::rekey_device_mark(from, to, current.get(&to).copied().flatten());
                repaired_marks.insert(from);
                repaired += 1;
            }
        }

        let orphans: Vec<uuid::Uuid> = units
            .into_iter()
            .filter(|unit| !current.contains_key(unit) && !repaired_marks.contains(unit))
            .collect();
        if orphans.is_empty() {
            continue;
        }
        let Ok(server) = client.fingerprints(publication).await else {
            unreachable += 1;
            continue;
        };
        let mut local = Vec::new();
        for unit in orphans {
            // A mark with no directory behind it has no content to match.
            if !stored.contains(&unit.to_string()) {
                unmatched += 1;
                continue;
            }
            match offline::shell_chapter_fingerprint(unit).await {
                Ok(fingerprint) => local.push((unit, fingerprint)),
                Err(_) => unmatched += 1,
            }
        }
        let plan = offline::plan_recovery(&local, &server.fingerprints, &stored);
        ambiguous += plan.ambiguous;
        unmatched += plan.unmatched;
        for (from, to) in plan.renames {
            match offline::shell_rename_chapter(from, to).await {
                Ok(()) => {
                    offline::rekey_device_mark(from, to, current.get(&to).copied().flatten());
                    // Keep the ground truth in step: what was matched
                    // against `stored` a moment ago has just moved.
                    stored.remove(&from.to_string());
                    stored.insert(to.to_string());
                    renamed += 1;
                }
                // Anything from a full disk to a permission error to a
                // directory that vanished under us. The files stay where
                // they are either way; the reason is reported rather than
                // guessed at.
                Err(err) => {
                    failed += 1;
                    first_failure.get_or_insert(err);
                }
            }
        }
    }

    let mut report = if renamed == 0 && repaired == 0 {
        "Found nothing to recover.".to_string()
    } else if renamed == 0 {
        String::new()
    } else {
        format!("Recovered {renamed} chapter(s).")
    };
    if repaired > 0 {
        if !report.is_empty() {
            report.push(' ');
        }
        report.push_str(&format!(
            "Re-attached {repaired} chapter(s) whose files an interrupted run had already moved."
        ));
    }
    if ambiguous > 0 {
        report.push_str(&format!(
            " {ambiguous} could not be told apart from another chapter and were left alone."
        ));
    }
    if unmatched > 0 {
        report.push_str(&format!(
            " {unmatched} matched nothing in the library and were left alone."
        ));
    }
    if legacy > 0 {
        report.push_str(&format!(
            " {legacy} were saved before this app recorded which title a chapter belongs to, \
             so there is no safe way to tell what they are; they were left alone."
        ));
    }
    if failed > 0 {
        let reason = first_failure
            .map(|err| format!(" (first error: {err})"))
            .unwrap_or_default();
        report.push_str(&format!(
            " {failed} could not be moved on this device and were left alone{reason}."
        ));
    }
    if unreachable > 0 {
        report.push_str(&format!(
            " {unreachable} title(s) could not be checked (server unreachable)."
        ));
    }
    report.push(' ');
    report.push_str(BROWSER_CAVEAT);
    report
}

#[component]
pub fn More() -> impl IntoView {
    let current = RwSignal::new(offline::theme());
    let pick = move |theme: Theme| {
        offline::set_theme(theme);
        current.set(theme);
    };

    let client = use_client();
    let health = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move { client.health().await }
        }
    });
    let base = client.base().to_string();

    // Backup: export downloads a JSON snapshot; restore reads one back and
    // merges it (additive — nothing already present is overwritten).
    let backup_status = RwSignal::new(None::<String>);
    let export = {
        let client = client.clone();
        move |_| {
            let client = client.clone();
            backup_status.set(Some("Preparing backup…".into()));
            spawn_local(async move {
                match client.backup().await {
                    Ok(backup) => match serde_json::to_string(&backup) {
                        Ok(json) => {
                            if download_json("yomu-backup.json", &json).is_ok() {
                                backup_status.set(Some(format!(
                                    "Exported {} titles.",
                                    backup.publications.len()
                                )));
                            } else {
                                backup_status.set(Some("Could not start the download.".into()));
                            }
                        }
                        Err(e) => backup_status.set(Some(format!("Export failed: {e}"))),
                    },
                    Err(e) => backup_status.set(Some(format!("Export failed: {e}"))),
                }
            });
        }
    };
    let import = {
        let client = client.clone();
        move |ev: leptos::ev::Event| {
            let Some(input) = ev
                .target()
                .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
            else {
                return;
            };
            let Some(file) = input.files().and_then(|f| f.get(0)) else {
                return;
            };
            // Let the same file be re-picked after a failed attempt.
            input.set_value("");
            let client = client.clone();
            backup_status.set(Some("Restoring…".into()));
            spawn_local(async move {
                let text = match wasm_bindgen_futures::JsFuture::from(file.text()).await {
                    Ok(v) => v.as_string().unwrap_or_default(),
                    Err(_) => {
                        backup_status.set(Some("Could not read the file.".into()));
                        return;
                    }
                };
                let backup = match serde_json::from_str::<yomu_domain::Backup>(&text) {
                    Ok(b) => b,
                    Err(e) => {
                        backup_status.set(Some(format!("Not a valid backup: {e}")));
                        return;
                    }
                };
                match client.restore(&backup).await {
                    Ok(s) => backup_status.set(Some(format!(
                        "Restored {} titles, {} chapters, {} read marks.",
                        s.publications, s.units, s.read_marks
                    ))),
                    Err(e) => backup_status.set(Some(format!("Restore failed: {e}"))),
                }
            });
        }
    };

    let rescan_status = RwSignal::new(None::<String>);
    let rescan = {
        let client = client.clone();
        move |_| {
            let client = client.clone();
            rescan_status.set(Some("Rescanning files…".into()));
            spawn_local(async move {
                match client.rescan().await {
                    Ok(r) => rescan_status.set(Some(format!(
                        "Scan done: {} added, {} updated, {} missing.",
                        r.added, r.updated, r.missing
                    ))),
                    Err(e) => rescan_status.set(Some(format!("Rescan failed: {e}"))),
                }
            });
        }
    };

    // Recovery for downloads orphaned by a source re-key: the files are
    // still here, under directory names the server no longer knows.
    let device_marks = crate::use_device_marks();
    let recover_status = RwSignal::new(None::<String>);
    let recover = {
        let client = client.clone();
        move |_| {
            let client = client.clone();
            recover_status.set(Some("Looking through saved chapters…".into()));
            spawn_local(async move {
                let report = recover_device_downloads(&client).await;
                device_marks.set(offline::device_chapters());
                recover_status.set(Some(report));
            });
        }
    };

    view! {
        <section class="more">
            <h2>"Settings"</h2>

            <h3 class="shelf-title">"Theme"</h3>
            <div class="theme-grid">
                {Theme::ALL
                    .into_iter()
                    .map(|theme| {
                        view! {
                            <button
                                class="theme-choice"
                                class:active=move || current.get() == theme
                                data-swatch=theme.key()
                                on:click=move |_| pick(theme)
                            >
                                <span class="swatch">
                                    <span class="swatch-accent"></span>
                                </span>
                                {theme.name()}
                            </button>
                        }
                    })
                    .collect_view()}
            </div>

            <h3 class="shelf-title">"Account"</h3>
            <p><crate::Account/></p>

            <h3 class="shelf-title">"Server"</h3>
            <p class="muted">
                {base} {" · "}
                {move || match health.get() {
                    Some(Ok(h)) => format!("yomu {} · {}", h.version, h.status),
                    Some(Err(_)) => "unreachable".into(),
                    None => "checking…".into(),
                }}
            </p>
            <crate::ConnectForm/>

            <h3 class="shelf-title">"Backup"</h3>
            <p class="muted">
                "Export your library, reading progress and read marks as a "
                "file, or restore one. Restoring merges — nothing you already "
                "have is overwritten."
            </p>
            <div class="backup-actions">
                <button class="button" on:click=export>"Export backup"</button>
                <label class="button">
                    "Restore backup"
                    <input
                        type="file"
                        accept="application/json,.json"
                        class="visually-hidden"
                        on:change=import
                    />
                </label>
            </div>
            {move || {
                backup_status
                    .get()
                    .map(|msg| view! { <p class="muted backup-status">{msg}</p> })
            }}

            <h3 class="shelf-title">"Files"</h3>
            <p class="muted">
                "Titles dropped into the server's books folder appear in the library "
                "automatically; rescan to pick up changes right away."
            </p>
            <div class="backup-actions">
                <button class="button" on:click=rescan>"Rescan files"</button>
            </div>
            {move || {
                rescan_status
                    .get()
                    .map(|msg| view! { <p class="muted backup-status">{msg}</p> })
            }}

            <p class="muted">
                "If a source changed its addresses, chapters saved on this device "
                "stop matching the library and read as not saved. This finds them "
                "again by their contents, and only when the match is certain."
            </p>
            <div class="backup-actions">
                <button class="button" on:click=recover>"Recover device downloads"</button>
            </div>
            {move || {
                recover_status
                    .get()
                    .map(|msg| view! { <p class="muted backup-status">{msg}</p> })
            }}

            <p class="home-more">
                <a href="/about">"About yomu →"</a>
            </p>
        </section>
    }
}
