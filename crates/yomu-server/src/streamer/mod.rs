//! Server-side streamer: turns user-supplied comic files (CBZ archives,
//! image directories) in the configured books dir into library entries and
//! serves their pages. The scan half lands next; file resolution in `files`.

mod files;

#[expect(
    unused_imports,
    reason = "streamer (2.x) entry point; wired into AppState next"
)]
pub use files::Streamer;
