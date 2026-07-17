//! The sync engine — the single background thread that owns a `Store` and a
//! `GraphClient`, backfills + delta-syncs Graph into the store, drains the
//! local outbox queue to Graph, and talks to the UI over `mpsc` channels.
//!
//! `outbox::apply_op` is the thin per-op dispatch layer; `engine` is the
//! thread + loop that drives it (plus backfill, delta, auth refresh,
//! throttling, and offline back-off).

pub mod engine;
pub mod outbox;
