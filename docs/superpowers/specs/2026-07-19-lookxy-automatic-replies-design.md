# lookxy automatic replies (out-of-office) — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-19.
**Builds on:** the Graph client (`send`/`parse_body`/`encode_path_segment`), the
sync engine's direct-call pattern (`fetch_body`/`respond_meeting` — a Graph
call gated by `with_auth`/`react`, no outbox op), the modal-form pattern
(`ui/eventform.rs` + `App::event_form`/`save_event_form`), the local-datetime
parser (`datetime::parse_start`/`parse_end`, UTC out), and the transient-notice
+ `is_capturing_text` machinery.

## Goal

From lookxy, let the user **view and set** the mailbox's automatic-replies
(out-of-office) configuration — full Outlook parity: on/off/scheduled status, an
optional scheduled window, external audience, and **separate** internal and
external reply messages — via Graph `/me/mailboxSettings`
(`automaticRepliesSetting`).

## Background

A mailbox's OOF configuration lives in Graph's `mailboxSettings` resource:

```
GET  /me/mailboxSettings           → { "automaticRepliesSetting": { … } }
PATCH /me/mailboxSettings          ← { "automaticRepliesSetting": { … } }
```

The `automaticRepliesSetting` object:

```json
{
  "status": "disabled" | "alwaysEnabled" | "scheduled",
  "externalAudience": "none" | "contactsOnly" | "all",
  "internalReplyMessage": "<html>",
  "externalReplyMessage": "<html>",
  "scheduledStartDateTime": { "dateTime": "2026-07-20T09:00:00.0000000", "timeZone": "UTC" },
  "scheduledEndDateTime":   { "dateTime": "2026-07-27T17:00:00.0000000", "timeZone": "UTC" }
}
```

The reply messages are HTML strings. lookxy edits them as plain text, so it
strips HTML on read and escapes + `<br>`-encodes newlines on write (see the
fidelity note below). The scheduled datetimes matter only when
`status == "scheduled"`.

## Product decisions (locked)

- **Full parity fields:** status (Off/On/Scheduled), scheduled start+end,
  external audience (None/Contacts/All), separate internal + external messages.
- **Entry key:** global **`O`** (uppercase; free in `on_key_char`, works in both
  Mail and Calendar modes since OOF is account-level).
- **Direct Graph calls, no outbox** — a settings write has no optimistic local
  mirror to reconcile; fetch/set mirror `respond_meeting`.
- **Plain-text message editing** — rich HTML from Outlook web is flattened to
  text on read; on write, escaped text with `<br>` for newlines. No rich
  editor.

## Architecture

### 1. Model (`mailcore/src/graph/model.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OofStatus { Disabled, AlwaysEnabled, Scheduled }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalAudience { None, ContactsOnly, All }

#[derive(Debug, Clone, PartialEq)]
pub struct AutomaticReplies {
    pub status: OofStatus,
    pub external_audience: ExternalAudience,
    pub internal_message: String,   // plain text
    pub external_message: String,   // plain text
    pub scheduled_start_utc: String, // canonical UTC, or "" when absent
    pub scheduled_end_utc: String,
}
```

- `OofStatus::from_wire(&str)` / `as_wire(&self) -> &'static str` map to
  `"disabled"/"alwaysEnabled"/"scheduled"` (unknown → `Disabled`).
