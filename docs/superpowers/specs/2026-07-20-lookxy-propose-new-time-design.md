# lookxy propose new time — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-20.
**Builds on:** the RSVP paths — the calendar RSVP prompt
(`App::start_rsvp`/`submit_rsvp`/`cancel_rsvp_comment`/`apply_rsvp`,
`RsvpPrompt`, `ui::calendar::draw_rsvp_prompt`/`handle_rsvp_prompt_key`) →
`SyncCommand::RespondEvent` → `OutboxOp::RespondEvent` →
`GraphClient::respond_event`; and the mail RSVP path
(`App::respond_meeting` → `SyncCommand::RespondMeeting` → engine
`respond_meeting` → `respond_event`). Also the local-datetime parser
(`datetime::parse_start`/`parse_end`).

## Goal

When **declining or tentatively-accepting** a meeting — from the Calendar RSVP
prompt or the mail reader — let the user optionally counter-propose a new time,
sent via Graph's `proposedNewTime` on the `decline`/`tentativelyAccept` action.
Accept never proposes (Graph rejects `proposedNewTime` on `accept`).

## Background

Graph's event RSVP actions take an optional `proposedNewTime`:

```
POST /me/events/{id}/decline   (or /tentativelyAccept)
{ "comment": "...", "sendResponse": true,
  "proposedNewTime": { "start": {"dateTime": "...", "timeZone": "UTC"},
                       "end":   {"dateTime": "...", "timeZone": "UTC"} } }
```

`accept` does not support it. lookxy already funnels both RSVP surfaces through
`respond_event`; adding the proposed window is a matter of threading it from the
two UI entry points down to that one call.

## Product decisions (locked)

- **Both surfaces:** Calendar RSVP (`a`/`d`/`t`) and the mail reader
  (`A`/`D`/`T`). Propose is offered on **decline/tentative only**.
- **Mail `A` stays instant** (accepts immediately, no prompt); mail `D`/`T` open
  the prompt.
- **Blank = no proposal.** The proposed-time fields start empty; typing a time
  is what proposes one. No separate on/off toggle.
- **Both-or-neither:** if either proposed field is filled, both must parse and
  `end > start`, else an inline error and nothing sent.
- **Esc** sends the plain RSVP (no comment, no proposal), unchanged.

## Architecture

### 1. Client (`mailcore/src/graph/client.rs`)

`respond_event` gains a proposed-window parameter:

```rust
pub fn respond_event(
    &self, id: &str, kind: RsvpKind,
    comment: Option<&str>, send_response: bool,
    proposed: Option<(String, String)>,  // (start_utc, end_utc)
) -> Result<(), GraphError>
```

The body adds a `"proposedNewTime"` object (start/end each
`{dateTime: <utc>, timeZone: "UTC"}`) **only when `proposed.is_some()`**. Every
caller passes `None` for accept; the two RSVP prompts pass `Some(window)` only
for decline/tentative.

### 2. Sync (`mailcore/src/store/mod.rs` + `sync/*`)

- `OutboxOp::RespondEvent` gains `proposed_start_utc: Option<String>` and
  `proposed_end_utc: Option<String>` (alongside the existing `comment`);
  `kind`/`to_json`/`from_json` updated; `sync::outbox::apply_op` passes
  `proposed_start_utc.zip(proposed_end_utc)` (i.e. `Some((s, e))` when both are
  present) to `respond_event`.
- `SyncCommand::RespondEvent` gains the same two fields.
- `SyncCommand::RespondMeeting` gains `comment: Option<String>` +
  `proposed_start_utc`/`proposed_end_utc: Option<String>`; the engine's
  `respond_meeting` (after resolving the event id) passes them to
  `respond_event`.

### 3. App (`lookxy/src/app.rs`)

The RSVP prompt is generalized to serve both surfaces and carry a proposed
window:

```rust
pub enum RsvpTarget { Event(String), Message(String) } // calendar event id vs mail message id
pub struct RsvpPrompt {
    pub target: RsvpTarget,
    pub kind: String,            // "accepted"/"declined"/"tentativelyAccepted"
    pub comment: String,
    pub proposed_start: String,  // local-time text; "" = none
    pub proposed_end: String,
    pub focus: RsvpField,        // ProposedStart | ProposedEnd | Comment
}
pub enum RsvpField { ProposedStart, ProposedEnd, Comment }
```

- **Calendar** `a`/`d`/`t` (`start_rsvp`): open the prompt with
  `target: Event(highlighted_event_id)`, empty proposed/comment, focus
  `Comment` for accept or `ProposedStart` for decline/tentative.
