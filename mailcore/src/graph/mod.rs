//! Microsoft Graph REST client and response types.
//!
//! `model` holds plain structs mirroring the Graph JSON fields lookxy uses
//! (mail folders, messages, attachment metadata, delta pages) plus
//! `from_json` constructors that parse them out of `crate::json::Value`.
//! The REST client that fetches these lives here too (a later task).

pub mod model;
