//! mailcore — headless engine for lookxy.
//!
//! Modules: `json` (hand-rolled), `auth` (OAuth2 auth-code + PKCE), `graph`
//! (Microsoft Graph REST client), `store` (SQLite), `sync` (background engine).

pub mod auth;
pub mod json;
pub mod pkce;

#[cfg(test)]
mod testserver;
