//! Shared Leptos UI, mounted by the web bundle (and later by a desktop
//! shell). Platform specifics are injected via [`AppConfig`], same seam as
//! chaos.

mod cache;
mod chapter_actions;
mod cover;
mod format;
mod notify;
pub mod offline;
mod pager;
mod pages;
mod pull;

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::{A, Route, Router, Routes};
use leptos_router::path;
use url::Url;
use yomu_client::YomuClient;

#[derive(Clone)]
pub struct AppConfig {
    pub api_base: Url,
}

pub fn use_client() -> YomuClient {
    let config = use_context::<AppConfig>().expect("AppConfig provided by the shell");
    YomuClient::new(config.api_base)
}

/// App-wide connectivity to the configured server. `Offline` puts every
/// cached read (see [`offline::cached`]) in cache-first mode — no network
/// until the user retries from the offline badge (or the browser fires
/// `online`). `Checking` is a probe in flight: reads stay cache-first so a
/// retry doesn't stampede requests before the probe decides.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Connectivity {
    Checking,
    Online,
    Offline,
}

pub fn use_connectivity() -> RwSignal<Connectivity> {
    use_context().expect("Connectivity provided by App")
}

/// How long to wait for a health probe to land before assuming its task
/// will never resume. The client's own deadline is 3s (`HEALTH_TIMEOUT`),
/// enforced with an AbortController — but a suspended Android WebView can
/// freeze the task *and* its abort timer, so this is the outer bound.
const PROBE_WATCHDOG: std::time::Duration = std::time::Duration::from_secs(6);

/// Serializes the health probe behind a single in-flight slot, so the
/// badge, the resume check and the `online` event can all ask for one
/// without stampeding the server.
///
/// A probe that never lands used to wedge the app: `Checking` is sticky,
/// the badge refused to retry from it, and only killing the app cleared it.
/// The watchdog closes that hole — the slot is released and the verdict
/// falls back to `Offline`, so the next tap (or simply switching back to
/// the app) tries again. A late answer from a superseded probe is dropped
/// on the generation counter.
#[derive(Clone, Copy)]
pub(crate) struct Probe {
    conn: RwSignal<Connectivity>,
    in_flight: RwSignal<bool>,
    generation: RwSignal<u32>,
}

impl Probe {
    fn new(conn: RwSignal<Connectivity>) -> Self {
        Self {
            conn,
            in_flight: RwSignal::new(false),
            generation: RwSignal::new(0),
        }
    }

    /// Run one probe unless another is genuinely in flight.
    fn spawn(self, client: YomuClient) {
        if self.in_flight.get_untracked() {
            return;
        }
        let generation = self.generation.get_untracked().wrapping_add(1);
        self.generation.set(generation);
        self.in_flight.set(true);
        self.conn.set(Connectivity::Checking);

        set_timeout(
            move || {
                if self.generation.get_untracked() == generation && self.in_flight.get_untracked() {
                    self.settle(generation, false);
                }
            },
            PROBE_WATCHDOG,
        );

        spawn_local(async move {
            let reachable = client.health().await.is_ok();
            if reachable {
                offline::mark_server_seen(client.base().as_str());
            }
            self.settle(generation, reachable);
        });
    }

    fn settle(self, generation: u32, reachable: bool) {
        if self.generation.get_untracked() != generation {
            return; // superseded by a newer probe; its verdict is the one that counts
        }
        self.in_flight.set(false);
        self.conn.set(if reachable {
            Connectivity::Online
        } else {
            Connectivity::Offline
        });
    }
}

pub(crate) fn use_probe() -> Probe {
    use_context().expect("Probe provided by App")
}

/// One in-flight local (device) save, shown on the manga page ring and
/// in the Downloads tab's device section. Keyed by chapter id in the
/// `LocalDownloads` map.
#[derive(Clone, PartialEq, Debug)]
pub struct LocalDownload {
    pub manga_id: uuid::Uuid,
    pub manga_title: String,
    pub chapter_title: String,
    pub done: u32,
    pub total: u32,
    pub failed: bool,
    pub cancel_requested: bool,
}

pub type LocalDownloads = RwSignal<std::collections::HashMap<uuid::Uuid, LocalDownload>>;

/// Reactive mirror of the device-saved-chapter marks (localStorage), so a
/// row flips to its on-device style the instant a save completes.
pub type DeviceMarks = RwSignal<std::collections::BTreeMap<uuid::Uuid, crate::offline::DeviceMark>>;

pub fn use_local_downloads() -> LocalDownloads {
    use_context().expect("LocalDownloads provided by App")
}

