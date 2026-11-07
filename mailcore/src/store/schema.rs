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
    sort_order      INTEGER,
    is_expanded     INTEGER NOT NULL DEFAULT 0,
    -- The whole-mailbox backfill (#1871): the full delta's in-progress
    -- `nextLink` (NULL: not started, or done), the items it returned so
    -- far, what `delta_link` covers (NULL: never synced, or a link from
    -- before #1871; 'head', 'window' or 'all'), and what the walk under way
    -- covers ('window' once a page ran with the window's cutoff, else 'all').
    backfill_link   TEXT,
    backfill_synced INTEGER NOT NULL DEFAULT 0,
    history_scope   TEXT,
    backfill_scope  TEXT
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
    bcc_recipients  TEXT NOT NULL DEFAULT '',
    received_at     TEXT NOT NULL DEFAULT '',
    sent_at         TEXT NOT NULL DEFAULT '',
    is_read         INTEGER NOT NULL DEFAULT 0,
    is_flagged      INTEGER NOT NULL DEFAULT 0,
    has_attachments INTEGER NOT NULL DEFAULT 0,
    importance      TEXT NOT NULL DEFAULT '',
    preview         TEXT NOT NULL DEFAULT '',
    is_draft        INTEGER NOT NULL DEFAULT 0,
    is_meeting_request INTEGER NOT NULL DEFAULT 0,
    categories      TEXT NOT NULL DEFAULT '',
    -- The `local:` id a draft had before `Store::reconcile_id` renamed it to
    -- its Graph id (#1884), so a caller still holding that id keeps reaching
    -- the draft. NULL for every other row. Lives on the row, so it goes with
    -- the message on every delete path. Its unique index is created in
    -- `Store::init`, after the migration that adds the column to old files.
    local_id        TEXT,
    -- A draft's unsaved-edit generation (#1871): `update_draft_fields`
    -- bumps it, a successful push clears it back to 0 if no edit came
    -- since. Non-zero: the row holds fields Graph has not seen, which a
    -- fresh delta must not overwrite.
    local_edit      INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_messages_folder_received
    ON messages(folder_id, received_at DESC);

-- The engine's own body fetches (#1978) ask for the newest mail across
-- every folder (`Store::body_candidates`), on every idle step.
CREATE INDEX IF NOT EXISTS idx_messages_received
    ON messages(received_at DESC);

CREATE TABLE IF NOT EXISTS master_categories (
    display_name TEXT PRIMARY KEY,
    color        TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS contacts (
    address    TEXT PRIMARY KEY,
    name       TEXT NOT NULL DEFAULT '',
    source     TEXT NOT NULL DEFAULT 'local',
    last_seen  TEXT NOT NULL DEFAULT '',
    frequency  INTEGER NOT NULL DEFAULT 0,
    relevance  INTEGER
);

-- The user's own Contacts folder (`/me/contacts`, #1934), one row per
-- lowercased address, replaced whole by every successful fetch
-- (`Store::replace_folder_contacts`). Apart from `contacts` on purpose: that
-- table is `/me/people` and mined correspondents, autocomplete only, while
-- this one answers `Store::is_contact` for a trust decision.
CREATE TABLE IF NOT EXISTS folder_contacts (
    address TEXT PRIMARY KEY,
    name    TEXT NOT NULL DEFAULT ''
);

-- `fetched_as` is the format `FetchBody` asked Graph for (`text`/`html`,
-- #1901); NULL for a body written locally (drafts, createReply), which
-- `Store::get_body_for` never offers for a refetch, or by an engine from
-- before the column (which always asked for text).
CREATE TABLE IF NOT EXISTS bodies (
    message_id   TEXT PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    content_type TEXT NOT NULL DEFAULT '',
    content      TEXT NOT NULL DEFAULT '',
    fetched_as   TEXT
);

-- A few internet headers of a fetched body (#1933): only
-- `graph::model::STORED_HEADERS` (Exchange's AuthAs and
-- MessageDirectionality stamps, and the `msip_labels` sensitivity
-- label, #2015), written with the body by
-- `Store::put_fetched_body_with_headers`, which replaces the message's
-- rows. `name` is ASCII-lowercased, `value` trimmed. A new table, so an
-- older database gets it empty at the next `open`: its mail reads `None`
-- from `Store::message_header`.
CREATE TABLE IF NOT EXISTS message_headers (
    message_id TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    value      TEXT NOT NULL,
    PRIMARY KEY (message_id, name)
);

CREATE TABLE IF NOT EXISTS attachments (
    id           TEXT NOT NULL,
    message_id   TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    name         TEXT NOT NULL DEFAULT '',
    content_type TEXT NOT NULL DEFAULT '',
    size         INTEGER NOT NULL DEFAULT 0,
    is_inline    INTEGER NOT NULL DEFAULT 0,
    content_id   TEXT,
    kind         TEXT NOT NULL DEFAULT 'file',
    source_url   TEXT,
    PRIMARY KEY (message_id, id)
);

-- `uploaded` is 1 once Graph confirmed a send's upload of the file to the
-- draft (#1874), so a resend after a failed send does not upload it again.
-- `content_id` set makes it inline (#1949: an html body's `cid:` picture);
-- `content_type` NULL is derived from the name at send time; `data` set
-- means the bytes live in the row (a pasted picture), keyed `cid:<content_id>`.
CREATE TABLE IF NOT EXISTS outbound_attachments (
    draft_id      TEXT NOT NULL,
    path          TEXT NOT NULL,
    name          TEXT NOT NULL,
    size          INTEGER NOT NULL DEFAULT 0,
    uploaded      INTEGER NOT NULL DEFAULT 0,
    content_id    TEXT,
    content_type  TEXT,
    data          BLOB,
    PRIMARY KEY (draft_id, path)
);

-- `send_posted` is 1 once a `SendDraft` op's `/send` POST went out (#1874):
-- written before the POST, so a lost answer or a crash still counts, and a
-- later attempt first asks Graph whether the message was delivered.
CREATE TABLE IF NOT EXISTS outbox (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    op          TEXT NOT NULL,
    message_id  TEXT,
    payload     TEXT NOT NULL DEFAULT '',
    attempts    INTEGER NOT NULL DEFAULT 0,
    last_error  TEXT,
    send_posted INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', '1');

-- v2 calendar: a read-only mirror of Graph calendar events, synced the same
-- way `messages` is (see `Store::upsert_event`). The spec's original idea —
-- event bodies reusing the existing `bodies` table keyed by `event:<id>` —
-- doesn't work: `bodies.message_id` has `REFERENCES messages(id) ON DELETE
-- CASCADE`, so an `event:<id>` key would fail that FK (no such row in
-- `messages`). Simplest correct fix, since an event has exactly one body:
-- `events.body_html` below, a plain column, no separate table, no FK to
-- fight. Each event carries the calendar it was fetched from in
-- `calendar_id` (#1984): `calendars` below lists them, and the chosen
-- calendars' selection is in `meta` (`calendar_selection`). `''` is a row
-- stored before #1984 (all from the default calendar), which a sync pass
-- moves to the default calendar's id once the list is known.
CREATE TABLE IF NOT EXISTS events (
    id               TEXT PRIMARY KEY,
    subject          TEXT NOT NULL DEFAULT '',
    start_utc        TEXT NOT NULL DEFAULT '',
    end_utc          TEXT NOT NULL DEFAULT '',
    is_all_day       INTEGER NOT NULL DEFAULT 0,
    location         TEXT NOT NULL DEFAULT '',
    organizer_name   TEXT NOT NULL DEFAULT '',
    organizer_addr   TEXT NOT NULL DEFAULT '',
    response_status  TEXT NOT NULL DEFAULT '',
    series_master_id TEXT,
    body_preview     TEXT NOT NULL DEFAULT '',
    web_link         TEXT NOT NULL DEFAULT '',
    last_modified    TEXT NOT NULL DEFAULT '',
    body_html        TEXT NOT NULL DEFAULT '',
    recurrence       TEXT NOT NULL DEFAULT '',
    reminder_minutes INTEGER NOT NULL DEFAULT 0,
    is_reminder_on   INTEGER NOT NULL DEFAULT 0,
    -- What a calendar client shows (#1983). `Store::init` adds each to an
    -- older database with `ALTER TABLE`, after this script runs: no index
    -- here may name one of them. `graph_recurrence` is Graph's recurrence
    -- of a series (read side); `recurrence` above is the create-side one
    -- `Store::event_for_send` re-sends.
    show_as                  TEXT NOT NULL DEFAULT '',
    is_online_meeting        INTEGER NOT NULL DEFAULT 0,
    online_meeting_provider  TEXT NOT NULL DEFAULT '',
    join_url                 TEXT NOT NULL DEFAULT '',
    categories               TEXT NOT NULL DEFAULT '',
    sensitivity              TEXT NOT NULL DEFAULT '',
    importance               TEXT NOT NULL DEFAULT '',
    event_type               TEXT NOT NULL DEFAULT '',
    graph_recurrence         TEXT NOT NULL DEFAULT '',
    is_cancelled             INTEGER NOT NULL DEFAULT 0,
    is_organizer             INTEGER NOT NULL DEFAULT 0,
    response_requested       INTEGER NOT NULL DEFAULT 1,
    allow_new_time_proposals INTEGER NOT NULL DEFAULT 1,
    has_attachments          INTEGER NOT NULL DEFAULT 0,
    original_start_tz        TEXT NOT NULL DEFAULT '',
    original_end_tz          TEXT NOT NULL DEFAULT '',
    calendar_id              TEXT NOT NULL DEFAULT '',
    -- A `local:` event created without a reminder (#1994): its `CreateEvent`
    -- sends none, so Graph applies the mailbox default, and the reconcile
    -- writes Graph's back and clears this. Internal: not in `EventRow`, not
    -- written by `upsert_event_on`.
    reminder_unset           INTEGER NOT NULL DEFAULT 0
);

-- Supports `events_in_window`'s `start_utc < ?to AND end_utc > ?from`
-- range filter, ordered by `start_utc`.
CREATE INDEX IF NOT EXISTS idx_events_window ON events(start_utc, end_utc);

-- The user's calendars as `GET /me/calendars` lists them (#1984), in
-- Graph's order (`position`). Written whole by `Store::replace_calendars`.
-- Which of them sync is `Store::synced_calendar_ids`: the selection in
-- `meta`, else the user's own (owner = the default calendar's owner).
CREATE TABLE IF NOT EXISTS calendars (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL DEFAULT '',
    color      TEXT NOT NULL DEFAULT '',
    hex_color  TEXT NOT NULL DEFAULT '',
    is_default INTEGER NOT NULL DEFAULT 0,
    can_edit   INTEGER NOT NULL DEFAULT 0,
    owner_name TEXT NOT NULL DEFAULT '',
    owner_addr TEXT NOT NULL DEFAULT '',
    position   INTEGER NOT NULL DEFAULT 0
);

-- What each calendar's events were fetched over and when (#2014), so a
-- `FetchCalendarRange` serves a fresh range from the store. Per calendar
-- the intervals `[from_utc, to_utc)` are disjoint (recording one trims the
-- older ones it overlaps); `fetched_at` is unix seconds. Written by
-- `apply_calendar_sync` (the window) and `apply_calendar_range`.
CREATE TABLE IF NOT EXISTS calendar_ranges (
    calendar_id TEXT NOT NULL,
    from_utc    TEXT NOT NULL,
    to_utc      TEXT NOT NULL,
    fetched_at  INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_calendar_ranges ON calendar_ranges(calendar_id, from_utc);

CREATE TABLE IF NOT EXISTS event_attendees (
    event_id TEXT NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    name     TEXT NOT NULL DEFAULT '',
    addr     TEXT NOT NULL DEFAULT '',
    type     TEXT NOT NULL DEFAULT '',
    response TEXT NOT NULL DEFAULT '',
    -- Graph `status.time`, `''` when never (#1983).
    response_time TEXT NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS idx_event_attendees_event ON event_attendees(event_id);

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
