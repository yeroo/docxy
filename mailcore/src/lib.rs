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
pub mod tokencache;

#[cfg(test)]
mod testserver;
