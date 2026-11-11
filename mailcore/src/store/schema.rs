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
    backfill_scope  TEXT,
    -- The first call of the walk whose link is `delta_link` found Graph
    -- refusing the reminder `$expand` (#2054), so the link carries none: a
    -- round on it never clears a stored reminder. `backfill_expand_refused`
    -- is the same for the backfill walk under way, moved to
    -- `reminder_expand_refused` when its link becomes `delta_link`.
    reminder_expand_refused INTEGER NOT NULL DEFAULT 0,
    backfill_expand_refused INTEGER NOT NULL DEFAULT 0
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
    local_edit      INTEGER NOT NULL DEFAULT 0,
    -- A flagged message's Outlook reminder (#2054): on (never on an
    -- unflagged row), the time it was set for and when it fires next, and
    -- the flag's due time, all `YYYY-MM-DDTHH:MM:SSZ` or ''. An old file
    -- gets them, and its delta links reset, in `Store::init`, which also
    -- creates their index.
    is_reminder_on     INTEGER NOT NULL DEFAULT 0,
    reminder_at        TEXT NOT NULL DEFAULT '',
    reminder_signal_at TEXT NOT NULL DEFAULT '',
    flag_due_at        TEXT NOT NULL DEFAULT '',
    -- The meeting message type (#2081): Graph's `meetingMessageType` name
    -- (`MeetingMessageType::as_graph`), NULL for any other message. An old
    -- file gets it, and its delta links reset, in `Store::init`.
    meeting_message_type TEXT,
    -- The message's protection (#2088): `PidTagSensitivity` (0 normal,
    -- 1 personal, 2 private, 3 confidential), whether it carries an enabled
    -- Purview sensitivity label and that label's name ('' when unnamed or
    -- none), and whether it is rights-protected (IRM). An old file gets
    -- them, and its delta links reset, in `Store::init`.
    sensitivity            INTEGER NOT NULL DEFAULT 0,
    has_sensitivity_label  INTEGER NOT NULL DEFAULT 0,
    sensitivity_label_name TEXT NOT NULL DEFAULT '',
    is_rights_protected    INTEGER NOT NULL DEFAULT 0,
    -- A meeting message's own reminder is set (#2089): its
    -- `PidLidReminderSet`, kept apart from the flag's `is_reminder_on`; it
    -- never fires. An old file gets it in `Store::init`, without a delta
    -- reset: the links already carry the `$expand` that brings it.
    meeting_reminder_on    INTEGER NOT NULL DEFAULT 0,
    -- Which message a reply answers (#2096): the row's own
    -- `internetMessageId` and its In-Reply-To (`PidTagInReplyToId`), `''`
    -- when unknown. A merge never clears a stored value. An old file gets
    -- them in `Store::init`, with a delta reset; the partial index over
    -- unsent reply drafts is made there too, after that migration.
    internet_message_id    TEXT NOT NULL DEFAULT '',
    in_reply_to            TEXT NOT NULL DEFAULT '',
    -- The message's size in bytes (#2123, `PidTagMessageSize`), NULL when
    -- unknown, and whether the engine's size fill has asked Graph for it
    -- (once per row). An old file gets both in `Store::init`, without a
    -- delta reset; the partial index over unsized rows is made there too,
    -- after that migration.
    size                   INTEGER,
    size_asked             INTEGER NOT NULL DEFAULT 0
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

-- #1934's Contacts-folder addresses, one row per lowercased address. Legacy
-- since #2063: nothing writes it any more, `Store::replace_contacts` empties
-- it, and `Store::is_contact` still reads it, so a database from before the
-- records keeps its trust answers until their first fetch.
CREATE TABLE IF NOT EXISTS folder_contacts (
    address TEXT PRIMARY KEY,
    name    TEXT NOT NULL DEFAULT ''
);

-- The user's own Contacts folder (`/me/contacts`, #1934, #2063), one row per
-- contact keyed by Graph `id` (or a `local:` id until its create reaches
-- Graph), replaced by every successful fetch (`Store::replace_contacts`)
-- except where a queued contact op protects a row. Apart from `contacts` on
-- purpose: that table is `/me/people` and mined correspondents,
-- autocomplete only, while this one answers `Store::is_contact` for a trust
-- decision. The phone and category lists are `\u{1f}`-joined
-- (`encode_categories`).
CREATE TABLE IF NOT EXISTS contact_records (
    id                   TEXT PRIMARY KEY,
    change_key           TEXT NOT NULL DEFAULT '',
    given_name           TEXT NOT NULL DEFAULT '',
    middle_name          TEXT NOT NULL DEFAULT '',
    surname              TEXT NOT NULL DEFAULT '',
    display_name         TEXT NOT NULL DEFAULT '',
    file_as              TEXT NOT NULL DEFAULT '',
    business_phones      TEXT NOT NULL DEFAULT '',
    home_phones          TEXT NOT NULL DEFAULT '',
    mobile_phone         TEXT NOT NULL DEFAULT '',
    company_name         TEXT NOT NULL DEFAULT '',
    job_title            TEXT NOT NULL DEFAULT '',
    department           TEXT NOT NULL DEFAULT '',
    business_street      TEXT NOT NULL DEFAULT '',
    business_city        TEXT NOT NULL DEFAULT '',
    business_state       TEXT NOT NULL DEFAULT '',
    business_postal_code TEXT NOT NULL DEFAULT '',
    business_country     TEXT NOT NULL DEFAULT '',
    home_street          TEXT NOT NULL DEFAULT '',
    home_city            TEXT NOT NULL DEFAULT '',
    home_state           TEXT NOT NULL DEFAULT '',
    home_postal_code     TEXT NOT NULL DEFAULT '',
    home_country         TEXT NOT NULL DEFAULT '',
    birthday             TEXT NOT NULL DEFAULT '',
    personal_notes       TEXT NOT NULL DEFAULT '',
    categories           TEXT NOT NULL DEFAULT ''
);

-- A contact's `emailAddresses`, in order. `address_key` is the
-- `folder_contact_key` `is_contact` matches on, `''` for a blank address or
-- one without an `@` (kept for display, never matched). The rows follow a
-- re-keyed contact (`ON UPDATE CASCADE`, `reconcile_created_contact`).
CREATE TABLE IF NOT EXISTS contact_emails (
    contact_id  TEXT NOT NULL
                REFERENCES contact_records(id) ON DELETE CASCADE ON UPDATE CASCADE,
    position    INTEGER NOT NULL,
    name        TEXT NOT NULL DEFAULT '',
    address     TEXT NOT NULL DEFAULT '',
    address_key TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (contact_id, position)
);
CREATE INDEX IF NOT EXISTS contact_emails_key ON contact_emails(address_key);

-- The Graph id a created contact's `local:` id was re-keyed to
-- (`reconcile_created_contact`, #2063), so an op or a host holding the
-- `local:` id still reaches it. No foreign key: an alias outlives the row,
-- so a delete queued by the `local:` id still reaches the Graph contact.
CREATE TABLE IF NOT EXISTS contact_aliases (
    local_id TEXT PRIMARY KEY,
    id       TEXT NOT NULL
);

-- The id a mail folder was re-keyed to (#1910): a folder created locally
-- under a `local:` id, once its create reached Graph
-- (`Store::reconcile_created_folder`), or a folder Graph answered a move
-- with a new id for. An op or a host holding the old id still reaches it
-- (`Store::current_folder_id`). Every alias names the newest id (a re-key
-- re-points the aliases to the old id), so one lookup resolves. No foreign
-- key: an alias outlives the row, so an op queued by the old id still
-- reaches the Graph folder.
CREATE TABLE IF NOT EXISTS folder_aliases (
    old_id TEXT PRIMARY KEY,
    id     TEXT NOT NULL
);

-- Every Graph id the store held and no longer does (#2107): a message or a
-- folder row deleted by any path (a delta `@removed`, a folder cascade, a
-- refused create's discard, a host's delete on its own connection), or an id
-- a row was re-keyed away from. Written by triggers on `messages` and
-- `folders` (`FORGOTTEN_TRIGGERS_SQL`, installed by `Store::init` after the
-- migrations, as they read `internet_message_id`); a row coming back under
-- the id clears it. A folder delete's walk of Graph (`delete_allow_check`)
-- reads it: an item Graph still has that the store once held elsewhere is
-- not taken. `imid` is a message's `internet_message_id` ('' for a folder,
-- or a row from before #2096). Pruned to the newest `FORGOTTEN_CAP` rows by
-- rowid (`INSERT OR REPLACE` gives a re-removed id a fresh one). `local:`
-- ids, which Graph never had, are not recorded.
CREATE TABLE IF NOT EXISTS forgotten_ids (
    kind TEXT NOT NULL,
    id   TEXT NOT NULL,
    imid TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (kind, id)
);
CREATE INDEX IF NOT EXISTS idx_forgotten_ids_imid ON forgotten_ids(imid) WHERE imid <> '';

-- What a `DeleteFolder` command removed (#2107), per batch (one command,
-- one batch, one queued `DeleteFolder` per Graph root): every Graph folder
-- (`kind` 'folder') and every message (`kind` 'message', with its
-- `internet_message_id`) the store showed in the deleted subtree. The walk
-- of Graph before each DELETE allows only these, and what the store never
-- held. Removed once the batch's last op is gone. A folder's `root` is the
-- batch's root whose DELETE takes it (the nearest root at or above it in the
-- stored tree): the folder stays hidden from listings only while that root's
-- op is queued (r2); '' for a message.
CREATE TABLE IF NOT EXISTS folder_delete_allow (
    batch TEXT NOT NULL,
    kind  TEXT NOT NULL,
    id    TEXT NOT NULL,
    imid  TEXT NOT NULL DEFAULT '',
    root  TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (batch, kind, id)
);

-- Contacts a refused write must read again from Graph (#2063), once no
-- queued contact op names them (`Store::owe_contact_reread`). Persisted, so a
-- restart before the last such op drains still re-reads.
CREATE TABLE IF NOT EXISTS contact_reread_owed (
    id TEXT PRIMARY KEY
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

-- The ids a message had before a move (#1896): Graph mints a new id on
-- move, and `Store::follow_move` re-keys the row to it, leaving one row
-- here per earlier id, each pointing at the message's latest id (a chain
-- A -> B -> C is kept flat: A -> C, B -> C). `Store::current_id` follows
-- it; the delta `@removed` path (`delete_message`) does not, and a delta
-- upsert of an alias is skipped. No foreign key: an alias outlives its
-- row, so a `Delete` queued by the old id of a message deleted locally
-- while its move was in flight still reaches the id Graph knows. Rows are
-- never pruned (one per move). A new table, so an older database gets it
-- empty at the next `open`.
CREATE TABLE IF NOT EXISTS moved_ids (
    old_id     TEXT PRIMARY KEY,
    message_id TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_moved_ids_message ON moved_ids(message_id);

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
-- `upload_url` is the Graph upload session a send opened for it (#1941), and
-- `upload_digest` the fingerprint of what that session uploads: a later
-- attempt resumes the session only while the digest still matches.
CREATE TABLE IF NOT EXISTS outbound_attachments (
    draft_id      TEXT NOT NULL,
    path          TEXT NOT NULL,
    name          TEXT NOT NULL,
    size          INTEGER NOT NULL DEFAULT 0,
    uploaded      INTEGER NOT NULL DEFAULT 0,
    content_id    TEXT,
    content_type  TEXT,
    data          BLOB,
    upload_url    TEXT,
    upload_digest TEXT,
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
    reminder_unset           INTEGER NOT NULL DEFAULT 0,
    -- Each end's named time zone and its wall clock there (#2039), what a
    -- `CreateEvent` / `UpdateEvent` sends as `start`/`end`; `''` sends UTC.
    -- Internal, as `reminder_unset`: not written by `upsert_event_on`, so a
    -- delta upsert keeps them. A window or range replace deletes synced rows
    -- and re-inserts them with `''` (their times reset with them, so no
    -- stale pair is left); `local:` rows, which `CreateEvent` reads, keep them.
    send_start_tz            TEXT NOT NULL DEFAULT '',
    send_start_local         TEXT NOT NULL DEFAULT '',
    send_end_tz              TEXT NOT NULL DEFAULT '',
    send_end_local           TEXT NOT NULL DEFAULT '',
    -- When the user answered the event (Graph `responseStatus.time`) and
    -- when it was created (`createdDateTime`), canonical UTC or `''` (#2068).
    -- Added to an older database by `Store::init`, as the #1983 columns.
    response_time            TEXT NOT NULL DEFAULT '',
    created_utc              TEXT NOT NULL DEFAULT ''
);

-- Supports `events_in_window`'s `start_utc < ?to AND end_utc > ?from`
-- range filter, ordered by `start_utc`.
CREATE INDEX IF NOT EXISTS idx_events_window ON events(start_utc, end_utc);

-- A series' rows by their master (#2064): the grid queries' `GRID_ROWS`
-- asks whether a master has any. `series_master_id` is an original
-- column, never added by `ALTER TABLE`, so an older table has it here.
CREATE INDEX IF NOT EXISTS idx_events_series ON events(series_master_id);

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

-- Each synced calendar's calendarView delta state (#2013): the deltaLink of
-- its last round and the window `[from_utc, to_utc)` that round covers. A
-- pass uses the link only while the window is the pass's own. Written with
-- the pass (`apply_calendar_sync`), deleted with the calendar's events.
CREATE TABLE IF NOT EXISTS calendar_deltas (
    calendar_id TEXT PRIMARY KEY,
    from_utc    TEXT NOT NULL,
    to_utc      TEXT NOT NULL,
    delta_link  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS event_attendees (
    event_id TEXT NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    name     TEXT NOT NULL DEFAULT '',
    addr     TEXT NOT NULL DEFAULT '',
    type     TEXT NOT NULL DEFAULT '',
    response TEXT NOT NULL DEFAULT '',
    -- Graph `status.time`, `''` when never (#1983).
    response_time TEXT NOT NULL DEFAULT '',
    -- The attendee's `proposedNewTime` in canonical UTC, `''` when none (#2023).
    proposed_start_utc TEXT NOT NULL DEFAULT '',
    proposed_end_utc TEXT NOT NULL DEFAULT ''
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

-- messages_fts_au is created by `migrate_fts_update_trigger`
-- (`MESSAGES_FTS_AU_SQL`), which also replaces an older database's.

-- messages_fts_ad and bodies_fts_ad are `FTS_DELETE_TRIGGERS_SQL`, run by
-- `Store::init` right after this: `Store::empty_folder` drops and re-creates
-- them from that one text (#2110).

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
"#;

/// The FTS triggers of a deleted message or body (#2110: moved out of
/// `SCHEMA_SQL`, the text unchanged). Each one's `DELETE` scans the whole
/// FTS table (`message_id` is `UNINDEXED`), so deleting a folder's thousands
/// of rows one trigger at a time is quadratic: `Store::empty_folder` drops
/// both inside its transaction, deletes the folder's FTS rows in one
/// statement, and runs this text again before it commits. Created by
/// `Store::init` right after `SCHEMA_SQL`; idempotent.
pub(crate) const FTS_DELETE_TRIGGERS_SQL: &str = "\
CREATE TRIGGER IF NOT EXISTS messages_fts_ad AFTER DELETE ON messages BEGIN
    DELETE FROM messages_fts WHERE message_id = old.id;
END;
CREATE TRIGGER IF NOT EXISTS bodies_fts_ad AFTER DELETE ON bodies BEGIN
    DELETE FROM messages_fts WHERE message_id = old.message_id;
    INSERT INTO messages_fts(message_id, subject, from_text, body)
    SELECT id, subject, from_name || ' ' || from_addr, ''
    FROM messages WHERE id = old.message_id;
END;";

/// The triggers that fill `forgotten_ids` (#2107): a deleted message or
/// folder row, and an id a re-key leaves, are recorded; a row inserted or
/// re-keyed under an id clears it. Cascades fire them, as they fire
/// `messages_fts_ad`. Created by `Store::init` after the migrations (an old
/// `messages` gets `internet_message_id` there); idempotent.
pub(crate) const FORGOTTEN_TRIGGERS_SQL: &str = "\
CREATE TRIGGER IF NOT EXISTS forgotten_messages_ad AFTER DELETE ON messages
WHEN substr(old.id, 1, 6) <> 'local:' BEGIN
    INSERT OR REPLACE INTO forgotten_ids(kind, id, imid)
    VALUES ('message', old.id, old.internet_message_id);
END;
CREATE TRIGGER IF NOT EXISTS forgotten_messages_ai AFTER INSERT ON messages BEGIN
    DELETE FROM forgotten_ids WHERE kind = 'message' AND id = new.id;
END;
CREATE TRIGGER IF NOT EXISTS forgotten_messages_au AFTER UPDATE OF id ON messages
WHEN old.id <> new.id BEGIN
    DELETE FROM forgotten_ids WHERE kind = 'message' AND id = new.id;
    INSERT OR REPLACE INTO forgotten_ids(kind, id, imid)
    SELECT 'message', old.id, old.internet_message_id
    WHERE substr(old.id, 1, 6) <> 'local:';
END;
CREATE TRIGGER IF NOT EXISTS forgotten_folders_ad AFTER DELETE ON folders
WHEN substr(old.id, 1, 6) <> 'local:' BEGIN
    INSERT OR REPLACE INTO forgotten_ids(kind, id) VALUES ('folder', old.id);
END;
CREATE TRIGGER IF NOT EXISTS forgotten_folders_ai AFTER INSERT ON folders BEGIN
    DELETE FROM forgotten_ids WHERE kind = 'folder' AND id = new.id;
END;
CREATE TRIGGER IF NOT EXISTS forgotten_folders_au AFTER UPDATE OF id ON folders
WHEN old.id <> new.id BEGIN
    DELETE FROM forgotten_ids WHERE kind = 'folder' AND id = new.id;
    INSERT OR REPLACE INTO forgotten_ids(kind, id)
    SELECT 'folder', old.id WHERE substr(old.id, 1, 6) <> 'local:';
END;";

/// The trigger that keeps a message's `messages_fts` row in step with an
/// UPDATE of the columns it reads (#1913): only those. `message_id` is
/// `UNINDEXED`, so its `DELETE` scans the whole FTS table; firing it on
/// every UPDATE (a read state, a flag) made marking a folder's 5 000 rows
/// read quadratic. `id` is listed for a re-key; the body has its own
/// triggers on `bodies`. Installed by `migrate_fts_update_trigger`.
pub(crate) const MESSAGES_FTS_AU_SQL: &str = "\
CREATE TRIGGER messages_fts_au
AFTER UPDATE OF id, subject, from_name, from_addr ON messages BEGIN
    DELETE FROM messages_fts WHERE message_id = old.id;
    INSERT INTO messages_fts(message_id, subject, from_text, body)
    SELECT new.id, new.subject, new.from_name || ' ' || new.from_addr,
           COALESCE((SELECT content FROM bodies WHERE message_id = new.id), '');
END;";