pub fn use_device_marks() -> DeviceMarks {
    use_context().expect("DeviceMarks provided by App")
}

/// One chapter queued to pull to this device once its server download
/// finishes ("download both"). Ordered oldest-first; persisted so it
/// survives navigation and app restarts.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PullItem {
    pub chapter_id: uuid::Uuid,
    pub manga_id: uuid::Uuid,
    pub manga_title: String,
    pub chapter_title: String,
}

pub type PullQueue = RwSignal<Vec<PullItem>>;

pub fn use_pull_queue() -> PullQueue {
    use_context().expect("PullQueue provided by App")
}

#[component]
pub fn App(config: AppConfig) -> impl IntoView {
    provide_context(config.clone());
    let conn = RwSignal::new(Connectivity::Checking);
    provide_context(conn);
    let probe = Probe::new(conn);
    provide_context(probe);
    let local_downloads: LocalDownloads = RwSignal::new(std::collections::HashMap::new());
    provide_context(local_downloads);
    let device_marks: DeviceMarks = RwSignal::new(offline::device_chapters());
    provide_context(device_marks);
    let pull_queue: PullQueue = RwSignal::new(offline::load_pull_queue());
    provide_context(pull_queue);
    // Page data that outlives the page component, so returning to a list
    // is free (see cache.rs). Library and Home share one, since they read
    // the same library() list.
    provide_context(cache::LibraryCache::default());
    provide_context(cache::CategoriesCache::default());
    provide_context(cache::DetailCache::default());
    // Write-through: persist any change so the queue survives restarts.
    Effect::new(move |_| {
        pull_queue.with(|q| offline::save_pull_queue(q));
    });
    offline::apply_theme(offline::theme());

    // Whenever the server (re)becomes reachable, sync progress and read
    // marks recorded while it wasn't. Covers startup (the boot gate flips
    // to Online) and every later recovery, badge retries included.
    let flush_client = YomuClient::new(config.api_base.clone());
    let flush_library = cache::use_library_cache();
    let flush_detail = cache::use_detail_cache();
    Effect::new(move |_| {
        if conn.get() != Connectivity::Online {
            return;
        }
        let client = flush_client.clone();
        spawn_local(async move {
            // The caches travel in: inside this task a context read has no
            // reactive owner and would silently return None.
            offline::flush_outbox(&client, flush_library, flush_detail).await;
            offline::flush_marks(&client, flush_library, flush_detail).await;
        });
    });
    // The OS says a network came back: one free probe.
    let probe_client = YomuClient::new(config.api_base.clone());
    let online_handle = window_event_listener(leptos::ev::online, move |_| {
        if conn.get_untracked() != Connectivity::Online {
            probe.spawn(probe_client.clone());
        }
    });
    on_cleanup(move || online_handle.remove());

    // Coming back to the app: probe unless we already know we're online.
    //
    // This is the recovery path that matters on a phone. `online` never
    // fires for a VPN that dozed with the app (the OS network never
    // changed), so before this the only way back was tapping the badge —
    // and a probe frozen mid-suspend could leave a `Checking` no tap could
    // clear. Returning to the app now always re-decides.
    //
    // Bound on the document, where `visibilitychange` is dispatched, rather
    // than relying on it bubbling to the window.
    let resume_client = YomuClient::new(config.api_base.clone());
    if let Some(document) = web_sys::window().and_then(|w| w.document()) {
        let on_visible = leptos::wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
            if offline::document_hidden() {
                return;
            }
            if offline::should_probe_on_resume(conn.get_untracked()) {
                probe.spawn(resume_client.clone());
            }
        });
        use leptos::wasm_bindgen::JsCast;
        let _ = document.add_event_listener_with_callback(
            "visibilitychange",
            on_visible.as_ref().unchecked_ref(),
        );
        on_visible.forget(); // lives for the whole app, like the pull driver's tick
    }

    // Shell update notifications: poll the server's updates feed while
    // the app is alive (see notify.rs; Android also polls app-off via
    // WorkManager).
    if offline::shell_available() {
        notify::start(conn, YomuClient::new(config.api_base.clone()));
    }

    // Drain the device-pull queue app-wide, so "download both" completes
    // even after leaving the manga page (or restarting).
    pull::start(
        conn,
        YomuClient::new(config.api_base.clone()),
        pull_queue,
        local_downloads,
        device_marks,
    );

    view! {
        <ServerGate>
            <OfflineBadge/>
            <Router>
                <nav class="topbar">
                    <span class="brand">"yomu"</span>
                    <A href="/">"Home"</A>
                    <A href="/library">"Library"</A>
                    <A href="/sources">"Sources"</A>
                    <A href="/search">"Search"</A>
                    <A href="/downloads">"Downloads"</A>
                    <A href="/more">"More"</A>
                    <span class="grow"></span>
                    <Account/>
                </nav>
                <main>
                    <Routes fallback=|| view! { <p class="muted">"Page not found"</p> }>
                        <Route path=path!("/") view=pages::Home/>
                        <Route path=path!("/library") view=pages::Library/>
                        <Route path=path!("/search") view=pages::Search/>
                        <Route path=path!("/sources") view=pages::Sources/>
                        <Route path=path!("/sources/:source") view=pages::SourceCatalog/>
                        <Route path=path!("/downloads") view=pages::Downloads/>
                        <Route path=path!("/more") view=pages::More/>
                        <Route path=path!("/about") view=pages::About/>
                        <Route path=path!("/manga/:id") view=pages::MangaPage/>
                        <Route path=path!("/read/:manga/:chapter") view=pages::Reader/>
                    </Routes>
                </main>
                // Phone navigation: the topbar collapses to this fixed tab
                // bar under 40rem (see styles.css).
                <nav class="tabbar">
                    <A href="/"><span class="tab-icon">"⌂"</span>"Home"</A>
                    <A href="/library"><span class="tab-icon">"▦"</span>"Library"</A>
                    <A href="/sources"><span class="tab-icon">"⛁"</span>"Sources"</A>
                    <A href="/search"><span class="tab-icon">"⌕"</span>"Search"</A>
                    <A href="/downloads"><span class="tab-icon">"↓"</span>"Downloads"</A>
                    <A href="/more"><span class="tab-icon">"≡"</span>"More"</A>
                </nav>
            </Router>
        </ServerGate>
    }
}