- **Mail reader**: `A` → `respond_meeting(Accept)` **instantly** (unchanged, no
  prompt); `D`/`T` → open the prompt with `target: Message(selected_msg)`, kind
  from the key, focus `ProposedStart`. (A guard keeps `D`/`T` a no-op unless the
  opened message is a meeting request, mirroring today's `respond_meeting`.)
- **Field editing:** `Tab` cycles focus among the shown fields; typing edits the
  focused field. For accept, only `Comment` is shown/focusable.
- **Submit** (`submit_rsvp`): if either proposed field is non-empty, parse both
  via `datetime::parse_start`/`parse_end` (inline error "Invalid proposed time"
  on failure or `end <= start`; nothing sent) → `Some((start_utc, end_utc))`;
  else `None`. Then dispatch by target:
  - `Event(id)` → optimistic `store.set_event_response` + `reload_agenda` +
    `SyncCommand::RespondEvent { id, kind, comment, proposed_start_utc,
    proposed_end_utc }`.
  - `Message(mid)` → `SyncCommand::RespondMeeting { message_id: mid, kind:
    RsvpKind, comment, proposed_start_utc, proposed_end_utc }` (kind mapped from
    the response-status string to `RsvpKind`).
- **Esc** (`cancel_rsvp_comment`): send the plain RSVP (no comment, no
  proposal) — same dispatch, all optional fields `None`.
- `is_capturing_text` already includes `rsvp_prompt` (multi-field entry keeps
  that true).

### 4. UI (`lookxy/src/ui/calendar.rs`)

`draw_rsvp_prompt` grows from one comment line to a small form: a
`Proposed start` and `Proposed end` row (rendered only when kind is
decline/tentative) plus the `Comment` row, with the focused field highlighted.
`handle_rsvp_prompt_key`: `Tab` → focus-cycle, `Enter` → submit, `Esc` →
send-without, `Backspace`/char → edit the focused field. The prompt is drawn as
the same centered overlay it is today (taller when the proposed rows show).

Because the prompt now serves both modes, its **key routing moves up**: today
`handle_rsvp_prompt_key` is dispatched from inside `calendar::handle_key`
(Calendar mode only). Move that check to the top of `ui::handle_key` — right
after the sign-in/OOF/full-frame-overlay handlers and before the mode branches —
so `if app.rsvp_prompt.is_some() { calendar::handle_rsvp_prompt_key(app, key);
return; }` fires in both modes (the function stays in `calendar.rs`, exposed
`pub(crate)`; the calendar-mode inner check is removed to avoid double routing).
For drawing: `ui::calendar::draw_rsvp_prompt` is already called from the
calendar draw; add a matching call in the mail-mode branch of `ui::draw` so the
prompt shows over the reader too.

## Data flow

```
Calendar 'd' (or reader 'D' on an invite)
  → open RsvpPrompt{target, kind: declined, proposed_start/end: "", comment: ""}
  → type proposed start/end (+ comment) → Enter
  → submit_rsvp: parse proposed window (both or neither; end>start)
  → Event  → RespondEvent{id, declined, comment, proposed_*}   (optimistic + outbox)
     Message → RespondMeeting{message_id, Decline, comment, proposed_*} (direct)
  → respond_event(id, Decline, comment, sendResponse=true, Some((start,end)))
  → POST /me/events/{id}/decline  { …, proposedNewTime: {start,end} }
```

## Error handling & edge cases

- **Half-filled / invalid / `end ≤ start` proposed time** → inline "Invalid
  proposed time", nothing sent, prompt stays open.
- **Accept** → the proposed rows are never shown/sent; accept carries only an
  optional comment (calendar) or is instant (mail).
- **Mail `D`/`T` on a non-invite** → no-op (guarded, same as `respond_meeting`
  today).
- **Send failure** → the existing RSVP handling: calendar rides the
  `RespondEvent` outbox retry/quarantine (with the calendar reconverge on
  quarantine); mail goes through the engine's `react` on the direct call.
- **Optimistic status** → `Event` path still writes `set_event_response`
  (declined/tentative) immediately; the proposed time itself has no separate
  local state (it's server-side only).

## Testing

**mailcore (unit):**
- `respond_event` body includes `proposedNewTime.start/end.dateTime` when
  `Some`, and omits the key entirely when `None`; the `decline` and
  `tentativelyAccept` routes both accept it.
- `OutboxOp::RespondEvent` `to_json`/`from_json` round-trip the two proposed
  fields (present and absent).
- `sync::outbox::apply_op` for `RespondEvent` with both proposed fields hits the
  right route with a `proposedNewTime` body.
- Engine `RespondMeeting` with proposed fields resolves the event and POSTs
  `decline` with `proposedNewTime`.

**lookxy (unit):**
- Calendar `d` opens the prompt; entering a valid proposed start/end and Enter
  sends `RespondEvent` whose `proposed_start_utc`/`end_utc` are the parsed UTC.
- Mail `D` on an invite opens the prompt (`Message` target); Enter with a
  proposed window sends `RespondMeeting` with it; mail `A` sends `RespondMeeting`
  instantly with no prompt.
- Half-filled or `end ≤ start` proposed time → inline error, no command sent.
- Blank proposed fields → the command carries `None`/`None` (plain RSVP).
- Accept prompt shows no proposed rows; `Esc` sends the plain RSVP.
- `draw_rsvp_prompt` renders the `Proposed start`/`Proposed end` rows for
  decline, not for accept (`TestBackend`).

## Scope boundaries (YAGNI)

- **Decline/tentative only** — accept never proposes.
- **Blank = no proposal** — no separate on/off control.
- **One proposed window** — no multiple alternatives.
- **No availability suggestion** — that's the Free/busy feature (next).
- **Mail `A` stays instant** — only `D`/`T` open the prompt.
