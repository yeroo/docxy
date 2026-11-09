//! mailcore — headless engine for lookxy.
//!
//! Modules: `json` (hand-rolled), `auth` (OAuth2 auth-code + PKCE), `graph`
//! (Microsoft Graph REST client), `store` (SQLite), `sync` (background engine).

pub mod auth;
pub mod compose_html;
pub mod graph;
pub mod htmlrender;
pub mod json;
pub mod pkce;
pub mod store;
pub mod sync;
pub mod thread;
pub mod tokencache;

/// In-process fake Graph server: test support for mailcore and for crates
/// built on it (#2054), so a client's own tests can drive the whole path.
#[doc(hidden)]
pub mod testserver;