#[derive(Clone, Copy, PartialEq)]
enum GateState {
    Checking,
    /// Server answered, or answered in the past (then `Connectivity` is
    /// `Offline` and the badge shows): render normally.
    Ready,
    /// Server unreachable and never reached from this address: genuine
    /// first-run or a wrong address — show the connect form.
    Unreachable,
}

/// Blocks the app behind one bounded health check (3 s — see the client's
/// probe timeout) so a shell (Tauri, or a fresh PWA install) pointing at
/// the wrong place gets a "connect to your server" form instead of a wall
/// of failed requests. The chosen URL is the `yomu-api-base` localStorage
/// override the API-base resolution already honors.
///
/// The decision is "have we ever reached *this* address?", not
/// `navigator.onLine`: away from a self-hosted server the device still has
/// connectivity (`onLine` is true) with no route home, and blocking there
/// would hide the downloaded library. A server that answered before is
/// treated as merely offline — the gate opens onto the cached UI and the
/// offline badge takes over; one that never answered is treated as
/// misconfigured (connect form).
#[component]
fn ServerGate(children: ChildrenFn) -> impl IntoView {
    let gate = RwSignal::new(GateState::Checking);
    let conn = use_connectivity();
    let client = use_client();
    let base = client.base().to_string();
    // Same watchdog as `Probe`, for the same reason: a boot probe that never
    // lands must not leave the app on "Connecting…" with no way out but a
    // force-restart. A server we have reached before opens the gate onto
    // the cached library; one we never have gets the connect form.
    {
        let base = base.clone();
        set_timeout(
            move || {
                if gate.get_untracked() == GateState::Checking {
                    conn.set(Connectivity::Offline);
                    gate.set(if offline::server_seen(&base) {
                        GateState::Ready
                    } else {
                        GateState::Unreachable
                    });
                }
            },
            PROBE_WATCHDOG,
        );
    }
    spawn_local(async move {
        match client.health().await {
            Ok(_) => {
                offline::mark_server_seen(&base);
                conn.set(Connectivity::Online);
                gate.set(GateState::Ready);
            }
            Err(_) if offline::server_seen(&base) => {
                conn.set(Connectivity::Offline);
                gate.set(GateState::Ready);
            }
            Err(_) => {
                conn.set(Connectivity::Offline);
                gate.set(GateState::Unreachable);
            }
        }
    });

    view! {
        {move || match gate.get() {
            GateState::Checking => view! { <p class="muted gate-msg">"Connecting…"</p> }.into_any(),
            GateState::Ready => children().into_any(),
            GateState::Unreachable => {
                view! {
                    <section class="server-gate">
                        <h2>"Cannot reach the yomu server"</h2>
                        <p class="muted">
                            "Enter the address of your server (for example "
                            <code>"http://192.168.1.128:4700"</code> ")."
                        </p>
                        <ConnectForm>
                            <button on:click=move |_| gate.set(GateState::Ready)>
                                "Continue anyway"
                            </button>
                        </ConnectForm>
                    </section>
                }
                    .into_any()
            }
        }}
    }
}

