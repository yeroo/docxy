# lookxy meeting RSVP from mail — design

**Status:** approved (design), pending implementation plan.
**Date:** 2026-07-19.
**Builds on:** the calendar RSVP path (`GraphClient::respond_event`,
`RsvpKind::{Accept,Decline,Tentative}`, `SyncCommand::RespondEvent`), the
message model/store, and the reading pane (`lookxy/src/ui/reading.rs`,
`App::on_key_char`/`selected_message`).

## Goal

From the reading pane, let the user Accept / Decline / Tentatively-accept a
**meeting-invite email**. The response is sent to the organizer and the RSVP is
recorded on the corresponding calendar event — reusing the existing
`respond_event` (accept/decline/tentativelyAccept) that calendar-side RSVP
already uses.

## Background

A meeting request arrives in the mailbox as a message whose Graph type is
`#microsoft.graph.eventMessageRequest` (a subtype of `message`). Today lookxy
treats it as an ordinary email — there's no way to respond without opening
Outlook. The message model doesn't record that it's an invite, and a
meeting-invite message has no accept/decline action of its own: you respond on
the **event** it references (exposed via the message's `event` navigation
property).

## Product decisions (locked)

- **Keys:** uppercase **A**=Accept, **D**=Decline, **T**=Tentative, active ONLY
  when the opened message is a meeting request (guarded, so they never act on
  ordinary mail — lowercase `a`/`d`/`t` keep meaning attachments/delete/thread).
- **Always notify the organizer** (`send_response = true`); **no comment prompt**
  (matches the calendar RSVP default path).
- **Request invites only** — meeting *response*/*cancellation* messages don't get
  RSVP affordances.
- **Minimal banner** — the invite's time/location/organizer are already in the
  email body, so the banner is flag-driven with no separate event-detail fetch.

## Architecture

### 1. Detect (`mailcore`)

- `Message` gains `is_meeting_request: bool`, set in `from_json` when
  `@odata.type == "#microsoft.graph.eventMessageRequest"`. This type is already
  present in the list/delta response (it's a derived resource type), so no extra
  Graph call and no `$select` change.
- Store: `messages.is_meeting_request` column (idempotent `ALTER TABLE … ADD
  COLUMN … DEFAULT 0` migration, same pattern as `is_draft`/`content_id`).
  `MessageRow` carries it; `upsert_message`/reads persist it.

### 2. Reader banner (`lookxy/src/ui/reading.rs`)

When `selected_message().is_meeting_request`, the reading pane draws a banner
between the headers and the body:

```
📅 Meeting invite — [A]ccept  [D]ecline  [T]entative
```

Flag-driven only; the body below already shows subject/time/organizer. The
banner scrolls with the header block (it's part of the fixed header area, not
the scrolling body).

### 3. Resolve event id + respond (`mailcore`)

- New client method `meeting_event_id(message_id) -> Result<Option<String>, GraphError>`:
  `GET /me/messages/{id}?$expand=microsoft.graph.eventMessageRequest/event($select=id)`,
  returning the expanded `event.id` (or `None` if the message isn't an
  eventMessageRequest / has no event).
- RSVP reuses the existing `respond_event(event_id, kind, comment, send_response)`.

### 4. Sync (`mailcore/src/sync/engine.rs`)

- `SyncCommand::RespondMeeting { message_id: String, kind: RsvpKind }`.
- Engine handler `respond_meeting`: resolve `meeting_event_id(message_id)`; on
  `Some(event_id)` call `respond_event(event_id, kind, None, true)` then emit
  `SyncEvent::MeetingResponded { message_id, kind }`; on `None` emit
  `SyncEvent::Error("not a meeting invite")`; on a Graph error, `react(e)`.

### 5. App (`lookxy/src/app.rs`)

- `App::respond_meeting(kind: RsvpKind)`: no-op unless the opened
  (`selected_msg`) message is a meeting request; otherwise send
  `RespondMeeting { message_id, kind }` and set a transient notice
  ("Responding…").
- Keys `A`/`D`/`T` routed from `on_key_char` (or the mail-mode key handler) to
  `respond_meeting(Accept/Decline/Tentative)`, guarded by the opened message
  being a meeting request.
- `on_sync_event` handles `MeetingResponded { message_id, kind }` → a notice
  ("Accepted the invite" / "Declined the invite" / "Tentatively accepted") when
  it's the open message; also marks that message read (a small courtesy —
  reuses the existing read path).

## Data flow

```
open a meeting-invite email (is_meeting_request = true)
  → reader shows the RSVP banner
  → user presses A
  → App::respond_meeting(Accept): RespondMeeting{message_id, Accept} + "Responding…" notice
  → engine: meeting_event_id(message_id) [GET …$expand=…/event($select=id)]
          → respond_event(event_id, Accept, None, send_response=true) [POST …/events/{id}/accept]
          → SyncEvent::MeetingResponded{message_id, Accept}
  → App notice "Accepted the invite"; message marked read
```

## Error handling & edge cases

- **Not a meeting request** → the `A`/`D`/`T` keys are no-ops (guarded); nothing
  sent.
- **No resolvable event id** (odd server state) → `SyncEvent::Error("not a
  meeting invite")` → error notice; nothing changes.
- **respond_event fails** (401/offline/etc.) → the standard `react(e)` handling
  (error notice / sign-in / retry semantics), same as calendar RSVP.
- **Response/cancellation messages** (`eventMessageResponse` /
  cancellations) → `is_meeting_request` stays false → no banner, no keys.
- **Recurring-series invites** → responded to exactly as `respond_event` handles
  them today (the event id is the series master's; no per-occurrence handling —
  consistent with the existing RSVP-only recurring behavior).

## Testing

**mailcore (unit):**
- `Message::from_json` sets `is_meeting_request` true for
  `@odata.type == "#microsoft.graph.eventMessageRequest"`, false otherwise
  (ordinary message, eventMessageResponse).
- Store round-trips `is_meeting_request`; migration idempotent.
- `meeting_event_id`: a message-with-expanded-event route returns the event id;
  a non-invite (no `event`) returns `None` (testserver route mirroring an
  existing GET test).
- Engine `RespondMeeting`: with an event-id-resolving route + an accept route,
  emits `MeetingResponded` and the POST hits `/events/{id}/accept` (mirror the
  existing `respond_event`/`save_attachment` engine tests); a `None`-event
  message emits `Error`.

**lookxy (unit):**
- `respond_meeting(Accept)` on a message with `is_meeting_request = true` sends
  `RespondMeeting { kind: Accept }`; on a non-invite message sends nothing.
- The `A`/`D`/`T` keys route to `respond_meeting` only when the opened message
  is an invite (and don't shadow lowercase behaviors).
- The reader renders the `📅 Meeting invite` banner for an invite message and
  not for an ordinary one (`TestBackend`, mirroring existing reader render
  tests).
- `MeetingResponded` produces the confirmation notice and marks the message
  read.

## Scope boundaries (YAGNI)

- **Invites only** — no handling of response/cancellation messages beyond
  suppressing the affordance.
- **No comment / no "propose new time" / no free-busy check.**
- **No rich event-detail banner** — flag-driven; the body carries the details.
- **No change** to the calendar-side RSVP (`RespondEvent`) or to ordinary mail
  keys.
