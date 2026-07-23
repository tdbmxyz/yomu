//! Shared domain types for yomu.
//!
//! Single source of truth for everything crossing the wire between
//! `yomu-server` and its clients (web now; native app with offline store
//! later). Compiles on native and wasm: no I/O, no async runtime, no
//! framework dependencies.

pub mod api;
pub mod auth;
pub mod backup;
pub mod progress;
pub mod publication;
pub mod source;

pub use api::*;
pub use auth::*;
pub use backup::*;
pub use progress::*;
pub use publication::*;
pub use source::*;