/// The offline indicator and the way back: shown whenever the server isn't
/// known to be reachable, tapping it runs one bounded health probe. On
/// success every open view refreshes by itself (resources track
/// [`Connectivity`]) and queued progress flushes (see `App`).
#[component]
fn OfflineBadge() -> impl IntoView {
    let conn = use_connectivity();
    // One "still offline" flash after a failed retry, cleared on a timer.
    let flash = RwSignal::new(false);
    let client = use_client();
    let probe = use_probe();
    // Tappable from `Checking` too: that state is not proof a probe is
    // still alive (see `Probe`), and a tap that does nothing reads as a
    // broken app.
    // Armed by a tap only: the flash answers the user's retry, so a probe
    // they didn't ask for (boot, resume) must not produce one.
    let armed = RwSignal::new(false);
    let retry = move |_| {
        armed.set(true);
        flash.set(false);
        probe.spawn(client.clone());
    };
    Effect::new(move |_| {
        if conn.get() == Connectivity::Offline && armed.get_untracked() {
            armed.set(false);
            flash.set(true);
            set_timeout(
                move || flash.set(false),
                std::time::Duration::from_millis(1800),
            );
        }
    });

    view! {
        {move || {
            (conn.get() != Connectivity::Online)
                .then(|| {
                    view! {
                        <button
                            class="offline-banner"
                            title="The server is unreachable — showing saved content. Tap to retry."
                            on:click=retry.clone()
                        >
                            {move || match conn.get() {
                                Connectivity::Checking => "connecting…",
                                _ if flash.get() => "still offline",
                                _ => "offline — retry",
                            }}
                        </button>
                    }
                })
        }}
    }
}

/// Server address form: stores the `yomu-api-base` override the API-base
/// resolution honors and reloads. Used by the startup gate and by the
/// More page (so an offline reader has somewhere to point at a server).
#[component]
pub(crate) fn ConnectForm(#[prop(optional)] children: Option<ChildrenFn>) -> impl IntoView {
    let current = use_client().base().to_string();
    let input = RwSignal::new(current.clone());
    let save = move |_| {
        let value = input.get_untracked();
        if Url::parse(&value).is_err() {
            return;
        }
        if let Some(window) = web_sys::window() {
            if let Ok(Some(storage)) = window.local_storage() {
                let _ = storage.set_item("yomu-api-base", &value);
            }
            let _ = window.location().reload();
        }
    };

    view! {
        <div class="gate-form">
            <input
                type="url"
                prop:value=move || input.get()
                on:input=move |ev| input.set(event_target_value(&ev))
            />
            <button class="primary" on:click=save>
                "Connect"
            </button>
            {children.map(|c| c())}
        </div>
    }
}

/// Topbar account widget. Only relevant when the server runs an OIDC
/// provider: single-account mode shows nothing (there is no one to sign
/// in as). Sign-in is a full-page redirect through the provider.
#[component]
pub(crate) fn Account() -> impl IntoView {
    let client = use_client();
    let me = LocalResource::new({
        let client = client.clone();
        move || {
            let client = client.clone();
            async move { client.me().await }
        }
    });

    let logout_client = client.clone();
    let sign_out = move |_| {
        let client = logout_client.clone();
        spawn_local(async move {
            let _ = client.logout().await;
            if let Some(window) = web_sys::window() {
                let _ = window.location().set_href("/");
            }
        });
    };

    view! {
        {move || {
            let sign_out = sign_out.clone();
            let me = me.get().and_then(|r| r.ok())?;
            match (me.mode, me.user) {
                (yomu_domain::AuthMode::Oidc, Some(user)) => {
                    Some(
                        view! {
                            <span class="account">
                                <span class="muted">{user.display_name}</span>
                                <button class="account-btn" on:click=sign_out>
                                    "sign out"
                                </button>
                            </span>
                        }
                            .into_any(),
                    )
                }
                (yomu_domain::AuthMode::Oidc, None) => {
                    // Full document navigation (the endpoint 302s to the
                    // provider), against the API base so a desktop shell
                    // pointing at a remote server signs in there.
                    let href = use_client()
                        .base()
                        .join("api/v1/auth/login")
                        .map(|u| u.to_string())
                        .unwrap_or_else(|_| "/api/v1/auth/login".into());
                    Some(
                        view! {
                            <a class="button" rel="external" href=href>
                                "Sign in"
                            </a>
                        }
                            .into_any(),
                    )
                }
                (yomu_domain::AuthMode::Single, _) => None,
            }
        }}
    }
}
