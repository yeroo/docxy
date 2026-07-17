//! The SQLite schema for the local mail store (spec §5).
//!
//! `SCHEMA_SQL` creates every table the store will ever need — `folders`,
//! `messages`, `bodies`, `attachments`, `outbox`, the `messages_fts` FTS5
//! index (kept in sync by triggers), and `meta` — even though this task
//! (Task 8) only exercises `folders`/`messages`/`meta`. Doing it all now
//! means later tasks (bodies, attachments, outbox, search) never have to
//! run a migration; they just start using tables that already exist.
//!
//! Every statement is idempotent (`IF NOT EXISTS` / `INSERT OR IGNORE`) so
//! `open` can run this against an already-initialized database without
//! erroring.

pub(crate) const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS folders (
    id              TEXT PRIMARY KEY,
    parent_id       TEXT,
    display_name    TEXT NOT NULL,
    total_count     INTEGER NOT NULL DEFAULT 0,
    unread_count    INTEGER NOT NULL DEFAULT 0,
    delta_link      TEXT,
    well_known_name TEXT,
    sort_order      INTEGER
);

CREATE TABLE IF NOT EXISTS messages (
    id              TEXT PRIMARY KEY,
    folder_id       TEXT NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    conversation_id TEXT NOT NULL DEFAULT '',
    subject         TEXT NOT NULL DEFAULT '',
    from_name       TEXT NOT NULL DEFAULT '',
    from_addr       TEXT NOT NULL DEFAULT '',
    to_recipients   TEXT NOT NULL DEFAULT '',
    cc_recipients   TEXT NOT NULL DEFAULT '',
    received_at     TEXT NOT NULL DEFAULT '',
    sent_at         TEXT NOT NULL DEFAULT '',
    is_read         INTEGER NOT NULL DEFAULT 0,
    is_flagged      INTEGER NOT NULL DEFAULT 0,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    importance      TEXT NOT NULL DEFAULT '',
    preview         TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS idx_messages_folder_received
    ON messages(folder_id, received_at DESC);

CREATE TABLE IF NOT EXISTS bodies (
    message_id   TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    content_type TEXT NOT NULL DEFAULT '',
    content      TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS attachments (
    id           TEXT NOT NULL,
    message_id   TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    name         TEXT NOT NULL DEFAULT '',
    content_type TEXT NOT NULL DEFAULT '',
    size         INTEGER NOT NULL DEFAULT 0,
    is_inline    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (message_id, id)
);

CREATE TABLE IF NOT EXISTS outbox (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    op         TEXT NOT NULL,
    message_id TEXT,
    payload    TEXT NOT NULL DEFAULT '',
    attempts   INTEGER NOT NULL DEFAULT 0,
    last_error TEXT
);

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '1');

-- Full-text index over subject/sender/body, kept in step with `messages`
-- and `bodies` by the triggers below (Task 9 exercises search itself).
CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
    message_id UNINDEXED,
    subject,
    from_text,
    body
);

CREATE TRIGGER IF NOT EXISTS messages_fts_ai AFTER INSERT ON messages BEGIN
    DELETE FROM messages_fts WHERE message_id = new.id;
    INSERT INTO messages_fts(message_id, subject, from_text, body)
    SELECT new.id, new.subject, new.from_name || ' ' || new.from_addr,
           COALESCE((SELECT content FROM bodies WHERE message_id = new.id), '');
END;

CREATE TRIGGER IF NOT EXISTS messages_fts_au AFTER UPDATE ON messages BEGIN
    DELETE FROM messages_fts WHERE message_id = old.id;
    INSERT INTO messages_fts(message_id, subject, from_text, body)
    SELECT new.id, new.subject, new.from_name || ' ' || new.from_addr,
           COALESCE((SELECT content FROM bodies WHERE message_id = new.id), '');
END;

CREATE TRIGGER IF NOT EXISTS messages_fts_ad AFTER DELETE ON messages BEGIN
    DELETE FROM messages_fts WHERE message_id = old.id;
END;

CREATE TRIGGER IF NOT EXISTS bodies_fts_ai AFTER INSERT ON bodies BEGIN
    DELETE FROM messages_fts WHERE message_id = new.message_id;
    INSERT INTO messages_fts(message_id, subject, from_text, body)
    SELECT id, subject, from_name || ' ' || from_addr, new.content
    FROM messages WHERE id = new.message_id;
END;

CREATE TRIGGER IF NOT EXISTS bodies_fts_au AFTER UPDATE ON bodies BEGIN
    DELETE FROM messages_fts WHERE message_id = new.message_id;
    INSERT INTO messages_fts(message_id, subject, from_text, body)
    SELECT id, subject, from_name || ' ' || from_addr, new.content
    FROM messages WHERE id = new.message_id;
END;

CREATE TRIGGER IF NOT EXISTS bodies_fts_ad AFTER DELETE ON bodies BEGIN
    DELETE FROM messages_fts WHERE message_id = old.message_id;
    INSERT INTO messages_fts(message_id, subject, from_text, body)
    SELECT id, subject, from_name || ' ' || from_addr, ''
    FROM messages WHERE id = old.message_id;
END;
"#;