- `ExternalAudience::from_wire` / `as_wire` map to `"none"/"contactsOnly"/"all"`
  (unknown → `All`, Graph's own default).
- `AutomaticReplies::from_json(v)` reads the `automaticRepliesSetting` object:
  status/audience via `from_wire`; `internal_message`/`external_message` via
  `html_to_plain` over `internalReplyMessage`/`externalReplyMessage`;
  scheduled start/end via `datetime_field_to_utc` (the same
  `{dateTime,timeZone}` → canonical-UTC helper `Event::from_json` uses), or `""`
  when the key is absent.

`html_to_plain(&str) -> String` (module-private helper): converts an OOF HTML
message to plain text — replace `<br>`, `<br/>`, `</p>`, `</div>` (case-
insensitive) with `\n`; drop all other `<…>` tags; decode `&amp; &lt; &gt;
&quot; &#39; &nbsp;`; collapse runs of 3+ newlines to 2; trim trailing
whitespace. Best-effort (see fidelity note).

`plain_to_html(&str) -> String`: the inverse for writing — HTML-escape `& < >`
(and `"` → `&quot;`), then replace `\n` with `<br>`. No wrapping element
(Graph accepts a bare fragment).

### 2. Graph client (`mailcore/src/graph/client.rs`)

```rust
pub fn get_automatic_replies(&self) -> Result<AutomaticReplies, GraphError>;
pub fn set_automatic_replies(&self, r: &AutomaticReplies) -> Result<(), GraphError>;
```

- `get_automatic_replies`: GET `/me/mailboxSettings`, `parse_body`, then
  `AutomaticReplies::from_json(&v)` (errors `GraphError::Parse` if the object is
  missing/malformed).
- `set_automatic_replies`: PATCH `/me/mailboxSettings` with a body built by a
  private `automatic_replies_body(r) -> String`:

  ```json
  { "automaticRepliesSetting": {
      "status": "<wire>",
      "externalAudience": "<wire>",
      "internalReplyMessage": "<plain_to_html>",
      "externalReplyMessage": "<plain_to_html>",
      // start/end included ONLY when status == Scheduled:
      "scheduledStartDateTime": { "dateTime": "<utc>", "timeZone": "UTC" },
      "scheduledEndDateTime":   { "dateTime": "<utc>", "timeZone": "UTC" }
  } }
  ```

  Non-scheduled statuses omit the two datetime keys entirely (sending stale/empty
  ones would make Graph reject the PATCH or silently flip to scheduled).

### 3. Sync (`mailcore/src/sync/engine.rs`)

- `SyncCommand::FetchAutomaticReplies` → handler `fetch_automatic_replies`:
  signed-in guard; `with_auth(|c| c.get_automatic_replies())`; on Ok emit
  `SyncEvent::AutomaticRepliesFetched { replies }`, else `react(e)`.
- `SyncCommand::SetAutomaticReplies { replies }` → handler
  `set_automatic_replies`: signed-in guard;
  `with_auth(|c| c.set_automatic_replies(&replies))`; on Ok emit
  `SyncEvent::AutomaticRepliesUpdated`, else `react(e)`.
- Events: `AutomaticRepliesFetched { replies: AutomaticReplies }`,
  `AutomaticRepliesUpdated`.

### 4. App (`lookxy/src/app.rs`)

- `OofForm` state (new, held as `app.oof_form: Option<OofForm>`), defined in
  `ui/oofform.rs`:

  ```rust
  pub enum OofField { Status, Start, End, Audience, Internal, External }
  pub struct OofForm {
      pub loading: bool,            // true until AutomaticRepliesFetched lands
      pub status: OofStatus,
      pub start: String,            // local-time text (parsed on save)
      pub end: String,
      pub audience: ExternalAudience,
      pub internal: String,         // multi-line plain text
      pub external: String,
      pub focus: OofField,
      pub error: Option<String>,    // inline footer validation message
  }
  ```

- `App::open_oof_form()` (bound to `O` in `on_key_char`): set
  `oof_form = Some(OofForm::loading_default())` (status Disabled, audience All,
  empty text, `loading = true`, focus Status) and send
  `SyncCommand::FetchAutomaticReplies`.
- `on_sync_event`:
  - `AutomaticRepliesFetched { replies }` → if the form is open, prefill its
    fields from `replies` (start/end rendered to local display text via the same
    inverse the event form uses — `datetime`'s local-display helper — or `""`
    when the UTC is empty), set `loading = false`. If the form isn't open, drop.
  - `AutomaticRepliesUpdated` → close the form (`oof_form = None`) and set a
    transient notice "Automatic replies updated" (reuses `attachment_notice`,
    the existing transient-notice field).
- `App::save_oof_form()`:
  - If `status == Scheduled`: parse `start` via `datetime::parse_start(start,
    now, offset)`; on `None` set `error = "Invalid start time"` and return
    (nothing sent). Parse `end` via `datetime::parse_end(end, &start_utc, now,
    offset)`; on `None` → `error = "Invalid end time"`, return. If
    `end_utc <= start_utc` → `error = "End must be after start"`, return.
    For non-Scheduled statuses, start/end are `""`.
  - Build `AutomaticReplies { status, external_audience: audience,
    internal_message: internal, external_message: external, scheduled_start_utc,
    scheduled_end_utc }` and send `SyncCommand::SetAutomaticReplies`. Leave the
    form open (it closes on `AutomaticRepliesUpdated`); set a transient
    "Saving…" notice.
- `is_capturing_text()` returns true when `oof_form.is_some()` (so global
  hotkeys don't steal keystrokes typed into the message fields).

### 5. UI (`lookxy/src/ui/oofform.rs` + `ui/mod.rs`)

- `oofform::draw(f, app)` — no-op unless `app.oof_form` is open; `Clear` the
  frame then render, mirroring `eventform::draw`. Layout top-to-bottom:
  a Status radio row `(x) Off  ( ) On  ( ) Scheduled`; a Start row and End row
  (rendered dimmed via `Color::DarkGray` when `status != Scheduled`); an
  Audience radio row `( ) None  ( ) Contacts  (x) All`; the Internal message
  editor; the External message editor; a 1-row footer (the inline `error`, or a
  key-hint line). While `loading`, the field area shows "loading…".
- `oofform::handle_key(app, key)` (called from `ui::mod::handle_key` when the
  form is open, before the pane handlers — same precedence `eventform`/`compose`
  get):
  - Tab / Shift-Tab (and Down/Up) cycle `focus` through the six fields.
  - Space on Status cycles Off→On→Scheduled→Off; on Audience cycles
    None→Contacts→All→None.
  - Char / Backspace edit the focused text field (Start/End single-line;
    Internal/External accept `Enter` as a newline).
  - `Ctrl-S` (and `Enter` when focus is NOT a multi-line message field) →
    `save_oof_form`.
  - `Esc` → `oof_form = None` (cancel, nothing sent).
- `ui/mod.rs`: call `oofform::draw` in the top-level draw (after the other
  overlays) and route keys to `oofform::handle_key` when `app.oof_form.is_some()`.

## Data flow

```
press O
  → open_oof_form: oof_form = Some(loading) + FetchAutomaticReplies
  → engine: get_automatic_replies  [GET /me/mailboxSettings]
          → AutomaticRepliesFetched { replies }
  → app prefills the form (loading = false)
  → user edits; Ctrl-S / Enter
  → save_oof_form: validate schedule → SetAutomaticReplies { replies } + "Saving…"
  → engine: set_automatic_replies  [PATCH /me/mailboxSettings]
          → AutomaticRepliesUpdated
  → app closes the form + "Automatic replies updated" notice
```

## Error handling & edge cases

- **Fetch fails** (offline/401/etc.) → `react(e)` (standard auth/throttle/offline
  handling; a 4xx/parse surfaces `SyncEvent::Error` → error notice). The form
  stays open in a non-loading, empty-editable state so the user can still author
  and save (a subsequent PATCH will set from scratch); `loading` is cleared by a
  timeout-free fallback: `react`-surfaced errors also set `loading = false` via
  the `Error` arm when a form is open.
- **PATCH fails** → `react(e)`; the form stays open with the error surfaced as a
  notice so the user can retry.
- **Scheduled with invalid/empty start or end** → inline footer error, nothing
  sent.
- **Scheduled with end ≤ start** → inline footer error, nothing sent.
- **Status not Scheduled** → start/end ignored and omitted from the PATCH, even
  if the fields contain text.
- **Not signed in** → the handlers emit `SyncEvent::Error("not signed in")`
  (same guard as `fetch_body`); the form shows the error notice.

## HTML fidelity note

OOF messages are HTML in Graph but edited as plain text here. `html_to_plain`
is best-effort: `<br>`/`</p>`/`</div>` become newlines, other tags are dropped,
the common entities are decoded. A message authored with rich formatting in
Outlook web (tables, styling, links) is flattened to its text content — the
user sees and re-saves plain text. `plain_to_html` escapes and `<br>`-encodes
on write, so a lookxy-authored message round-trips faithfully; a
round-tripped Outlook-web message loses its formatting. This is an accepted v1
limitation (no rich editor).

## Testing

**mailcore (unit):**
- `OofStatus`/`ExternalAudience` `from_wire`/`as_wire` round-trip, unknown →
  default.
- `AutomaticReplies::from_json`: a scheduled setting with both messages +
  window parses all fields (status, audience, plain-text messages, canonical UTC
  start/end); a disabled setting with no datetimes yields `""` starts/ends.
- `html_to_plain`: `<br>`/`</p>`/`</div>` → newline; other tags dropped;
  `&amp;`/`&lt;`/`&gt;`/`&quot;`/`&#39;`/`&nbsp;` decoded; 3+ newlines collapsed.
- `plain_to_html`: escapes `& < >` and `"`, `\n` → `<br>`.
- Client `get_automatic_replies` (FakeServer GET route returns a full
  `automaticRepliesSetting`; asserts parsed fields). Client
  `set_automatic_replies` (FakeServer PATCH; asserts the request body's
  `automaticRepliesSetting.status`/`externalAudience`/message HTML, that a
  Scheduled write includes both datetime objects, and that a Disabled write
  omits them).
- Engine `FetchAutomaticReplies` emits `AutomaticRepliesFetched`;
  `SetAutomaticReplies` PATCHes and emits `AutomaticRepliesUpdated` (mirror the
  existing engine harness — seed token, `spawn_with_bases`, `wait_for`).

**lookxy (unit):**
- `open_oof_form` (via `on_key_char('O')`) sends `FetchAutomaticReplies` and
  opens a loading form.
- `AutomaticRepliesFetched` prefills the form's fields and clears `loading`.
- `save_oof_form` with `status = Scheduled` and valid times sends
  `SetAutomaticReplies` whose `scheduled_start_utc`/`end_utc` are the parsed
  UTC; with `status = Disabled` sends empty starts/ends.
- `save_oof_form` with `status = Scheduled` + invalid start → inline `error`
  set, no command sent.
- Status cycling (Off→On→Scheduled→Off) and audience cycling
  (None→Contacts→All→None) via the form key handler.
- `AutomaticRepliesUpdated` closes the form and sets the confirmation notice.
- `oofform::draw` (TestBackend) renders the status/audience radios and both
  message labels; schedule rows dimmed when not Scheduled.
- `is_capturing_text()` is true while the form is open.

## Scope boundaries (YAGNI)

- **No rich-text editing** of the messages (plain text only).
- **No OOF templates / history / presets.**
- **No calendar auto-detect** (don't infer the window from a vacation event).
- **No local persistence/mirror** of the setting — it's fetched live each open
  and written straight through.
- **No change** to any existing mail/calendar surface.
