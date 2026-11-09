//! Turns one queued `store::OutboxOp` into the matching `GraphClient` call.
//!
//! This is deliberately the smallest possible layer between the store's
//! outbox queue and the Graph client: no retry policy, no backoff, no
//! attempt bookkeeping — that's the drain loop's job (a later task), which
//! calls `apply_op` for each `store::OutboxRow` in `Store::pending_ops()`
//! order and, on success, calls `Store::drop_op`; on failure, calls
//! `Store::bump_op_attempts` with the error's `Display` text.
//!
//! `SaveDraft`/`SendDraft` are the exception to "no store access here": pushing
//! a draft to Graph needs its currently-stored subject/recipients/body, and a
//! draft still addressed by a local (`local:`) id has to be reconciled to the
//! Graph-minted id the moment it's created there — otherwise nothing else
//! would ever do it (Graph never reports a `local:` id back through delta
//! sync, so there's no later point where this reconciliation would happen
//! for free). So `apply_op` takes a `&Store` too, used by those two ops —
//! plus `CreateEvent`/`UpdateEvent`, which need the same store access for
//! the same reason: `event_input_for` reads the event's currently-stored
//! fields to build the `EventInput` Graph is sent, and `CreateEvent`
//! reconciles a `local:` id to its Graph id afterward exactly like
//! `SaveDraft`/`SendDraft` do for drafts.
//!
//! The message ops (`MarkRead`, `SetFlag`, `SetCategories`, `Move`,
//! `Delete`, and #2054's `DismissMessageReminder` / `SnoozeMessageReminder`)
//! read the store only to resolve their id (`Store::current_id`) at apply
//! time, so an op queued with an id an earlier op moved reaches the
//! message (#1896). `Delete` and the two reminder ops send nothing for an
//! id that still resolves to `local:` (never on Graph). `Move` hands the id Graph minted back to the
//! caller ([`Applied::Moved`]), which re-keys the store: a store failure
//! there must not turn into a retry of a move Graph already made.

use crate::graph::client::{
    EventFieldSet, EventInput, GraphClient, GraphError, HTTP_ERROR_TEXT_MAX, RsvpKind,
    UploadObserver, graph_error_message, one_line,
};
use crate::graph::model::Recipient;
use crate::store::{OutboundAttachment, OutboxOp, Store};

/// What an applied op left for the caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// Nothing more.
    Done,
    /// A `Move` reached Graph: the message Graph knew as `old_id` (the op's
    /// id, resolved) is `new_id` now. The caller re-keys the store
    /// (`Store::follow_move`) and reports it (#1896).
    Moved { old_id: String, new_id: String },
    /// A `CreateContact` reached Graph and the store re-keyed the contact
    /// created as `local_id` to `id` (#2063). The caller reports it.
    ContactCreated { local_id: String, id: String },
}

/// The id the message `id` names is stored under now (#1884, #1896).
fn resolve(store: &Store, id: &str) -> Result<String, GraphError> {
    store
        .current_id(id)
        .map_err(|e| GraphError::Parse(e.to_string()))
}

/// Dispatches `op` to the Graph mutation it represents.
///
/// A message op addresses the message under the id it is stored as now
/// (`Store::current_id`). `Move` returns [`Applied::Moved`] with the id
/// Graph minted; this function does not write it to the store.
pub fn apply_op(client: &GraphClient, store: &Store, op: &OutboxOp) -> Result<Applied, GraphError> {
    match op {
        OutboxOp::Move { id, dest } => {
            let old_id = resolve(store, id)?;
            let new_id = client.move_message(&old_id, dest)?;
            Ok(Applied::Moved { old_id, new_id })
        }
        OutboxOp::CreateContact { id } => create_contact(client, store, id),
        op => apply_other(client, store, op).map(|()| Applied::Done),
    }
}

/// A store error while applying a contact op, as the drain's `Parse`.
fn store_err(e: crate::store::StoreError) -> GraphError {
    GraphError::Parse(e.to_string())
}

/// `CreateContact` (#2063): POST the `local:` row as stored now, then re-key
/// it. A row deleted before the drain sends nothing, nor does an id that
/// already resolves to a Graph id (the re-key committed and the op was not
/// dropped: a crash, or a retry).
fn create_contact(client: &GraphClient, store: &Store, id: &str) -> Result<Applied, GraphError> {
    if store.resolve_contact_id(id).map_err(store_err)? != id {
        return Ok(Applied::Done);
    }
    let Some(row) = store.folder_contact(id).map_err(store_err)? else {
        return Ok(Applied::Done);
    };
    let created = client.create_contact(&row.fields)?;
    store
        .reconcile_created_contact(id, &created)
        .map_err(store_err)?;
    Ok(Applied::ContactCreated {
        local_id: id.to_string(),
        id: created.id,
    })
}

/// `apply_op` for every op but `Move`.
fn apply_other(client: &GraphClient, store: &Store, op: &OutboxOp) -> Result<(), GraphError> {
    match op {
        OutboxOp::MarkRead { id, read } => client.mark_read(&resolve(store, id)?, *read),
        OutboxOp::SetFlag { id, flagged } => client.set_flag(&resolve(store, id)?, *flagged),
        OutboxOp::SetCategories { id, categories } => {
            client.set_message_categories(&resolve(store, id)?, categories)
        }
        OutboxOp::Move { .. } => unreachable!("apply_op handles Move"),
        OutboxOp::Delete { id } => {
            // A reconciled draft's `local:` id is deleted under its Graph id
            // (#1884), a moved message's old id under its new one (#1896).
            // Graph soft-deletes it to Recoverable Items, not to Deleted
            // Items (a `Move` to that folder). An id that still resolves to
            // `local:` is treated as
            // never having reached Graph, so nothing is deleted there (as
            // `DeleteEvent`). That is not an invariant: a delete on another
            // connection that lands while this engine's `create_draft` is in
            // flight leaves `reconcile_id` no row to rename, and the Graph
            // draft stays for a later sync to bring back.
            let target = resolve(store, id)?;
            if target.starts_with("local:") {
                Ok(())
            } else {
                client.delete_message(&target)
            }
        }
        OutboxOp::SaveDraft { id } => ensure_draft_on_graph(client, store, id).map(|_id| ()),
        OutboxOp::SendDraft { id } => apply_send(client, store, id, None, &mut |_, _, _| {}),
        OutboxOp::RespondEvent {
            id,
            kind,
            comment,
            send_response,
            proposed_start_utc,
            proposed_end_utc,
        } => {
            // An unrecognized `kind` must NOT silently fall back to
            // accepting: that's the worst possible default for an action
            // with an external, user-visible side effect (a corrupt outbox
            // row would otherwise accept a meeting no one asked to accept).
            // Erroring here — rather than defaulting — makes the drain
            // loop's normal 4xx retry/quarantine policy handle it: the op
            // is retried, then quarantined after `MAX_OP_ATTEMPTS` like any
            // other op Graph keeps rejecting, without ever calling Graph
            // with a guessed action.
            let rsvp = RsvpKind::from_status(kind)
                .ok_or_else(|| GraphError::Parse(format!("unrecognized RSVP kind: {kind}")))?;
            let proposed = proposed_start_utc.clone().zip(proposed_end_utc.clone());
            client.respond_event(id, rsvp, comment.as_deref(), *send_response, proposed)
        }
        OutboxOp::CreateEvent { id } => {
            // Every group, read at send time, so an edit of the `local:`
            // event before the drain is carried too; no reminder when the
            // host set none, so Graph applies the mailbox default (#1994).
            let mut fields = EventFieldSet::ALL;
            let (mut input, reminder_unset) = event_input_for(store, id)?;
            if reminder_unset {
                fields.remove(EventFieldSet::REMINDER);
            }
            input.fields = fields;
            // The calendar the event was filed in (#2048): `/me/events` for
            // the default, as before.
            let calendar = store
                .event_create_calendar(id)
                .map_err(|e| GraphError::Parse(e.to_string()))?;
            let created = match calendar {
                Some(cal) => client.create_event_in(&cal, &input)?,
                None => client.create_event(&input)?,
            };
            store
                .reconcile_created_event(id, &created)
                .map_err(|e| GraphError::Parse(e.to_string()))?;
            Ok(())
        }
        OutboxOp::UpdateEvent { id, fields } => {
            // Only the groups the edit changed (#1994), each with its latest
            // stored value. An edit that changed nothing sends nothing.
            if fields.is_empty() {
                return Ok(());
            }
            let (mut input, _) = event_input_for(store, id)?;
            input.fields = *fields;
            let updated = client.update_event(id, &input)?;
            // An edit that turned the online meeting on: Graph's answer holds
            // the join link. The PATCH already happened, so a store error is
            // not a failed op (the next refresh corrects the row).
            if let Some(ev) = updated.filter(|_| fields.contains(EventFieldSet::ONLINE_MEETING)) {
                let _ =
                    store.set_event_online_meeting(id, &ev.join_url, &ev.online_meeting_provider);
            }
            Ok(())
        }
        OutboxOp::DeleteEvent { id } => {
            // A local:-only event never reached Graph; nothing to delete there.
            if id.starts_with("local:") {
                Ok(())
            } else {
                client.delete_event(id)
            }
        }
        OutboxOp::CancelEvent { id, comment } => {
            // A `local:` event never reached Graph, so nobody was invited.
            if id.starts_with("local:") {
                return Ok(());
            }
            // A 404 is returned as is: the drain drops the op and resets
            // the calendars (#1985), as only Graph knows whether the cancel
            // went out.
            client.cancel_event(id, comment.as_deref())
        }
        // Nothing local to reconcile. A retry after a lost 202 forwards
        // twice: Graph has nothing to ask whether the first one went out.
        OutboxOp::ForwardEvent { id, to, comment } => {
            client.forward_event(id, to, comment.as_deref())
        }
        // A series edit (#2022) sends the body built at command time: the
        // master was not stored when #2022 added this, and still may not be
        // (a calendar pass stores it only when a round returns or fetches
        // it, #2064). A `local:` series is edited through its row
        // (`UpdateEvent`, or its pending `CreateEvent`), so nothing goes.
        OutboxOp::UpdateEventSeries { id, body } => {
            if id.starts_with("local:") {
                return Ok(());
            }
            client.update_event_body(id, body.clone()).map(|_| ())
        }
        // A reminder (#2025): nothing local to reconcile. The engine refuses
        // a `local:` id, so only a hand-edited row holds one, and it sends
        // nothing. A 404 is returned as is: the drain drops the op.
        OutboxOp::DismissReminder { id } => {
            if id.starts_with("local:") {
                return Ok(());
            }
            client.dismiss_reminder(id)
        }
        OutboxOp::SnoozeReminder {
            id,
            new_reminder_utc,
        } => {
            if id.starts_with("local:") {
                return Ok(());
            }
            client.snooze_reminder(id, new_reminder_utc)
        }
        // A flagged message's reminder (#2054), under the id the message is
        // stored as now (a move before the drain, #1896). The engine refuses
        // a `local:` id; one that still resolves to `local:` was never on
        // Graph and sends nothing.
        OutboxOp::DismissMessageReminder { id } => {
            let target = resolve(store, id)?;
            if target.starts_with("local:") {
                return Ok(());
            }
            client.dismiss_message_reminder(&target)
        }
        OutboxOp::SnoozeMessageReminder {
            id,
            new_reminder_utc,
        } => {
            let target = resolve(store, id)?;
            if target.starts_with("local:") {
                return Ok(());
            }
            client.snooze_message_reminder(&target, new_reminder_utc)
        }
        OutboxOp::CreateContact { .. } => unreachable!("apply_op handles CreateContact"),
        // Only the properties the edit changed (#2063), each with its value
        // at send time. An id still `local:` after resolving has no Graph
        // contact (its create was refused, and the row went with it); a row
        // gone (deleted since) has nothing to send: its `DeleteContact`
        // follows.
        OutboxOp::UpdateContact { id, fields } => {
            if fields.is_empty() {
                return Ok(());
            }
            let target = store.resolve_contact_id(id).map_err(store_err)?;
            if target.starts_with("local:") {
                return Ok(());
            }
            let Some(row) = store.folder_contact(&target).map_err(store_err)? else {
                return Ok(());
            };
            client.update_contact(&target, &row.fields, *fields)
        }
        // A 404 is done: the contact is already gone (#2063).
        OutboxOp::DeleteContact { id } => {
            let target = store.resolve_contact_id(id).map_err(store_err)?;
            if target.starts_with("local:") {
                return Ok(());
            }
            match client.delete_contact(&target) {
                Err(GraphError::NotFound) => Ok(()),
                r => r,
            }
        }
    }
}

/// Sends the draft `id`: `SaveDraft`'s create or PATCH, then each pending
/// attachment, then the `/send` POST. `seq` is the queued op's outbox row
/// (`None` for a call outside the outbox, which tracks nothing).
///
/// With a `seq`, `outbox.send_posted` is set right before the POST (#1874),
/// so an answer that may follow a delivery (any 5xx, a 429/503 throttle
/// included: a gateway 503 can follow a send the back end accepted), a
/// dropped connection or a crash counts; only a refusal Graph answered (a
/// 4xx, a 404, a 401) clears it, as nothing went out. A later attempt of a
/// posted op first asks whether it went out, before any PATCH or upload:
/// the local row gone (a Drafts delta's `@removed`), or Graph saying the
/// draft id is gone (404) or no draft any more, is a delivered send (`Ok`).
/// When Graph answered, the attachment rows are cleared; when the row is
/// gone they stay behind (a `local:` id no longer resolves to the Graph id
/// they are keyed by), which is harmless: nothing reads them. Graph saying
/// it is still a draft clears the flag and sends it again. A failure of
/// that check is returned as is: the drain keeps a retryable one queued and
/// never returns a posted op to Drafts.
///
/// A file-read or upload error returns before the POST, so the drain's
/// failure policy applies (a 5xx defers the op; Graph's own 401 for the
/// bearer token, a 429/503 throttle or a transport failure keeps it queued;
/// any other failure returns the draft to Drafts, an upload session url's
/// 401 or 404 included, as they come back as `Http`, #1941) and the
/// attachments are NOT cleared. Each upload Graph
/// confirmed is marked `uploaded` at once, and one already marked is
/// skipped, so a confirmed upload is never repeated. An upload whose answer
/// was lost (a 5xx or a dropped connection after Graph stored it), or whose
/// marker could not be written, is uploaded again by the next attempt.
///
/// `progress` hears each attachment's upload as it runs (#1941): the row,
/// then the bytes Graph has and the total, once for a small attachment
/// after its POST, and after each accepted chunk of an upload session (a
/// resumed one first reports where it goes on from). A skipped (uploaded)
/// row reports nothing. An upload session's url is stored on the row with
/// the digest of what it uploads (`upload_digest`: name, content type,
/// content id and bytes), and a later attempt resumes the session only
/// while the digest matches: a file edited between attempts is uploaded
/// from byte 0 in a new session, never spliced into the old one. A store
/// error writing or clearing the session only loses the resume.
pub(crate) fn apply_send(
    client: &GraphClient,
    store: &Store,
    id: &str,
    seq: Option<i64>,
    progress: &mut dyn FnMut(&OutboundAttachment, u64, u64),
) -> Result<(), GraphError> {
    let store_err = |e: crate::store::StoreError| GraphError::Parse(e.to_string());
    if let Some(seq) = seq
        && store.send_posted(seq).map_err(store_err)?
    {
        let Some((row, _)) = store.draft(id).map_err(store_err)? else {
            return Ok(());
        };
        // Still `local:`: never on Graph, so never sent (a send queued by an
        // older engine reads as posted). No check: send it as usual.
        if !row.id.starts_with("local:") && !client.is_draft(&row.id)? {
            let _ = store.clear_outbound_attachments(&row.id);
            return Ok(());
        }
        store.set_send_posted(seq, false).map_err(store_err)?;
    }
    let graph_id = ensure_draft_on_graph(client, store, id)?;
    for att in store.outbound_attachments(&graph_id).map_err(store_err)? {
        if att.uploaded {
            continue;
        }
        let bytes = if att.from_bytes {
            // A row removed between the listing and here fails like a
            // missing file.
            store
                .outbound_attachment_bytes(&graph_id, &att.path)
                .map_err(store_err)?
                .ok_or_else(|| {
                    GraphError::Parse(format!("attachment {} is no longer there", att.name))
                })?
        } else {
            std::fs::read(&att.path).map_err(|e| {
                GraphError::Parse(format!("cannot read attachment {}: {e}", att.path))
            })?
        };
        let content_type = att
            .content_type
            .clone()
            .unwrap_or_else(|| content_type_for(&att.name));
        let digest = upload_digest(&att.name, &content_type, att.content_id.as_deref(), &bytes);
        let resume_url = match (&att.upload_url, &att.upload_digest) {
            (Some(url), Some(stored)) if *stored == digest => Some(url.as_str()),
            _ => None,
        };
        let mut obs = SendObserver {
            store,
            graph_id: &graph_id,
            att: &att,
            digest: &digest,
            progress: &mut *progress,
        };
        client.upload_attachment(
            &graph_id,
            &att.name,
            &content_type,
            &bytes,
            att.content_id.as_deref(),
            resume_url,
            &mut obs,
        )?;
        store
            .mark_outbound_attachment_uploaded(&graph_id, &att.path)
            .map_err(store_err)?;
    }
    if let Some(seq) = seq {
        store.set_send_posted(seq, true).map_err(store_err)?;
    }
    if let Err(e) = client.send_draft(&graph_id) {
        // Graph answered and refused it (a 4xx, a 404, a 401): it was not
        // sent, so the failure is the send's own. A 5xx, a 429/503 throttle
        // (a gateway 503 can follow a send the back end accepted) or a lost
        // answer leaves the flag: the next attempt checks delivery first,
        // at the cost of one GET.
        let refused = matches!(
            e,
            GraphError::Http { status, .. } if status < 500
        ) || matches!(e, GraphError::NotFound | GraphError::Unauthorized);
        if let (Some(seq), true) = (seq, refused) {
            let _ = store.set_send_posted(seq, false);
        }
        return Err(e);
    }
    // The send happened: from here nothing may read as a failed send, which
    // would return a delivered message to Drafts (#1874). A store error
    // leaves the attachment rows behind, which is harmless.
    let _ = store.clear_outbound_attachments(&graph_id);
    Ok(())
}

/// `apply_send`'s observer of one attachment's upload (#1941): stores the
/// upload session on the row (with the digest of what it uploads), forgets
/// one Graph refused, and passes progress on.
struct SendObserver<'a> {
    store: &'a Store,
    graph_id: &'a str,
    att: &'a OutboundAttachment,
    digest: &'a str,
    progress: &'a mut dyn FnMut(&OutboundAttachment, u64, u64),
}

impl UploadObserver for SendObserver<'_> {
    fn session(&mut self, upload_url: Option<&str>) {
        let session = upload_url.map(|url| (url, self.digest));
        let _ = self.store.set_outbound_attachment_upload_session(
            self.graph_id,
            &self.att.path,
            session,
        );
    }

    fn progress(&mut self, sent: u64, total: u64) {
        (self.progress)(self.att, sent, total);
    }
}

/// The fingerprint of what an upload session uploads (#1941): what its
/// `AttachmentItem` names (name, content type, content id) and the bytes, as
/// `"{len}:{FNV-1a 64 hex}"`. Each text field is hashed with a byte before
/// it saying whether it is there and one after it that no UTF-8 text holds,
/// so no two field lists hash the same input. Not cryptographic: it tells
/// an unchanged attachment from an edited one, which no attacker controls.
fn upload_digest(name: &str, content_type: &str, content_id: Option<&str>, bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = OFFSET;
    let mut feed = |data: &[u8]| {
        for b in data {
            h ^= u64::from(*b);
            h = h.wrapping_mul(PRIME);
        }
    };
    for field in [Some(name), Some(content_type), content_id] {
        match field {
            Some(text) => {
                feed(&[1]);
                feed(text.as_bytes());
            }
            None => feed(&[0]),
        }
        feed(&[0xff]);
    }
    feed(bytes);
    format!("{}:{h:016x}", bytes.len())
}

/// The text a failed send reports in `SyncEvent::SendFailed` (#1874): Graph's
/// own `error.message` for an HTTP error whose body carries a non-empty one
/// (as one line, capped like `GraphError`'s `Display`, #1954), else
/// `HTTP {status}`; a `Parse` failure's message as is (a missing
/// attachment file, a store error); any other error's `Display`.
pub(crate) fn send_error_text(err: &GraphError) -> String {
    match err {
        GraphError::Http { status, body } => graph_error_message(body)
            .map(|m| one_line(&m, HTTP_ERROR_TEXT_MAX))
            .unwrap_or_else(|| format!("HTTP {status}")),
        GraphError::Parse(m) => m.clone(),
        other => other.to_string(),
    }
}

/// Reads the stored event `id` and builds the `EventInput` the create/update
/// calls take (with no `fields`: the caller sets them), and whether the event
/// was created without a reminder (`EventSendData::reminder_unset`). Errors
/// if the event isn't in the store (a corrupt/raced op).
fn event_input_for(store: &Store, id: &str) -> Result<(EventInput, bool), GraphError> {
    let d = store
        .event_for_send(id)
        .map_err(|e| GraphError::Parse(e.to_string()))?
        .ok_or_else(|| GraphError::Parse(format!("no local event stored for {id}")))?;
    let input = EventInput {
        subject: d.subject,
        start_utc: d.start_utc,
        end_utc: d.end_utc,
        is_all_day: d.is_all_day,
        location: d.location,
        attendees: d.attendees,
        body_html: d.body_html,
        recurrence: d.recurrence,
        series_recurrence: d.series_recurrence,
        show_as: d.show_as,
        sensitivity: d.sensitivity,
        importance: d.importance,
        categories: d.categories,
        is_reminder_on: d.is_reminder_on,
        reminder_minutes: d.reminder_minutes,
        is_online_meeting: d.is_online_meeting,
        online_meeting_provider: d.online_meeting_provider,
        response_requested: d.response_requested,
        allow_new_time_proposals: d.allow_new_time_proposals,
        start_time_zone: d.start_time_zone,
        start_local: d.start_local,
        end_time_zone: d.end_time_zone,
        end_local: d.end_local,
        fields: EventFieldSet::default(),
    };
    Ok((input, d.reminder_unset))
}

/// A best-effort MIME type from a file name's extension, defaulting to
/// `application/octet-stream`. Small built-in map — no new dependency.
fn content_type_for(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "txt" | "log" | "md" => "text/plain",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Ensures the draft addressed by `id` exists on Graph with its
/// currently-stored fields, returning the id it now lives under on Graph.
///
/// The draft is loaded with `Store::draft`, which also follows a `local:` id
/// already reconciled to its Graph id (#1884, #1881), and create vs update is
/// decided by the id it is stored under now (`row.id`), not by `id`. Still
/// `local:` (never yet pushed): `create_draft`s it with its stored
/// subject/recipients/body and reconciles the store (`Store::reconcile_id`)
/// to the Graph-minted id, which the rest of Graph (and the next `SendDraft`,
/// if this was a `SaveDraft`) now knows it by. Otherwise `update_draft`s the
/// Graph id in place with whatever is currently stored and returns it, so a
/// second `SaveDraft`/`SendDraft` with the same `local:` id patches the draft
/// rather than creating it twice.
///
/// Shared by `SaveDraft` (which only needs this) and `SendDraft` (which
/// needs it done first, then sends the result).
fn ensure_draft_on_graph(
    client: &GraphClient,
    store: &Store,
    id: &str,
) -> Result<String, GraphError> {
    // The unsaved-edit generation is read before the draft (#1871): an edit
    // compose makes after this read bumps it past, so the clear below
    // leaves that edit marked unsaved.
    let generation = store
        .local_edit_generation(id)
        .map_err(|e| GraphError::Parse(e.to_string()))?;
    let (row, body) = store
        .draft(id)
        .map_err(|e| GraphError::Parse(e.to_string()))?
        .ok_or_else(|| GraphError::Parse(format!("no local draft stored for {id}")))?;
    let to = parse_recipients(&row.to_recipients);
    let cc = parse_recipients(&row.cc_recipients);
    let bcc = parse_recipients(&row.bcc_recipients);

    if row.id.starts_with("local:") {
        let created = client.create_draft(&body.content, &row.subject, &to, &cc, &bcc)?;
        store
            .reconcile_id(&row.id, &created.id)
            .map_err(|e| GraphError::Parse(e.to_string()))?;
        mark_pushed(store, &created.id, generation);
        Ok(created.id)
    } else {
        client.update_draft(&row.id, &body.content, &row.subject, &to, &cc, &bcc)?;
        mark_pushed(store, &row.id, generation);
        Ok(row.id)
    }
}

/// Graph now has generation `generation` of the draft stored under `id`'s
/// fields (#1871): the row is clean unless compose edited it since. A store
/// error is ignored: the draft stays protected from fresh deltas until its
/// next push.
fn mark_pushed(store: &Store, id: &str, generation: i64) {
    if generation > 0 {
        let _ = store.clear_local_edit(id, generation);
    }
}

/// Parses the flat recipient text `MessageRow.to_recipients`/`cc_recipients`
/// holds for a draft back into `Recipient`s for `create_draft`/`update_draft`
/// — the exact inverse of `store::encode_recipients` (`"{name} <{addr}>"`
/// joined by `"; "`), which is what a reply/forward draft's columns look
/// like (`sync::engine::store_composed_draft` files it via
/// `Store::upsert_message`, same as any synced message). A from-scratch
/// local draft's columns, on the other hand, are whatever `update_draft_fields`
/// was given directly (compose's flat `to`/`cc` input, e.g. `bob@x` or
/// `bob@x, carol@x` — no `Name <addr>` structure, since compose never
/// captured a display name there).
///
/// Splits on `;` FIRST — that's the only separator `encode_recipients` ever
/// joins on, so it's always safe to split on. `,` is NOT safe to split on
/// unconditionally: the default corporate directory display-name format is
/// "Surname, Given" (e.g. `"Doe, John <john@x>"`), and splitting that on `,`
/// would wrongly produce a bogus, address-less `"Doe"` recipient alongside
/// `"John <john@x>"`. So each `;`-separated part is handled by
/// `parse_recipient_part`, which only treats `,` as a separator for the
/// bare-address (no `<...>`) shape, where it's the compose UI's own
/// separator for `"a@x, b@y"`-style typed input.
pub(crate) fn parse_recipients(raw: &str) -> Vec<Recipient> {
    raw.split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .flat_map(parse_recipient_part)
        .collect()
}

/// Parses one `;`-separated part into one or more `Recipient`s.
///
/// A part containing a `<...>`-wrapped address (`Name <addr>`, recognized
/// only when `<` is followed later by `>` — `part.find('<')` before
/// `part.rfind('>')`) is a synced message's or reply/forward draft's column
/// from `store::encode_recipients`, and is ALWAYS exactly one recipient —
/// the display name before `<` may itself contain a comma (`"Doe, John"`),
/// which is part of the name, not a separator.
///
/// A part with no `<...>` (including one with a lone `<` or `>`, which a
/// real address never contains) is the bare-address shape a from-scratch
/// local draft's flat compose input uses. There, a user may type several
/// addresses comma-separated into one field (`"a@x, b@y"`), so `,` IS a
/// separator: the part is split on it into one address-only `Recipient`
/// (empty name) per non-empty piece.
fn parse_recipient_part(part: &str) -> Vec<Recipient> {
    if let (Some(open), Some(close)) = (part.find('<'), part.rfind('>')) {
        if open < close {
            return vec![Recipient {
                name: part[..open].trim().to_string(),
                address: part[open + 1..close].trim().to_string(),
            }];
        }
    }
    part.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|addr| Recipient {
            name: String::new(),
            address: addr.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::client::ContactFieldSet;
    use crate::graph::model::{Body, MailFolder, Message};
    use crate::json::{self, Value};
    use crate::testserver::{FakeServer, Route};

    #[test]
    fn apply_op_dispatches_mark_read() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::MarkRead {
                id: "M1".into(),
                read: true,
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].body, r#"{"isRead":true}"#);
    }

    #[test]
    fn apply_op_dispatches_set_flag() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::SetFlag {
                id: "M1".into(),
                flagged: true,
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].body, r#"{"flag":{"flagStatus":"flagged"}}"#);
    }

    #[test]
    fn apply_op_dispatches_set_categories() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::SetCategories {
                id: "M1".into(),
                categories: vec!["Work".into()],
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH");
        assert!(reqs[0].path.contains("/me/messages/M1"));
    }

    #[test]
    fn apply_op_move_returns_the_new_id() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/move".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"M1-NEW"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let result = apply_op(
            &c,
            &store,
            &OutboxOp::Move {
                id: "M1".into(),
                dest: "DEST".into(),
            },
        );
        assert_eq!(
            result,
            Ok(Applied::Moved {
                old_id: "M1".into(),
                new_id: "M1-NEW".into()
            })
        );
        let reqs = srv.requests();
        assert_eq!(reqs[0].body, r#"{"destinationId":"DEST"}"#);
        // The store is the caller's to re-key.
        assert_eq!(store.current_id("M1").unwrap(), "M1");
    }

    /// A store holding `OLD1`, re-keyed to `NEW1` by a move (#1896).
    fn store_moved_old1_to_new1() -> Store {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_folder(&MailFolder {
                id: "F".into(),
                display_name: "F".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: None,
            })
            .unwrap();
        store
            .upsert_message(
                "F",
                &Message {
                    id: "OLD1".into(),
                    conversation_id: String::new(),
                    subject: String::new(),
                    from: Recipient::default(),
                    to: vec![],
                    cc: vec![],
                    received: String::new(),
                    sent: String::new(),
                    is_read: false,
                    is_flagged: false,
                    has_attachments: false,
                    importance: String::new(),
                    preview: String::new(),
                    is_draft: false,
                    is_meeting_request: false,
                    categories: vec![],
                    is_reminder_on: false,
                    reminder_at: String::new(),
                    reminder_signal_at: String::new(),
                    flag_due_at: String::new(),
                },
            )
            .unwrap();
        assert!(store.follow_move("OLD1", "NEW1").unwrap());
        store
    }

    #[test]
    fn apply_op_mark_read_of_a_moved_id_patches_the_new_id() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/NEW1".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = store_moved_old1_to_new1();
        let result = apply_op(
            &c,
            &store,
            &OutboxOp::MarkRead {
                id: "OLD1".into(),
                read: true,
            },
        );
        assert_eq!(result, Ok(Applied::Done));
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].path, "/me/messages/NEW1");
    }

    #[test]
    fn apply_op_move_of_a_moved_id_posts_to_the_new_id() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/NEW1/move".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"NEW2"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = store_moved_old1_to_new1();
        let result = apply_op(
            &c,
            &store,
            &OutboxOp::Move {
                id: "OLD1".into(),
                dest: "F".into(),
            },
        );
        assert_eq!(
            result,
            Ok(Applied::Moved {
                old_id: "NEW1".into(),
                new_id: "NEW2".into()
            })
        );
        assert_eq!(srv.requests()[0].path, "/me/messages/NEW1/move");
    }

    #[test]
    fn apply_op_dispatches_delete() {
        let srv = FakeServer::start(vec![Route {
            method: "DELETE".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 204,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(&c, &store, &OutboxOp::Delete { id: "M1".into() }).unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "DELETE");
    }

    #[test]
    fn apply_op_dispatches_respond_event_accept_with_comment() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/E1/accept".into(),
            status: 202,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::RespondEvent {
                id: "E1".into(),
                kind: "accepted".into(),
                comment: Some("looking forward to it".into()),
                send_response: true,
                proposed_start_utc: None,
                proposed_end_utc: None,
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "POST");
        assert!(reqs[0].path.ends_with("/accept"));
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(
            sent.get("comment").and_then(Value::as_str),
            Some("looking forward to it")
        );
        assert_eq!(
            sent.get("sendResponse").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn apply_op_dispatches_respond_event_decline_maps_to_the_right_action() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/E1/decline".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::RespondEvent {
                id: "E1".into(),
                kind: "declined".into(),
                comment: None,
                send_response: true,
                proposed_start_utc: None,
                proposed_end_utc: None,
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert!(reqs[0].path.ends_with("/decline"));
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(sent.get("comment").and_then(Value::as_str), Some(""));
    }

    /// "Don't send a response" (#2023): every kind's POST carries
    /// `sendResponse: false`.
    #[test]
    fn apply_op_respond_event_without_sending_a_response() {
        let actions = [
            ("accepted", "accept"),
            ("declined", "decline"),
            ("tentativelyAccepted", "tentativelyAccept"),
        ];
        let srv = FakeServer::start(
            actions
                .iter()
                .map(|(_, action)| Route {
                    method: "POST".into(),
                    path_prefix: format!("/me/events/E1/{action}"),
                    status: 202,
                    headers: vec![],
                    body: "".into(),
                })
                .collect(),
        );
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        for (kind, _) in actions {
            apply_op(
                &c,
                &store,
                &OutboxOp::RespondEvent {
                    id: "E1".into(),
                    kind: kind.into(),
                    comment: None,
                    send_response: false,
                    proposed_start_utc: None,
                    proposed_end_utc: None,
                },
            )
            .unwrap();
        }
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 3);
        for (req, (_, action)) in reqs.iter().zip(actions) {
            assert_eq!(req.path, format!("/me/events/E1/{action}"));
            let sent = json::parse(&req.body).unwrap();
            assert_eq!(
                sent.get("sendResponse").and_then(Value::as_bool),
                Some(false),
                "{action}"
            );
        }
    }

    #[test]
    fn apply_op_respond_event_rejects_an_unrecognized_kind_without_calling_graph() {
        // A corrupt/unexpected outbox row (`kind` not one of the three
        // `Store::set_event_response` vocabulary values) must NOT default to
        // accepting the meeting — see `RsvpKind::from_status`'s doc
        // comment. No route is stubbed at all: if this silently defaulted to `Accept` and called
        // `.../accept`, the fake server would 404 and the test would still
        // catch it via the empty-requests assertion below, but the point is
        // Graph is never even reached.
        let srv = FakeServer::start(vec![]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let result = apply_op(
            &c,
            &store,
            &OutboxOp::RespondEvent {
                id: "E1".into(),
                kind: "garbage".into(),
                comment: None,
                send_response: true,
                proposed_start_utc: None,
                proposed_end_utc: None,
            },
        );
        assert!(matches!(result, Err(GraphError::Parse(_))));
        assert!(srv.requests().is_empty());
    }

    #[test]
    fn apply_op_propagates_graph_error() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 404,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let result = apply_op(
            &c,
            &store,
            &OutboxOp::MarkRead {
                id: "M1".into(),
                read: true,
            },
        );
        assert_eq!(result, Err(GraphError::NotFound));
    }

    /// A Graph draft JSON body shaped like `create_draft`'s response —
    /// `isDraft` matters here (unlike the other fixtures in this file) since
    /// these tests round-trip a draft through the store afterward.
    fn draft_json(id: &str, subject: &str) -> String {
        format!(
            r#"{{"id":"{id}","conversationId":"C1","subject":"{subject}",
            "from":{{"emailAddress":{{"name":"","address":""}}}},
            "toRecipients":[],"ccRecipients":[],
            "receivedDateTime":"","sentDateTime":"","isRead":false,
            "hasAttachments":false,"importance":"normal","bodyPreview":"",
            "isDraft":true}}"#
        )
    }

    #[test]
    fn apply_op_save_draft_creates_and_reconciles_a_local_draft() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages".into(),
            status: 201,
            headers: vec![],
            body: draft_json("GRAPH-1", "Hi"),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store
            .create_local_draft("Hi", "bob@x", "carol@x", "<p>hello</p>")
            .unwrap();

        apply_op(
            &c,
            &store,
            &OutboxOp::SaveDraft {
                id: local_id.clone(),
            },
        )
        .unwrap();

        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/messages");
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(sent.get("subject").and_then(Value::as_str), Some("Hi"));
        let to = sent.get("toRecipients").and_then(Value::as_array).unwrap();
        assert_eq!(
            to[0]
                .get("emailAddress")
                .and_then(|e| e.get("address"))
                .and_then(Value::as_str),
            Some("bob@x")
        );
        let cc = sent.get("ccRecipients").and_then(Value::as_array).unwrap();
        assert_eq!(
            cc[0]
                .get("emailAddress")
                .and_then(|e| e.get("address"))
                .and_then(Value::as_str),
            Some("carol@x")
        );

        // Reconciled: the draft is stored under the Graph id, and the local
        // id still reaches it (#1884).
        let (row, _) = store.draft(&local_id).unwrap().unwrap();
        assert_eq!(row.id, "GRAPH-1");
        let (row, body) = store.draft("GRAPH-1").unwrap().unwrap();
        assert_eq!(row.subject, "Hi");
        assert_eq!(body.content, "<p>hello</p>");
    }

    /// A draft compose edited, so its fields are unsaved (#1871).
    fn edited_draft(store: &Store) -> String {
        let local_id = store.create_local_draft("Old", "b@x", "", "old").unwrap();
        store
            .update_draft_fields(&local_id, "Edited", "b@x", "", "", "<p>new</p>")
            .unwrap();
        assert_eq!(store.local_edit_generation(&local_id).unwrap(), 1);
        local_id
    }

    #[test]
    fn a_successful_push_leaves_the_draft_clean() {
        // The create path (a local draft) and the update path (synced).
        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages".into(),
                status: 201,
                headers: vec![],
                body: draft_json("GRAPH-1", "Edited"),
            },
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/messages/GRAPH-5".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let created = edited_draft(&store);
        apply_op(
            &c,
            &store,
            &OutboxOp::SaveDraft {
                id: created.clone(),
            },
        )
        .unwrap();
        assert_eq!(store.current_id(&created).unwrap(), "GRAPH-1");
        assert_eq!(store.local_edit_generation("GRAPH-1").unwrap(), 0);

        let synced = edited_draft(&store);
        store.reconcile_id(&synced, "GRAPH-5").unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::SaveDraft {
                id: "GRAPH-5".into(),
            },
        )
        .unwrap();
        assert_eq!(store.local_edit_generation("GRAPH-5").unwrap(), 0);
        assert!(store.locally_edited_ids().unwrap().is_empty());
    }

    #[test]
    fn an_edit_made_during_the_push_stays_unsaved() {
        // The PATCH is held 600 ms; 200 ms in, compose saves again on its
        // own connection. The push read generation 1 and sent it, so the
        // clear must not match generation 2.
        let srv = FakeServer::start_paced(vec![(
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/messages/GRAPH-5".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
            None,
            std::time::Duration::from_millis(600),
        )]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let dir = std::env::temp_dir().join(format!(
            "lookxy-outbox-push-race-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mail.db");
        let store = Store::open(&path).unwrap();
        let synced = edited_draft(&store);
        store.reconcile_id(&synced, "GRAPH-5").unwrap();
        let compose = {
            let path = path.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(200));
                Store::open(&path)
                    .unwrap()
                    .update_draft_fields("GRAPH-5", "Later", "b@x", "", "", "<p>later</p>")
                    .unwrap();
            })
        };
        apply_op(
            &c,
            &store,
            &OutboxOp::SaveDraft {
                id: "GRAPH-5".into(),
            },
        )
        .unwrap();
        compose.join().unwrap();
        assert_eq!(store.locally_edited_ids().unwrap(), ["GRAPH-5"]);
        assert_eq!(store.local_edit_generation("GRAPH-5").unwrap(), 2);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_push_leaves_the_draft_unsaved() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/GRAPH-5".into(),
            status: 500,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let synced = edited_draft(&store);
        store.reconcile_id(&synced, "GRAPH-5").unwrap();
        assert!(
            apply_op(
                &c,
                &store,
                &OutboxOp::SaveDraft {
                    id: "GRAPH-5".into()
                }
            )
            .is_err()
        );
        assert_eq!(store.locally_edited_ids().unwrap(), ["GRAPH-5"]);
    }

    #[test]
    fn apply_op_save_draft_patches_an_already_synced_draft() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/GRAPH-5".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store
            .create_local_draft("Old", "b@x", "", "old body")
            .unwrap();
        store.reconcile_id(&local_id, "GRAPH-5").unwrap();

        apply_op(
            &c,
            &store,
            &OutboxOp::SaveDraft {
                id: "GRAPH-5".into(),
            },
        )
        .unwrap();

        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(reqs[0].path, "/me/messages/GRAPH-5");
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(sent.get("subject").and_then(Value::as_str), Some("Old"));

        // Still addressable under the same (already-Graph) id — no reconcile.
        let (row, body) = store.draft("GRAPH-5").unwrap().unwrap();
        assert_eq!(row.subject, "Old");
        assert_eq!(body.content, "old body");
    }

    #[test]
    fn apply_op_send_draft_of_a_local_draft_creates_then_sends() {
        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/GRAPH-9/send".into(),
                status: 202,
                headers: vec![],
                body: "".into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages".into(),
                status: 201,
                headers: vec![],
                body: draft_json("GRAPH-9", "Hi"),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store
            .create_local_draft("Hi", "bob@x", "", "<p>hello</p>")
            .unwrap();

        apply_op(
            &c,
            &store,
            &OutboxOp::SendDraft {
                id: local_id.clone(),
            },
        )
        .unwrap();

        let reqs = srv.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].path, "/me/messages");
        assert!(reqs[1].path.starts_with("/me/messages/GRAPH-9/send"));

        let (row, _) = store.draft(&local_id).unwrap().unwrap();
        assert_eq!(row.id, "GRAPH-9");
        assert!(store.draft("GRAPH-9").unwrap().is_some());
    }

    #[test]
    fn apply_op_send_draft_of_an_already_synced_draft_patches_then_sends() {
        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/GRAPH-7/send".into(),
                status: 202,
                headers: vec![],
                body: "".into(),
            },
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/messages/GRAPH-7".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store.create_local_draft("Hi", "b@x", "", "body").unwrap();
        store.reconcile_id(&local_id, "GRAPH-7").unwrap();

        apply_op(
            &c,
            &store,
            &OutboxOp::SendDraft {
                id: "GRAPH-7".into(),
            },
        )
        .unwrap();

        let reqs = srv.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].method, "PATCH");
        assert!(reqs[1].path.starts_with("/me/messages/GRAPH-7/send"));
    }

    #[test]
    fn send_draft_uploads_pending_attachments_then_clears_them() {
        // temp file to attach
        let dir = std::env::temp_dir().join(format!("lookxy-att-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("note.txt");
        std::fs::write(&file, b"hello").unwrap();

        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/GRAPH-1/send".into(),
                status: 202,
                headers: vec![],
                body: "".into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/GRAPH-1/attachments".into(),
                status: 201,
                headers: vec![],
                body: "{}".into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages".into(),
                status: 201,
                headers: vec![],
                body: draft_json("GRAPH-1", "Hi"),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store
            .create_local_draft("Hi", "bob@x", "", "<p>hello</p>")
            .unwrap();
        store
            .add_outbound_attachment(&local_id, file.to_str().unwrap(), "note.txt", 5)
            .unwrap();

        apply_op(
            &c,
            &store,
            &OutboxOp::SendDraft {
                id: local_id.clone(),
            },
        )
        .unwrap();

        // the attachment POST was made, and the pending rows are cleared after send:
        assert!(
            srv.requests()
                .iter()
                .any(|r| r.path.contains("/attachments"))
        );
        assert!(store.outbound_attachments("GRAPH-1").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_draft_errors_and_keeps_attachments_when_a_file_is_missing() {
        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/GRAPH-2/send".into(),
                status: 202,
                headers: vec![],
                body: "".into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages".into(),
                status: 201,
                headers: vec![],
                body: draft_json("GRAPH-2", "Hi"),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store
            .create_local_draft("Hi", "bob@x", "", "<p>hello</p>")
            .unwrap();
        store
            .add_outbound_attachment(&local_id, "/does/not/exist/missing.pdf", "missing.pdf", 123)
            .unwrap();

        let r = apply_op(
            &c,
            &store,
            &OutboxOp::SendDraft {
                id: local_id.clone(),
            },
        );
        assert!(r.is_err());

        // the pending attachment is NOT cleared (so a retry can re-upload
        // once the file is back). The draft is reconciled to GRAPH-2 by
        // `ensure_draft_on_graph` before the upload loop runs, so that's
        // where the pending row now lives.
        let kept = store.outbound_attachments("GRAPH-2").unwrap();
        assert_eq!(kept.len(), 1);
        assert!(!kept[0].uploaded, "a file never uploaded is not marked");
        // And send was never reached.
        assert!(!srv.requests().iter().any(|r| r.path.ends_with("/send")));
    }

    #[test]
    fn send_draft_skips_an_attachment_already_uploaded() {
        // #1874: a failed send uploaded it; the resend must not attach it twice.
        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/GRAPH-3/send".into(),
                status: 202,
                headers: vec![],
                body: "".into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/GRAPH-3/attachments".into(),
                status: 201,
                headers: vec![],
                body: "{}".into(),
            },
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/messages/GRAPH-3".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store.create_local_draft("Hi", "bob@x", "", "body").unwrap();
        store.reconcile_id(&local_id, "GRAPH-3").unwrap();
        // The file is gone too: an uploaded row is never read again.
        store
            .add_outbound_attachment(&local_id, "/does/not/exist/a.pdf", "a.pdf", 1)
            .unwrap();
        store
            .mark_outbound_attachment_uploaded(&local_id, "/does/not/exist/a.pdf")
            .unwrap();

        apply_op(
            &c,
            &store,
            &OutboxOp::SendDraft {
                id: local_id.clone(),
            },
        )
        .unwrap();

        assert!(
            !srv.requests()
                .iter()
                .any(|r| r.path.contains("/attachments"))
        );
        assert!(
            srv.requests()
                .iter()
                .any(|r| r.path == "/me/messages/GRAPH-3/send")
        );
        assert!(store.outbound_attachments(&local_id).unwrap().is_empty());
    }

    /// The JSON bodies of the requests to exactly `path` (#1949 tests).
    fn bodies_to(srv: &FakeServer, method: &str, path: &str) -> Vec<Value> {
        srv.requests()
            .iter()
            .filter(|r| r.method == method && r.path == path)
            .map(|r| json::parse(&r.body).unwrap())
            .collect()
    }

    #[test]
    fn send_draft_uploads_an_inline_picture_from_bytes_with_its_content_id() {
        // #1949: an html body's `cid:pic1` and a pasted picture (bytes, no
        // file) attached inline as `pic1`.
        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/GRAPH-5/send".into(),
                status: 202,
                headers: vec![],
                body: "".into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/GRAPH-5/attachments".into(),
                status: 201,
                headers: vec![],
                body: "{}".into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages".into(),
                status: 201,
                headers: vec![],
                body: draft_json("GRAPH-5", "Hi"),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store
            .create_local_draft("Hi", "bob@x", "", r#"<p><img src="cid:pic1"></p>"#)
            .unwrap();
        store
            // A name whose extension says nothing: the type sent must be the row's.
            .add_outbound_attachment_bytes(&local_id, "pasted", "image/png", b"\x89PNG", "pic1")
            .unwrap();

        apply_op(
            &c,
            &store,
            &OutboxOp::SendDraft {
                id: local_id.clone(),
            },
        )
        .unwrap();

        let created = bodies_to(&srv, "POST", "/me/messages");
        assert_eq!(created.len(), 1);
        assert!(
            created[0].to_string().contains("cid:pic1"),
            "the body refers to it"
        );
        let atts = bodies_to(&srv, "POST", "/me/messages/GRAPH-5/attachments");
        assert_eq!(atts.len(), 1);
        let a = &atts[0];
        assert_eq!(a.get("isInline").and_then(Value::as_bool), Some(true));
        assert_eq!(a.get("contentId").and_then(Value::as_str), Some("pic1"));
        assert_eq!(a.get("name").and_then(Value::as_str), Some("pasted"));
        assert_eq!(
            a.get("contentType").and_then(Value::as_str),
            Some("image/png")
        );
        assert_eq!(
            a.get("contentBytes").and_then(Value::as_str),
            Some(crate::graph::client::base64_encode(b"\x89PNG").as_str())
        );
        let reqs = srv.requests();
        let att_at = reqs.iter().position(|r| r.path.ends_with("/attachments"));
        let send_at = reqs.iter().position(|r| r.path.ends_with("/send"));
        assert!(
            att_at.unwrap() < send_at.unwrap(),
            "uploaded before the send"
        );
        assert!(store.outbound_attachments("GRAPH-5").unwrap().is_empty());
    }

    #[test]
    fn upload_digest_tells_every_field_apart() {
        // #1941: the digest changes with anything the session's
        // AttachmentItem or bytes hold, and only then.
        let d = |n: &str, t: &str, c: Option<&str>, b: &[u8]| upload_digest(n, t, c, b);
        let base = d("a.bin", "application/pdf", None, b"abc");
        assert_eq!(base, d("a.bin", "application/pdf", None, b"abc"));
        assert!(base.starts_with("3:"), "{base}");
        for other in [
            d("b.bin", "application/pdf", None, b"abc"),
            d("a.bin", "image/png", None, b"abc"),
            d("a.bin", "application/pdf", Some("pic1"), b"abc"),
            d("a.bin", "application/pdf", Some(""), b"abc"),
            d("a.bin", "application/pdf", None, b"abd"),
            d("a.bin", "application/pdf", None, b"abcd"),
            // A field boundary moved: "a.bi" + "napplication/pdf".
            d("a.bi", "napplication/pdf", None, b"abc"),
        ] {
            assert_ne!(base, other);
        }
    }

    /// A temp dir for one outbox test's files.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mailcore-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn put_route(path: &str, status: u16) -> Route {
        Route {
            method: "PUT".into(),
            path_prefix: path.into(),
            status,
            headers: vec![],
            body: "".into(),
        }
    }

    /// A Graph server for a send of a local draft created as `GRAPH-8`
    /// whose createUploadSession answers name `upload_urls` in turn.
    fn session_send_server(upload_urls: &[String]) -> FakeServer {
        let mut routes: Vec<(Route, Option<usize>)> = upload_urls
            .iter()
            .map(|url| {
                (
                    Route {
                        method: "POST".into(),
                        path_prefix: "/me/messages/GRAPH-8/attachments/createUploadSession".into(),
                        status: 200,
                        headers: vec![],
                        body: format!(r#"{{"uploadUrl":"{url}"}}"#),
                    },
                    Some(1),
                )
            })
            .collect();
        for (path, status, body) in [
            ("/me/messages/GRAPH-8/send", 202, String::new()),
            ("/me/messages/GRAPH-8", 200, "{}".to_string()),
            ("/me/messages", 201, draft_json("GRAPH-8", "Hi")),
        ] {
            let method = if path == "/me/messages/GRAPH-8" {
                "PATCH"
            } else {
                "POST"
            };
            routes.push((
                Route {
                    method: method.into(),
                    path_prefix: path.into(),
                    status,
                    headers: vec![],
                    body,
                },
                None,
            ));
        }
        FakeServer::start_limited(routes)
    }

    /// The `(path, Content-Range)` of each PUT `srv` received.
    fn put_ranges(srv: &FakeServer) -> Vec<(String, String)> {
        srv.requests()
            .into_iter()
            .filter(|r| r.method == "PUT")
            .map(|r| {
                let range = r
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-range"))
                    .map(|(_, v)| v.clone())
                    .expect("a Content-Range");
                (r.path, range)
            })
            .collect()
    }

    /// Two full upload-session chunks and a short third one.
    const BIG: usize = 2 * 3_932_160 + 500;

    #[test]
    fn a_changed_file_is_not_resumed_into_the_old_session() {
        // #1941: the first attempt fails on chunk 2, the file is rewritten
        // (same size, other bytes), and the retry must not splice the new
        // file's rest onto the old file's first chunk.
        let dir = temp_dir("1941-changed");
        let file = dir.join("big.bin");
        std::fs::write(&file, vec![b'a'; BIG]).unwrap();
        let upload_srv = FakeServer::start_limited(vec![
            (put_route("/upload/s1", 202), Some(1)),
            (put_route("/upload/s1", 500), Some(1)),
            (put_route("/upload/s2", 202), None),
        ]);
        let s1 = format!("{}/upload/s1", upload_srv.base_url);
        let s2 = format!("{}/upload/s2", upload_srv.base_url);
        let srv = session_send_server(&[s1.clone(), s2.clone()]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local = store
            .create_local_draft("Hi", "bob@x", "", "<p>hi</p>")
            .unwrap();
        let path = file.to_str().unwrap();
        store
            .add_outbound_attachment(&local, path, "big.bin", BIG as i64)
            .unwrap();

        let mut seen = Vec::new();
        let err = apply_send(&c, &store, &local, None, &mut |a, sent, total| {
            seen.push((a.path.clone(), sent, total))
        })
        .unwrap_err();
        assert!(
            matches!(err, GraphError::Http { status: 500, .. }),
            "{err:?}"
        );
        let row = &store.outbound_attachments("GRAPH-8").unwrap()[0];
        assert_eq!(
            row.upload_url.as_deref(),
            Some(s1.as_str()),
            "kept to resume"
        );
        assert_eq!(
            row.upload_digest.as_deref(),
            Some(
                upload_digest(
                    "big.bin",
                    "application/octet-stream",
                    None,
                    &vec![b'a'; BIG]
                )
                .as_str()
            )
        );

        std::fs::write(&file, vec![b'b'; BIG]).unwrap();
        apply_send(&c, &store, &local, None, &mut |a, sent, total| {
            seen.push((a.path.clone(), sent, total))
        })
        .unwrap();

        assert!(
            !upload_srv.requests().iter().any(|r| r.method == "GET"),
            "the old session is not even asked"
        );
        assert_eq!(
            bodies_to(
                &srv,
                "POST",
                "/me/messages/GRAPH-8/attachments/createUploadSession"
            )
            .len(),
            2,
            "a new session"
        );
        let ranges = put_ranges(&upload_srv);
        assert_eq!(ranges.len(), 5, "{ranges:?}");
        assert!(ranges[2..].iter().all(|(p, _)| p == "/upload/s2"));
        assert_eq!(ranges[2].1, format!("bytes 0-3932159/{BIG}"), "from byte 0");
        let t = BIG as u64;
        let sents: Vec<u64> = seen.iter().map(|(_, s, _)| *s).collect();
        assert_eq!(sents, vec![3_932_160, 3_932_160, 7_864_320, t]);
        assert!(seen.iter().all(|(p, _, total)| p == path && *total == t));
        assert!(store.outbound_attachments("GRAPH-8").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_draft_after_a_refused_resume_uploads_from_a_new_session() {
        // #1941: a live session (its GET answers) whose PUT Graph refuses is
        // dropped in the same attempt, which sends from a new session.
        let dir = temp_dir("1941-refused");
        let file = dir.join("big.bin");
        std::fs::write(&file, vec![b'a'; BIG]).unwrap();
        let upload_srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/upload/old".into(),
                status: 200,
                headers: vec![],
                body: r#"{"nextExpectedRanges":["3932160-"]}"#.into(),
            },
            put_route("/upload/old", 416),
            put_route("/upload/new", 202),
        ]);
        let old = format!("{}/upload/old", upload_srv.base_url);
        let new = format!("{}/upload/new", upload_srv.base_url);
        let srv = session_send_server(std::slice::from_ref(&new));
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local = store
            .create_local_draft("Hi", "bob@x", "", "<p>hi</p>")
            .unwrap();
        let path = file.to_str().unwrap();
        store
            .add_outbound_attachment(&local, path, "big.bin", BIG as i64)
            .unwrap();
        let digest = upload_digest(
            "big.bin",
            "application/octet-stream",
            None,
            &vec![b'a'; BIG],
        );
        store
            .set_outbound_attachment_upload_session(&local, path, Some((&old, &digest)))
            .unwrap();

        apply_send(&c, &store, &local, None, &mut |_, _, _| {}).unwrap();

        let ranges = put_ranges(&upload_srv);
        assert_eq!(ranges.len(), 4, "{ranges:?}");
        assert_eq!(ranges[0].0, "/upload/old");
        assert!(ranges[1..].iter().all(|(p, _)| p == "/upload/new"));
        assert_eq!(ranges[1].1, format!("bytes 0-3932159/{BIG}"));
        assert!(
            srv.requests()
                .iter()
                .any(|r| r.path == "/me/messages/GRAPH-8/send")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_draft_uploads_a_large_inline_file_through_an_upload_session_with_its_content_id() {
        // #1949: an inline file over the small-upload limit goes through an
        // upload session whose AttachmentItem carries both fields. The
        // upload target is its own server, as its URL goes in the session
        // answer (see `add_attachment_routes_small_inline_and_large_to_upload_session`).
        let dir = std::env::temp_dir().join(format!(
            "mailcore-1949-large-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("big.png");
        let big = vec![7u8; 3 * 1024 * 1024 + 100];
        std::fs::write(&file, &big).unwrap();

        let upload_srv = FakeServer::start(vec![Route {
            method: "PUT".into(),
            path_prefix: "/upload/xyz".into(),
            status: 201,
            headers: vec![],
            body: "".into(),
        }]);
        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/GRAPH-8/send".into(),
                status: 202,
                headers: vec![],
                body: "".into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/GRAPH-8/attachments/createUploadSession".into(),
                status: 200,
                headers: vec![],
                body: format!(r#"{{"uploadUrl":"{}/upload/xyz"}}"#, upload_srv.base_url),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages".into(),
                status: 201,
                headers: vec![],
                body: draft_json("GRAPH-8", "Hi"),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store
            .create_local_draft("Hi", "bob@x", "", r#"<p><img src="cid:pic1"></p>"#)
            .unwrap();
        store
            .add_inline_outbound_attachment_file(
                &local_id,
                file.to_str().unwrap(),
                "big.png",
                big.len() as i64,
                None,
                "pic1",
            )
            .unwrap();

        apply_op(
            &c,
            &store,
            &OutboxOp::SendDraft {
                id: local_id.clone(),
            },
        )
        .unwrap();

        let sessions = bodies_to(
            &srv,
            "POST",
            "/me/messages/GRAPH-8/attachments/createUploadSession",
        );
        assert_eq!(sessions.len(), 1);
        let item = sessions[0]
            .get("AttachmentItem")
            .expect("an AttachmentItem");
        assert_eq!(item.get("isInline").and_then(Value::as_bool), Some(true));
        assert_eq!(item.get("contentId").and_then(Value::as_str), Some("pic1"));
        assert_eq!(item.get("name").and_then(Value::as_str), Some("big.png"));
        assert_eq!(
            item.get("contentType").and_then(Value::as_str),
            Some("image/png"),
            "derived from the name"
        );
        // The PUTs cover the whole file (FakeServer bodies are lossy UTF-8,
        // so the ranges are checked, not the bytes).
        let mut covered = 0usize;
        for r in upload_srv.requests() {
            assert_eq!(r.method, "PUT");
            let range = r
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-range"))
                .map(|(_, v)| v.clone())
                .expect("a Content-Range");
            let (span, total) = range
                .strip_prefix("bytes ")
                .and_then(|r| r.split_once('/'))
                .unwrap();
            assert_eq!(total.parse::<usize>().unwrap(), big.len());
            let (start, end) = span.split_once('-').unwrap();
            assert_eq!(start.parse::<usize>().unwrap(), covered);
            covered = end.parse::<usize>().unwrap() + 1;
        }
        assert_eq!(covered, big.len(), "the PUTs cover the file");
        assert!(
            srv.requests()
                .iter()
                .any(|r| r.path == "/me/messages/GRAPH-8/send")
        );
        assert!(store.outbound_attachments("GRAPH-8").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_send_keeps_an_inline_bytes_row_and_the_resend_skips_it() {
        // #1949 with #1874: `/send` refused once (403), then accepted. The
        // inline picture is uploaded by the first attempt only, and kept
        // (content id, bytes, marked uploaded) in between.
        let srv = FakeServer::start_limited(vec![
            (
                Route {
                    method: "POST".into(),
                    path_prefix: "/me/messages/GRAPH-9/send".into(),
                    status: 403,
                    headers: vec![],
                    body: r#"{"error":{"code":"ErrorAccessDenied","message":"no"}}"#.into(),
                },
                Some(1),
            ),
            (
                Route {
                    method: "POST".into(),
                    path_prefix: "/me/messages/GRAPH-9/send".into(),
                    status: 202,
                    headers: vec![],
                    body: "".into(),
                },
                None,
            ),
            (
                Route {
                    method: "POST".into(),
                    path_prefix: "/me/messages/GRAPH-9/attachments".into(),
                    status: 201,
                    headers: vec![],
                    body: "{}".into(),
                },
                None,
            ),
            (
                Route {
                    method: "PATCH".into(),
                    path_prefix: "/me/messages/GRAPH-9".into(),
                    status: 200,
                    headers: vec![],
                    body: "{}".into(),
                },
                None,
            ),
            (
                Route {
                    method: "POST".into(),
                    path_prefix: "/me/messages".into(),
                    status: 201,
                    headers: vec![],
                    body: draft_json("GRAPH-9", "Hi"),
                },
                None,
            ),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store
            .create_local_draft("Hi", "bob@x", "", r#"<p><img src="cid:pic1"></p>"#)
            .unwrap();
        store
            .add_outbound_attachment_bytes(&local_id, "pic.png", "image/png", b"PIC", "pic1")
            .unwrap();
        let send = OutboxOp::SendDraft {
            id: local_id.clone(),
        };

        assert!(
            apply_op(&c, &store, &send).is_err(),
            "the 403 fails the send"
        );
        let kept = store.outbound_attachments(&local_id).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].content_id.as_deref(), Some("pic1"));
        assert!(kept[0].from_bytes);
        assert!(kept[0].uploaded, "Graph confirmed the upload");
        assert_eq!(
            store
                .outbound_attachment_bytes("GRAPH-9", "cid:pic1")
                .unwrap(),
            Some(b"PIC".to_vec())
        );

        apply_op(&c, &store, &send).unwrap();
        let uploads = bodies_to(&srv, "POST", "/me/messages/GRAPH-9/attachments");
        assert_eq!(uploads.len(), 1, "never uploaded twice");
        assert_eq!(
            uploads[0].get("contentId").and_then(Value::as_str),
            Some("pic1")
        );
        let sends = srv
            .requests()
            .iter()
            .filter(|r| r.path == "/me/messages/GRAPH-9/send")
            .count();
        assert_eq!(sends, 2);
        assert!(store.outbound_attachments("GRAPH-9").unwrap().is_empty());
    }

    #[test]
    fn a_posted_send_of_a_draft_still_local_creates_and_sends_without_a_check() {
        // FIX r3: a send an older engine queued reads as posted, but a
        // `local:` draft was never on Graph, so it cannot have been sent.
        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/GRAPH-5/send".into(),
                status: 202,
                headers: vec![],
                body: "".into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages".into(),
                status: 201,
                headers: vec![],
                body: draft_json("GRAPH-5", "Hi"),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store.create_local_draft("Hi", "bob@x", "", "body").unwrap();
        let seq = store
            .enqueue_op(&OutboxOp::SendDraft {
                id: local_id.clone(),
            })
            .unwrap();
        store.set_send_posted(seq, true).unwrap();

        apply_send(&c, &store, &local_id, Some(seq), &mut |_, _, _| {}).unwrap();

        let reqs = srv.requests();
        assert!(reqs.iter().all(|r| r.method != "GET"), "no delivery check");
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].path, "/me/messages");
        assert_eq!(reqs[1].path, "/me/messages/GRAPH-5/send");
        assert!(store.send_posted(seq).unwrap(), "set again for this POST");
    }

    #[test]
    fn send_error_text_prefers_graphs_own_message() {
        let http = |status: u16, body: &str| GraphError::Http {
            status,
            body: body.into(),
        };
        assert_eq!(
            send_error_text(&http(
                403,
                r#"{"error":{"code":"ErrorAccessDenied","message":"Access is denied."}}"#
            )),
            "Access is denied."
        );
        // Graph's message is shown as one line, capped like `Display` (#1954).
        assert_eq!(
            send_error_text(&http(
                400,
                r#"{"error":{"code":"X","message":"Bad id.\r\n\tTry again."}}"#
            )),
            "Bad id. Try again."
        );
        let long = send_error_text(&http(
            400,
            &format!(
                r#"{{"error":{{"code":"X","message":"{}"}}}}"#,
                "é".repeat(HTTP_ERROR_TEXT_MAX * 2)
            ),
        ));
        assert_eq!(long.chars().count(), HTTP_ERROR_TEXT_MAX);
        assert!(long.ends_with('…'), "{long}");
        assert_eq!(send_error_text(&http(403, "<html>nope</html>")), "HTTP 403");
        assert_eq!(
            send_error_text(&http(400, r#"{"error":{"code":"X","message":"  "}}"#)),
            "HTTP 400"
        );
        assert_eq!(
            send_error_text(&http(400, r#"{"error":{"code":"X"}}"#)),
            "HTTP 400"
        );
        assert_eq!(send_error_text(&GraphError::NotFound), "not found (404)");
        assert_eq!(
            send_error_text(&GraphError::Parse("cannot read attachment /x: gone".into())),
            "cannot read attachment /x: gone"
        );
    }

    /// A Graph event JSON body shaped like `create_event`'s response —
    /// mirrors the fixture `graph::client`'s own tests use.
    fn event_json(id: &str, subject: &str) -> String {
        format!(
            r#"{{"id":"{id}","subject":"{subject}",
            "start":{{"dateTime":"2026-07-20T11:00:00.0000000","timeZone":"UTC"}},
            "end":{{"dateTime":"2026-07-20T12:00:00.0000000","timeZone":"UTC"}},
            "isAllDay":false,"location":{{"displayName":"Room 1"}},
            "organizer":{{"emailAddress":{{"name":"Me","address":"me@x"}}}},
            "responseStatus":{{"response":"organizer"}},
            "attendees":[{{"emailAddress":{{"name":"Bob","address":"bob@x.com"}},"type":"required","status":{{"response":"none"}}}}],
            "bodyPreview":"agenda","webLink":"","lastModifiedDateTime":""}}"#
        )
    }

    fn sample_event_fields() -> crate::store::LocalEventFields {
        crate::store::LocalEventFields {
            subject: "Sync".into(),
            start_utc: "2026-07-20T11:00:00Z".into(),
            end_utc: "2026-07-20T12:00:00Z".into(),
            is_all_day: false,
            location: "Room 1".into(),
            body_html: "<p>agenda</p>".into(),
            attendees: vec![crate::graph::client::AttendeeInput {
                name: "Bob".into(),
                addr: "bob@x.com".into(),
                r#type: String::new(),
            }],
            recurrence: None,
            ..Default::default()
        }
    }

    /// A store whose calendar list holds a default `CAL1` and editable
    /// `CAL2` and `A/b+=` (#2048).
    fn store_with_calendars() -> Store {
        let cal = |id: &str, default: bool| crate::graph::model::Calendar {
            id: id.into(),
            is_default: default,
            can_edit: true,
            owner_addr: "me@x".into(),
            ..Default::default()
        };
        let store = Store::open_in_memory().unwrap();
        store
            .replace_calendars(&[cal("CAL1", true), cal("CAL2", false), cal("A/b+=", false)])
            .unwrap();
        store
    }

    /// Answers a create in any calendar, or `/me/events`, with `EV1`, and
    /// a PATCH of it.
    fn create_routes() -> Vec<Route> {
        let post = |prefix: &str| Route {
            method: "POST".into(),
            path_prefix: prefix.into(),
            status: 201,
            headers: vec![],
            body: event_json("EV1", "Sync"),
        };
        vec![
            post("/me/calendars/"),
            post("/me/events"),
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/events/EV1".into(),
                status: 200,
                headers: vec![],
                body: event_json("EV1", "Sync v2"),
            },
        ]
    }

    fn create_in(store: &Store, calendar: Option<&str>) -> String {
        store
            .create_local_event(
                &crate::store::LocalEventFields {
                    calendar_id: calendar.map(String::from),
                    ..sample_event_fields()
                },
                "Me",
                "me@x",
            )
            .unwrap()
    }

    #[test]
    fn create_event_in_a_chosen_calendar_posts_to_its_events() {
        let srv = FakeServer::start(create_routes());
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = store_with_calendars();
        let local_id = create_in(&store, Some("CAL2"));
        apply_op(&c, &store, &OutboxOp::CreateEvent { id: local_id }).unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/calendars/CAL2/events");
        let rows = store
            .events_in_range(
                "2026-07-20T00:00:00Z",
                "2026-07-21T00:00:00Z",
                &["CAL2".to_string()],
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "EV1");
        assert_eq!(rows[0].calendar_id, "CAL2");
        // An edit still addresses `/me/events/{id}`, in any calendar.
        let changed = store
            .update_event_fields(
                "EV1",
                &crate::store::LocalEventFields {
                    subject: "Sync v2".into(),
                    ..sample_event_fields()
                },
            )
            .unwrap();
        assert_eq!(changed, EventFieldSet::SUBJECT);
        apply_op(
            &c,
            &store,
            &OutboxOp::UpdateEvent {
                id: "EV1".into(),
                fields: changed,
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[1].method, "PATCH");
        assert_eq!(reqs[1].path, "/me/events/EV1");
    }

    #[test]
    fn create_event_in_the_default_calendar_posts_to_me_events() {
        let with_list = |chosen: Option<&str>| (store_with_calendars(), chosen.map(String::from));
        let cases = [
            with_list(None),
            with_list(Some("")),
            with_list(Some("CAL1")),
            // No calendar list yet: the row's calendar is `''`.
            (Store::open_in_memory().unwrap(), None),
        ];
        for (store, chosen) in cases {
            let srv = FakeServer::start(create_routes());
            let c = GraphClient::new(&srv.base_url, "AT");
            let local_id = create_in(&store, chosen.as_deref());
            apply_op(&c, &store, &OutboxOp::CreateEvent { id: local_id }).unwrap();
            let reqs = srv.requests();
            assert_eq!(reqs.len(), 1, "{chosen:?}");
            assert_eq!(reqs[0].path, "/me/events", "{chosen:?}");
            assert!(store.event_for_send("EV1").unwrap().is_some(), "{chosen:?}");
        }
    }

    #[test]
    fn create_event_encodes_the_calendar_id() {
        let srv = FakeServer::start(create_routes());
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = store_with_calendars();
        let local_id = create_in(&store, Some("A/b+="));
        apply_op(&c, &store, &OutboxOp::CreateEvent { id: local_id }).unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].path, "/me/calendars/A%2Fb%2B%3D/events");
        let rows = store
            .events_in_range(
                "2026-07-20T00:00:00Z",
                "2026-07-21T00:00:00Z",
                &["A/b+=".to_string()],
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "EV1");
    }

    #[test]
    fn apply_op_create_event_posts_and_reconciles() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events".into(),
            status: 201,
            headers: vec![],
            body: event_json("EV1", "Sync"),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store
            .create_local_event(&sample_event_fields(), "Me", "me@x")
            .unwrap();

        apply_op(
            &c,
            &store,
            &OutboxOp::CreateEvent {
                id: local_id.clone(),
            },
        )
        .unwrap();

        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/events");
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(sent.get("subject").and_then(Value::as_str), Some("Sync"));

        // reconciled: the event now lives under "EV1"
        assert!(store.event_for_send("EV1").unwrap().is_some());
        assert!(store.event_for_send(&local_id).unwrap().is_none());
    }

    /// Creates a local event from `fields` in a fresh store, applies its
    /// `CreateEvent` against a server answering `EV1`, and returns the POSTed
    /// body and the local row as it was before the create.
    fn create_event_body(
        fields: &crate::store::LocalEventFields,
    ) -> (Value, crate::store::EventRow) {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events".into(),
            status: 201,
            headers: vec![],
            body: event_json("EV1", "Sync"),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store.create_local_event(fields, "Me", "me@x").unwrap();
        let row = store
            .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap()
            .into_iter()
            .find(|r| r.id == local_id)
            .unwrap();
        apply_op(&c, &store, &OutboxOp::CreateEvent { id: local_id }).unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            (reqs[0].method.as_str(), reqs[0].path.as_str()),
            ("POST", "/me/events")
        );
        (json::parse(&reqs[0].body).unwrap(), row)
    }

    /// A lossless recurrence of pattern `kind` and range `range` (#2034),
    /// with the fields a recurrence dialog fills for that kind.
    fn lossless(kind: &str, range: &str) -> crate::graph::model::EventRecurrence {
        use crate::graph::model::{EventRecurrence, RecurrencePattern, RecurrenceRange};
        let mut pattern = RecurrencePattern {
            r#type: kind.into(),
            interval: 2,
            ..Default::default()
        };
        match kind {
            "daily" => {}
            "weekly" => {
                pattern.days_of_week = vec!["monday".into(), "wednesday".into()];
                pattern.first_day_of_week = "monday".into();
            }
            "absoluteMonthly" => pattern.day_of_month = 15,
            "relativeMonthly" => {
                pattern.days_of_week = vec!["tuesday".into()];
                pattern.index = "last".into();
            }
            "absoluteYearly" => {
                pattern.month = 3;
                pattern.day_of_month = 31;
            }
            "relativeYearly" => {
                pattern.month = 11;
                pattern.days_of_week = vec!["thursday".into()];
                pattern.index = "fourth".into();
            }
            other => panic!("unknown kind {other}"),
        }
        let mut range = RecurrenceRange {
            r#type: range.into(),
            start_date: "2026-07-20".into(),
            recurrence_time_zone: "W. Europe Standard Time".into(),
            ..Default::default()
        };
        match range.r#type.as_str() {
            "noEnd" => {}
            "endDate" => range.end_date = "2027-07-20".into(),
            "numbered" => range.number_of_occurrences = 7,
            other => panic!("unknown range {other}"),
        }
        EventRecurrence { pattern, range }
    }

    #[test]
    fn create_event_sends_every_lossless_recurrence_unchanged() {
        for kind in [
            "daily",
            "weekly",
            "absoluteMonthly",
            "relativeMonthly",
            "absoluteYearly",
            "relativeYearly",
        ] {
            for range in ["noEnd", "endDate", "numbered"] {
                let rec = lossless(kind, range);
                let (body, row) = create_event_body(&crate::store::LocalEventFields {
                    series_recurrence: Some(rec.clone()),
                    ..sample_event_fields()
                });
                let what = format!("{kind} {range}");
                let sent = body.get("recurrence").expect(&what);
                assert_eq!(sent, &rec.to_send_json(), "{what}");
                assert_eq!(
                    crate::graph::model::EventRecurrence::from_json(sent)
                        .map(|r| (r.pattern, r.range.r#type)),
                    Some((rec.pattern.clone(), rec.range.r#type.clone())),
                    "{what}"
                );
                let sent_range = sent.get("range").unwrap();
                assert_eq!(
                    sent_range.get("numberOfOccurrences").is_some(),
                    range == "numbered",
                    "{what}"
                );
                assert_eq!(
                    sent_range.get("endDate").is_some(),
                    range == "endDate",
                    "{what}"
                );
                assert_eq!(row.recurrence, Some(rec), "{what}");
                assert_eq!(row.event_type, "seriesMaster", "{what}");
            }
        }
    }

    #[test]
    fn create_event_sends_a_lossless_recurrence_as_given() {
        let (body, _) = create_event_body(&crate::store::LocalEventFields {
            series_recurrence: Some(crate::graph::model::EventRecurrence {
                pattern: crate::graph::model::RecurrencePattern {
                    r#type: "relativeYearly".into(),
                    interval: 1,
                    month: 11,
                    day_of_month: 0,
                    days_of_week: vec!["friday".into()],
                    first_day_of_week: "monday".into(),
                    index: "last".into(),
                },
                range: crate::graph::model::RecurrenceRange {
                    r#type: "numbered".into(),
                    start_date: "2026-11-27".into(),
                    end_date: String::new(),
                    number_of_occurrences: 5,
                    recurrence_time_zone: "Pacific Standard Time".into(),
                },
            }),
            ..sample_event_fields()
        });
        let expected = json::parse(
            r#"{"pattern":{"type":"relativeYearly","interval":1,"month":11,
                "daysOfWeek":["friday"],"firstDayOfWeek":"monday","index":"last"},
                "range":{"type":"numbered","startDate":"2026-11-27",
                "numberOfOccurrences":5,"recurrenceTimeZone":"Pacific Standard Time"}}"#,
        )
        .unwrap();
        assert_eq!(body.get("recurrence"), Some(&expected));
    }

    #[test]
    fn create_event_with_create_side_recurrence_sends_as_before() {
        use crate::graph::model::{Recurrence, RecurrenceKind};
        let rec = Recurrence {
            kind: RecurrenceKind::Weekly,
            interval: 1,
            days_of_week: vec!["monday".into()],
            day_of_month: 0,
            start_date: "2026-07-20".into(),
            until: Some("2026-08-31".into()),
        };
        let (body, row) = create_event_body(&crate::store::LocalEventFields {
            recurrence: Some(rec.clone()),
            ..sample_event_fields()
        });
        assert_eq!(body.get("recurrence"), Some(&rec.to_json()));
        assert_eq!(row.event_type, "seriesMaster");
    }

    #[test]
    fn create_event_without_recurrence_sends_none() {
        let (body, row) = create_event_body(&sample_event_fields());
        assert!(body.get("recurrence").is_none());
        assert_eq!(row.recurrence, None);
        assert_eq!(row.event_type, "singleInstance");
    }

    /// The keys of a sent JSON object, in order.
    fn sent_keys(body: &str) -> Vec<String> {
        match json::parse(body).unwrap() {
            Value::Object(o) => o.into_iter().map(|(k, _)| k).collect(),
            other => panic!("not an object: {other:?}"),
        }
    }

    /// A synced event `EV1` with Graph's attendees and Outlook's fields.
    fn store_with_synced_event() -> Store {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_event(&crate::store::NewEvent {
                id: "EV1".into(),
                subject: "Sync".into(),
                start_utc: "2026-07-20T11:00:00Z".into(),
                end_utc: "2026-07-20T12:00:00Z".into(),
                location: "Room 1".into(),
                body_html: "<p>agenda</p>".into(),
                show_as: "tentative".into(),
                ..Default::default()
            })
            .unwrap();
        store
    }

    #[test]
    fn update_event_sends_only_changed_fields() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/events/EV1".into(),
            status: 200,
            headers: vec![],
            body: event_json("EV1", "Sync v2"),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = store_with_synced_event();
        let changed = store
            .update_event_fields(
                "EV1",
                &crate::store::LocalEventFields {
                    subject: "Sync v2".into(),
                    start_utc: "2026-07-20T11:00:00Z".into(),
                    end_utc: "2026-07-20T12:00:00Z".into(),
                    location: "Room 1".into(),
                    body_html: "<p>agenda</p>".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(changed, EventFieldSet::SUBJECT);
        apply_op(
            &c,
            &store,
            &OutboxOp::UpdateEvent {
                id: "EV1".into(),
                fields: changed,
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(sent_keys(&reqs[0].body), ["subject"]);
        assert_eq!(
            json::parse(&reqs[0].body)
                .unwrap()
                .get("subject")
                .and_then(Value::as_str),
            Some("Sync v2")
        );
    }

    /// Clearing a filled occurrence's body (#2064 r4): the host showed the
    /// master's body (the occurrence has none of its own), the edit clears
    /// it, and the PATCH sends it cleared, not the master's again.
    #[test]
    fn update_event_clearing_an_occurrences_shown_body_sends_it_empty() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/events/O1".into(),
            status: 200,
            headers: vec![],
            body: event_json("O1", "Standup"),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let common = crate::store::NewEvent {
            subject: "Standup".into(),
            start_utc: "2026-07-20T11:00:00Z".into(),
            end_utc: "2026-07-20T11:15:00Z".into(),
            ..Default::default()
        };
        store
            .upsert_event(&crate::store::NewEvent {
                id: "M".into(),
                event_type: "seriesMaster".into(),
                body_html: "<p>series</p>".into(),
                ..common.clone()
            })
            .unwrap();
        store
            .upsert_event(&crate::store::NewEvent {
                id: "O1".into(),
                event_type: "occurrence".into(),
                series_master_id: Some("M".into()),
                ..common
            })
            .unwrap();
        assert_eq!(
            store.event_body("O1").unwrap().as_deref(),
            Some("<p>series</p>")
        );
        let changed = store
            .update_event_fields(
                "O1",
                &crate::store::LocalEventFields {
                    subject: "Standup".into(),
                    start_utc: "2026-07-20T11:00:00Z".into(),
                    end_utc: "2026-07-20T11:15:00Z".into(),
                    body_html: String::new(),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(changed, EventFieldSet::BODY);
        apply_op(
            &c,
            &store,
            &OutboxOp::UpdateEvent {
                id: "O1".into(),
                fields: changed,
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(sent_keys(&reqs[0].body), ["body"]);
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(
            sent.get("body")
                .and_then(|b| b.get("content"))
                .and_then(Value::as_str),
            Some("")
        );
    }

    #[test]
    fn update_event_with_empty_set_sends_nothing() {
        // No route: any request would fail the op.
        let srv = FakeServer::start(vec![]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = store_with_synced_event();
        apply_op(
            &c,
            &store,
            &OutboxOp::UpdateEvent {
                id: "EV1".into(),
                fields: EventFieldSet::default(),
            },
        )
        .unwrap();
        assert!(srv.requests().is_empty());
    }

    /// A queued pre-#1994 `UpdateEvent` (no `fields`) still sends the old
    /// full set.
    #[test]
    fn legacy_update_event_sends_the_old_full_set() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/events/EV1".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = store_with_synced_event();
        let op = OutboxOp::from_json(&json::parse(r#"{"kind":"updateEvent","id":"EV1"}"#).unwrap())
            .unwrap();
        apply_op(&c, &store, &op).unwrap();
        assert_eq!(
            sent_keys(&srv.requests()[0].body),
            [
                "subject",
                "start",
                "end",
                "isAllDay",
                "location",
                "attendees",
                "body"
            ]
        );
    }

    #[test]
    fn update_event_online_meeting_writes_back_join_url() {
        let teams = "https://teams.microsoft.com/l/meetup-join/T";
        let answer = format!(
            r#"{{"id":"EV1","subject":"Sync","isOnlineMeeting":true,
                "onlineMeetingProvider":"teamsForBusiness",
                "onlineMeeting":{{"joinUrl":"{teams}"}}}}"#
        );
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/events/EV1".into(),
            status: 200,
            headers: vec![],
            body: answer,
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = store_with_synced_event();
        let changed = store
            .update_event_fields(
                "EV1",
                &crate::store::LocalEventFields {
                    subject: "Sync".into(),
                    start_utc: "2026-07-20T11:00:00Z".into(),
                    end_utc: "2026-07-20T12:00:00Z".into(),
                    location: "Room 1".into(),
                    body_html: "<p>agenda</p>".into(),
                    is_online_meeting: Some(true),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(changed, EventFieldSet::ONLINE_MEETING);
        apply_op(
            &c,
            &store,
            &OutboxOp::UpdateEvent {
                id: "EV1".into(),
                fields: changed,
            },
        )
        .unwrap();
        // No provider set: Graph picks the calendar's default.
        assert_eq!(sent_keys(&srv.requests()[0].body), ["isOnlineMeeting"]);
        let row = store
            .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap()
            .into_iter()
            .find(|r| r.id == "EV1")
            .unwrap();
        assert_eq!(row.join_url, teams);
        assert_eq!(row.online_meeting_provider, "teamsForBusiness");
    }

    #[test]
    fn create_event_without_reminder_omits_it_and_stores_graphs() {
        let answer = event_json("EV1", "Sync").replacen(
            r#""isAllDay":false"#,
            r#""isAllDay":false,"isReminderOn":true,"reminderMinutesBeforeStart":15"#,
            1,
        );
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events".into(),
            status: 201,
            headers: vec![],
            body: answer,
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store
            .create_local_event(&sample_event_fields(), "Me", "me@x")
            .unwrap();
        apply_op(&c, &store, &OutboxOp::CreateEvent { id: local_id }).unwrap();
        let k = sent_keys(&srv.requests()[0].body);
        assert!(!k.iter().any(|x| x == "isReminderOn"), "{k:?}");
        assert!(!k.iter().any(|x| x == "reminderMinutesBeforeStart"));
        // Every other group is sent: Graph's create defaults as stored.
        for want in [
            "subject",
            "start",
            "attendees",
            "body",
            "showAs",
            "sensitivity",
            "importance",
            "categories",
            "isOnlineMeeting",
            "responseRequested",
            "allowNewTimeProposals",
        ] {
            assert!(k.iter().any(|x| x == want), "{want} not sent: {k:?}");
        }
        // The mailbox default Graph applied is stored right away.
        let sent = store.event_for_send("EV1").unwrap().unwrap();
        assert!(sent.is_reminder_on);
        assert_eq!(sent.reminder_minutes, 15);
        assert!(!sent.reminder_unset);
    }

    #[test]
    fn create_event_with_reminder_sends_it() {
        let teams = "https://teams.microsoft.com/l/meetup-join/C";
        let answer = event_json("EV1", "Sync").replacen(
            r#""isAllDay":false"#,
            &format!(
                r#""isAllDay":false,"isReminderOn":false,"reminderMinutesBeforeStart":0,"isOnlineMeeting":true,"onlineMeeting":{{"joinUrl":"{teams}"}}"#
            ),
            1,
        );
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events".into(),
            status: 201,
            headers: vec![],
            body: answer,
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store
            .create_local_event(
                &crate::store::LocalEventFields {
                    reminder: Some(crate::store::EventReminder {
                        on: false,
                        minutes_before_start: 0,
                    }),
                    is_online_meeting: Some(true),
                    ..sample_event_fields()
                },
                "Me",
                "me@x",
            )
            .unwrap();
        apply_op(&c, &store, &OutboxOp::CreateEvent { id: local_id }).unwrap();
        let sent = json::parse(&srv.requests()[0].body).unwrap();
        assert_eq!(
            sent.get("isReminderOn").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            sent.get("reminderMinutesBeforeStart")
                .and_then(Value::as_i64),
            Some(0)
        );
        assert_eq!(
            sent.get("isOnlineMeeting").and_then(Value::as_bool),
            Some(true)
        );
        assert!(sent.get("onlineMeetingProvider").is_none());
        // The online meeting's join link Graph returned is stored.
        let row = store
            .events_in_window("2026-07-20T00:00:00Z", "2026-07-21T00:00:00Z")
            .unwrap()
            .into_iter()
            .find(|r| r.id == "EV1")
            .unwrap();
        assert_eq!(row.join_url, teams);
        assert!(!row.is_reminder_on);
    }

    #[test]
    fn apply_op_delete_event_of_a_local_id_makes_no_graph_call() {
        // A local:-only event id that never synced: DeleteEvent must NOT
        // hit Graph. No route is stubbed at all — any request would panic
        // in the fake server's route matching.
        let srv = FakeServer::start(vec![]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::DeleteEvent {
                id: "local:never".into(),
            },
        )
        .unwrap();
        assert!(srv.requests().is_empty());
    }

    /// `sample_event_fields` named in two zones (#2039): 09:00 in Berlin's
    /// summer (UTC+2) is 07:00 UTC; the end, 08:30 in London (UTC+1).
    fn zoned_event_fields() -> crate::store::LocalEventFields {
        crate::store::LocalEventFields {
            start_utc: "2026-07-20T07:00:00Z".into(),
            end_utc: "2026-07-20T07:30:00Z".into(),
            start_time_zone: "W. Europe Standard Time".into(),
            start_local: "2026-07-20T09:00:00".into(),
            end_time_zone: "Europe/London".into(),
            end_local: "2026-07-20T08:30:00".into(),
            ..sample_event_fields()
        }
    }

    #[test]
    fn queued_create_event_sends_named_zones() {
        let (body, _) = create_event_body(&crate::store::LocalEventFields {
            series_recurrence: Some(lossless("weekly", "numbered")),
            ..zoned_event_fields()
        });
        assert_eq!(
            body.get("start").unwrap().to_string(),
            r#"{"dateTime":"2026-07-20T09:00:00","timeZone":"W. Europe Standard Time"}"#
        );
        assert_eq!(
            body.get("end").unwrap().to_string(),
            r#"{"dateTime":"2026-07-20T08:30:00","timeZone":"Europe/London"}"#
        );
        assert_eq!(
            body.get("recurrence")
                .and_then(|r| r.get("range"))
                .and_then(|r| r.get("recurrenceTimeZone"))
                .and_then(Value::as_str),
            Some("W. Europe Standard Time")
        );
        // The create-side recurrence names the start's zone too.
        use crate::graph::model::{Recurrence, RecurrenceKind};
        let (body, _) = create_event_body(&crate::store::LocalEventFields {
            recurrence: Some(Recurrence {
                kind: RecurrenceKind::Weekly,
                interval: 1,
                days_of_week: vec!["monday".into()],
                day_of_month: 0,
                start_date: "2026-07-20".into(),
                until: None,
            }),
            ..zoned_event_fields()
        });
        assert_eq!(
            body.get("recurrence")
                .and_then(|r| r.get("range"))
                .unwrap()
                .to_string(),
            r#"{"type":"noEnd","startDate":"2026-07-20","recurrenceTimeZone":"W. Europe Standard Time"}"#
        );
    }

    #[test]
    fn queued_update_without_times_sends_no_start_or_end() {
        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/events".into(),
                status: 201,
                headers: vec![],
                body: event_json("EV1", "Sync"),
            },
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/events/EV1".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store
            .create_local_event(&zoned_event_fields(), "Me", "me@x")
            .unwrap();
        apply_op(&c, &store, &OutboxOp::CreateEvent { id: local_id }).unwrap();
        // Edit `EV1` as a host does, then send its queued `UpdateEvent`;
        // returns the PATCH body.
        let n = std::cell::Cell::new(1);
        let edit = |f: crate::store::LocalEventFields| {
            let fields = store.update_event_fields("EV1", &f).unwrap();
            apply_op(
                &c,
                &store,
                &OutboxOp::UpdateEvent {
                    id: "EV1".into(),
                    fields,
                },
            )
            .unwrap();
            let reqs = srv.requests();
            n.set(n.get() + 1);
            assert_eq!(reqs.len(), n.get());
            assert_eq!(reqs[n.get() - 1].method, "PATCH");
            reqs[n.get() - 1].body.clone()
        };
        let unnamed = |f: crate::store::LocalEventFields| crate::store::LocalEventFields {
            start_time_zone: String::new(),
            end_time_zone: String::new(),
            ..f
        };
        // A title edit, with no zone or with the form's: no `start`/`end`.
        let body = edit(unnamed(crate::store::LocalEventFields {
            subject: "Sync v2".into(),
            ..zoned_event_fields()
        }));
        assert_eq!(sent_keys(&body), ["subject"]);
        let body = edit(crate::store::LocalEventFields {
            subject: "Sync v3".into(),
            ..zoned_event_fields()
        });
        assert_eq!(sent_keys(&body), ["subject"]);
        // A move that names the zones sends their new wall clocks.
        let body = edit(crate::store::LocalEventFields {
            subject: "Sync v3".into(),
            start_utc: "2026-07-20T08:00:00Z".into(),
            start_local: "2026-07-20T10:00:00".into(),
            ..zoned_event_fields()
        });
        let sent = json::parse(&body).unwrap();
        assert_eq!(sent_keys(&body), ["start", "end", "isAllDay"]);
        assert_eq!(
            sent.get("start").unwrap().to_string(),
            r#"{"dateTime":"2026-07-20T10:00:00","timeZone":"W. Europe Standard Time"}"#
        );
        assert_eq!(
            sent.get("end").unwrap().to_string(),
            r#"{"dateTime":"2026-07-20T08:30:00","timeZone":"Europe/London"}"#
        );
        // A move without a zone sends UTC, never the stored wall clock.
        let body = edit(unnamed(crate::store::LocalEventFields {
            subject: "Sync v3".into(),
            start_utc: "2026-07-20T06:00:00Z".into(),
            ..zoned_event_fields()
        }));
        let sent = json::parse(&body).unwrap();
        assert_eq!(
            sent.get("start").unwrap().to_string(),
            r#"{"dateTime":"2026-07-20T06:00:00","timeZone":"UTC"}"#
        );
        assert_eq!(
            sent.get("end").unwrap().to_string(),
            r#"{"dateTime":"2026-07-20T07:30:00","timeZone":"UTC"}"#
        );
    }

    #[test]
    fn update_event_series_patches_master_with_stored_body() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/events/M%2F1".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        // No row stored: the op carries everything it sends.
        let store = Store::open_in_memory().unwrap();
        let body = r#"{"subject":"Review","recurrence":{"pattern":{"type":"daily","interval":1},"range":{"type":"noEnd","startDate":"2026-07-14"}}}"#;
        apply_op(
            &c,
            &store,
            &OutboxOp::UpdateEventSeries {
                id: "M/1".into(),
                body: body.into(),
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(reqs[0].path, "/me/events/M%2F1");
        assert_eq!(reqs[0].body, body);
    }

    #[test]
    fn update_event_series_local_id_makes_no_request() {
        let srv = FakeServer::start(vec![]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::UpdateEventSeries {
                id: "local:series".into(),
                body: r#"{"subject":"x"}"#.into(),
            },
        )
        .unwrap();
        assert!(srv.requests().is_empty());
    }

    #[test]
    fn delete_occurrence_and_series_delete_their_ids() {
        // Pin (#2022): "delete this occurrence" is `DeleteEvent` on the
        // occurrence's id, "delete the series" on the master's.
        let srv = FakeServer::start(vec![Route {
            method: "DELETE".into(),
            path_prefix: "/me/events/".into(),
            status: 204,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        for id in ["OCC=1", "MASTER"] {
            apply_op(&c, &store, &OutboxOp::DeleteEvent { id: id.into() }).unwrap();
        }
        let paths: Vec<(String, String)> = srv
            .requests()
            .into_iter()
            .map(|r| (r.method, r.path))
            .collect();
        assert_eq!(
            paths,
            [
                ("DELETE".to_string(), "/me/events/OCC%3D1".to_string()),
                ("DELETE".to_string(), "/me/events/MASTER".to_string()),
            ]
        );
    }

    #[test]
    fn cancel_event_local_id_makes_no_request() {
        let srv = FakeServer::start(vec![]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::CancelEvent {
                id: "local:never".into(),
                comment: Some("off".into()),
            },
        )
        .unwrap();
        assert!(srv.requests().is_empty());
    }

    /// A `dismissReminder` / `snoozeReminder` answer: `200`, no body.
    fn reminder_route(path: &str) -> Route {
        Route {
            method: "POST".into(),
            path_prefix: path.into(),
            status: 200,
            headers: vec![],
            body: "".into(),
        }
    }

    #[test]
    fn dismiss_reminder_posts_dismiss_reminder() {
        let srv = FakeServer::start(vec![reminder_route("/me/events/EV1/dismissReminder")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(&c, &store, &OutboxOp::DismissReminder { id: "EV1".into() }).unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/events/EV1/dismissReminder");
        assert_eq!(reqs[0].body, "");
    }

    #[test]
    fn snooze_reminder_posts_snooze_reminder() {
        let srv = FakeServer::start(vec![reminder_route("/me/events/EV1/snoozeReminder")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::SnoozeReminder {
                id: "EV1".into(),
                new_reminder_utc: "2026-11-08T10:15:00Z".into(),
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/events/EV1/snoozeReminder");
        assert_eq!(
            reqs[0].body,
            r#"{"newReminderTime":{"dateTime":"2026-11-08T10:15:00","timeZone":"UTC"}}"#
        );
    }

    #[test]
    fn reminder_ops_local_id_make_no_request() {
        let srv = FakeServer::start(vec![]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        for op in [
            OutboxOp::DismissReminder {
                id: "local:never".into(),
            },
            OutboxOp::SnoozeReminder {
                id: "local:never".into(),
                new_reminder_utc: "2026-11-08T10:15:00Z".into(),
            },
        ] {
            apply_op(&c, &store, &op).unwrap();
        }
        assert!(srv.requests().is_empty());
    }

    #[test]
    fn cancel_event_posts_cancel() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/EV1/cancel".into(),
            status: 202,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::CancelEvent {
                id: "EV1".into(),
                comment: Some("Moved".into()),
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/events/EV1/cancel");
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(sent.get("comment").and_then(Value::as_str), Some("Moved"));
    }

    #[test]
    fn cancel_event_404_and_refusals_are_errors() {
        // `apply_op` reports a 404 as is: the drain, not this layer, drops
        // the op and resets the calendars (#1985).
        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/events/GONE/cancel".into(),
                status: 404,
                headers: vec![],
                body: r#"{"error":{"code":"ErrorItemNotFound"}}"#.into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/events/MINE/cancel".into(),
                status: 400,
                headers: vec![],
                body: r#"{"error":{"code":"ErrorInvalidRequest"}}"#.into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let cancel = |id: &str| OutboxOp::CancelEvent {
            id: id.into(),
            comment: None,
        };
        assert_eq!(
            apply_op(&c, &store, &cancel("GONE")),
            Err(GraphError::NotFound)
        );
        assert!(matches!(
            apply_op(&c, &store, &cancel("MINE")),
            Err(GraphError::Http { status: 400, .. })
        ));
        assert_eq!(srv.requests().len(), 2);
    }

    #[test]
    fn forward_event_posts_forward() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/EV1/forward".into(),
            status: 202,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::ForwardEvent {
                id: "EV1".into(),
                to: vec![Recipient {
                    name: "Ann".into(),
                    address: "ann@x".into(),
                }],
                comment: Some("FYI".into()),
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].path, "/me/events/EV1/forward");
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(sent.get("comment").and_then(Value::as_str), Some("FYI"));
        let to = sent.get("toRecipients").and_then(Value::as_array).unwrap();
        assert_eq!(to.len(), 1);
        assert_eq!(
            to[0]
                .get("emailAddress")
                .and_then(|a| a.get("address"))
                .and_then(Value::as_str),
            Some("ann@x")
        );
    }

    #[test]
    fn apply_op_save_draft_errors_when_nothing_is_stored_for_the_id() {
        let srv = FakeServer::start(vec![]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let result = apply_op(
            &c,
            &store,
            &OutboxOp::SaveDraft {
                id: "local:missing".into(),
            },
        );
        assert!(matches!(result, Err(GraphError::Parse(_))));
    }

    #[test]
    fn parse_recipients_splits_name_and_address_and_bare_addresses() {
        let recipients = parse_recipients("Alice <alice@x>; bob@y");
        assert_eq!(recipients.len(), 2);
        assert_eq!(recipients[0].name, "Alice");
        assert_eq!(recipients[0].address, "alice@x");
        assert_eq!(recipients[1].name, "");
        assert_eq!(recipients[1].address, "bob@y");
    }

    #[test]
    fn parse_recipients_keeps_a_comma_in_an_encoded_display_name_as_one_recipient() {
        // Final v2 review, Fix 1: "Surname, Given" is the default corporate
        // directory display-name format. A `<...>`-wrapped part is always one
        // recipient, however many commas its name contains.
        let recipients = parse_recipients("Doe, John <john@x>");
        assert_eq!(recipients.len(), 1);
        assert_eq!(recipients[0].name, "Doe, John");
        assert_eq!(recipients[0].address, "john@x");
    }

    #[test]
    fn parse_recipients_splits_bare_comma_separated_addresses() {
        // The compose UI's To/Cc fields have no `<...>` structure at all, so a
        // user typing "a@x, b@y" into one field must still split into two
        // recipients.
        let recipients = parse_recipients("a@x, b@y");
        assert_eq!(recipients.len(), 2);
        assert_eq!(recipients[0].name, "");
        assert_eq!(recipients[0].address, "a@x");
        assert_eq!(recipients[1].name, "");
        assert_eq!(recipients[1].address, "b@y");
    }

    #[test]
    fn parse_recipients_handles_a_mix_of_comma_in_name_and_semicolon_separated_parts() {
        let recipients = parse_recipients("Doe, John <john@x>; Jane Roe <jane@y>");
        assert_eq!(recipients.len(), 2);
        assert_eq!(recipients[0].name, "Doe, John");
        assert_eq!(recipients[0].address, "john@x");
        assert_eq!(recipients[1].name, "Jane Roe");
        assert_eq!(recipients[1].address, "jane@y");
    }

    #[test]
    fn apply_op_save_draft_strips_the_name_wrapper_for_a_reply_style_draft() {
        // Regression test for the bug the coordinator's review caught: a
        // reply/forward draft (`sync::engine::store_composed_draft`) files
        // its message via `Store::upsert_message`, which encodes recipients
        // as `"Name <addr>; Name <addr>"` (`store::encode_recipients`) —
        // NOT the flat bare-address format a from-scratch local draft's
        // `update_draft_fields` produces. `parse_recipients` must strip that
        // wrapper, or Graph gets `"address":"Alice <alice@x>"` and rejects
        // it (400).
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/DRAFT1".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();

        store
            .upsert_folder(&MailFolder {
                id: "DRAFTS".into(),
                display_name: "Drafts".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some("drafts".into()),
            })
            .unwrap();
        // Same shape `create_reply`/`create_forward` hands `store_composed_
        // draft`: a Graph-minted id (not `local:`), `to`/`cc` as `Recipient`s
        // with a display name — `upsert_message` is what turns those into
        // the `"Name <addr>"` text this test is really exercising.
        let draft = Message {
            id: "DRAFT1".into(),
            conversation_id: "C1".into(),
            subject: "Re: Hi".into(),
            from: Recipient {
                name: "Me".into(),
                address: "me@x".into(),
            },
            to: vec![Recipient {
                name: "Alice".into(),
                address: "alice@x".into(),
            }],
            cc: vec![],
            received: "".into(),
            sent: "".into(),
            is_read: false,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: "".into(),
            is_draft: true,
            is_meeting_request: false,
            categories: Vec::new(),
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
        };
        store.upsert_message("DRAFTS", &draft).unwrap();
        store
            .put_body(
                "DRAFT1",
                &Body {
                    content_type: "html".into(),
                    content: "<p>quoted</p>".into(),
                },
            )
            .unwrap();

        apply_op(
            &c,
            &store,
            &OutboxOp::SaveDraft {
                id: "DRAFT1".into(),
            },
        )
        .unwrap();

        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH");
        let sent = json::parse(&reqs[0].body).unwrap();
        let to = sent.get("toRecipients").and_then(Value::as_array).unwrap();
        assert_eq!(
            to[0]
                .get("emailAddress")
                .and_then(|e| e.get("address"))
                .and_then(Value::as_str),
            Some("alice@x"),
            "expected the bare address, not the \"Name <addr>\" wrapper: {}",
            reqs[0].body
        );
    }

    #[test]
    fn apply_op_send_draft_keeps_a_comma_in_name_as_one_recipient_through_reply_and_send() {
        // Final v2 review, Fix 1: the exact bug — a reply/forward draft whose
        // recipient's display name is "Surname, Given" (the default corporate
        // directory format, e.g. "Doe, John") stores `to_recipients` as
        // `"Doe, John <john@x>"` via `store::encode_recipients`. The old
        // `parse_recipients`, which split on both `;` and `,`, cut that into
        // a bogus address-less `"Doe"` recipient plus `"John <john@x>"`,
        // which Graph would reject — so the reply looked sent locally (the
        // optimistic `mark_sent`/move-to-Sent already ran) but was never
        // actually delivered. This drives the full `SendDraft` path (update
        // the already-Graph-addressed draft, then send it) and asserts the
        // wire body carries exactly one recipient with the address from
        // inside `<>` and the full comma-containing name intact.
        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/messages/DRAFT1/send".into(),
                status: 202,
                headers: vec![],
                body: "".into(),
            },
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/messages/DRAFT1".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();

        store
            .upsert_folder(&MailFolder {
                id: "DRAFTS".into(),
                display_name: "Drafts".into(),
                parent_id: None,
                total_count: 0,
                unread_count: 0,
                well_known_name: Some("drafts".into()),
            })
            .unwrap();
        let draft = Message {
            id: "DRAFT1".into(),
            conversation_id: "C1".into(),
            subject: "Re: Hi".into(),
            from: Recipient {
                name: "Me".into(),
                address: "me@x".into(),
            },
            to: vec![Recipient {
                name: "Doe, John".into(),
                address: "john@x".into(),
            }],
            cc: vec![],
            received: "".into(),
            sent: "".into(),
            is_read: false,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: "".into(),
            is_draft: true,
            is_meeting_request: false,
            categories: Vec::new(),
            is_reminder_on: false,
            reminder_at: String::new(),
            reminder_signal_at: String::new(),
            flag_due_at: String::new(),
        };
        store.upsert_message("DRAFTS", &draft).unwrap();
        store
            .put_body(
                "DRAFT1",
                &Body {
                    content_type: "html".into(),
                    content: "<p>quoted</p>".into(),
                },
            )
            .unwrap();

        apply_op(
            &c,
            &store,
            &OutboxOp::SendDraft {
                id: "DRAFT1".into(),
            },
        )
        .unwrap();

        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH", "update_draft must run before send");
        let sent = json::parse(&reqs[0].body).unwrap();
        let to = sent.get("toRecipients").and_then(Value::as_array).unwrap();
        assert_eq!(
            to.len(),
            1,
            "a comma in the display name must not split it into two recipients: {}",
            reqs[0].body
        );
        assert_eq!(
            to[0]
                .get("emailAddress")
                .and_then(|e| e.get("address"))
                .and_then(Value::as_str),
            Some("john@x"),
            "expected the address from inside <>, not \"Doe\": {}",
            reqs[0].body
        );
        assert_eq!(
            to[0]
                .get("emailAddress")
                .and_then(|e| e.get("name"))
                .and_then(Value::as_str),
            Some("Doe, John")
        );
        assert!(reqs[1].path.starts_with("/me/messages/DRAFT1/send"));
    }

    fn route(method: &str, path_prefix: &str, status: u16, body: &str) -> Route {
        Route {
            method: method.into(),
            path_prefix: path_prefix.into(),
            status,
            headers: vec![],
            body: body.into(),
        }
    }

    #[test]
    fn apply_op_save_draft_by_a_reconciled_local_id_patches_and_never_creates() {
        // #1884: a compose window left open across a SaveDraft still holds
        // the local id; its next SaveDraft must patch the Graph draft.
        let srv = FakeServer::start(vec![
            route("PATCH", "/me/messages/GRAPH-5", 200, "{}"),
            route("POST", "/me/messages", 201, r#"{"id":"SECOND"}"#),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store.create_local_draft("Old", "b@x", "", "old").unwrap();
        store.reconcile_id(&local_id, "GRAPH-5").unwrap();
        store
            .update_draft_fields(&local_id, "Edited", "b@x", "", "", "<p>new</p>")
            .unwrap();

        apply_op(
            &c,
            &store,
            &OutboxOp::SaveDraft {
                id: local_id.clone(),
            },
        )
        .unwrap();

        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(reqs[0].path, "/me/messages/GRAPH-5");
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(sent.get("subject").and_then(Value::as_str), Some("Edited"));
        assert_eq!(store.current_id(&local_id).unwrap(), "GRAPH-5");
    }

    #[test]
    fn apply_op_send_draft_by_a_reconciled_local_id_patches_then_sends() {
        // #1881: the retry of a SendDraft whose create already happened.
        let srv = FakeServer::start(vec![
            route("POST", "/me/messages/GRAPH-7/send", 202, ""),
            route("PATCH", "/me/messages/GRAPH-7", 200, "{}"),
            route("POST", "/me/messages", 201, r#"{"id":"SECOND"}"#),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store.create_local_draft("Hi", "b@x", "", "body").unwrap();
        store.reconcile_id(&local_id, "GRAPH-7").unwrap();

        apply_op(&c, &store, &OutboxOp::SendDraft { id: local_id }).unwrap();

        let reqs = srv.requests();
        let seen: Vec<(&str, &str)> = reqs
            .iter()
            .map(|r| (r.method.as_str(), r.path.as_str()))
            .collect();
        assert_eq!(
            seen,
            vec![
                ("PATCH", "/me/messages/GRAPH-7"),
                ("POST", "/me/messages/GRAPH-7/send")
            ]
        );
    }

    #[test]
    fn apply_op_delete_by_a_reconciled_local_id_deletes_the_graph_id() {
        let srv = FakeServer::start(vec![route("DELETE", "/me/messages/", 204, "")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store.create_local_draft("Hi", "", "", "body").unwrap();
        store.reconcile_id(&local_id, "GRAPH-8").unwrap();

        apply_op(&c, &store, &OutboxOp::Delete { id: local_id }).unwrap();

        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "DELETE");
        assert_eq!(reqs[0].path, "/me/messages/GRAPH-8");
    }

    #[test]
    fn apply_op_delete_of_a_never_synced_local_id_makes_no_graph_call() {
        // A local: draft that never reached Graph has nothing to delete there
        // (before #1884 this sent DELETE /me/messages/local:… and was
        // quarantined).
        let srv = FakeServer::start(vec![route("DELETE", "/me/messages/", 204, "")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local_id = store.create_local_draft("Hi", "", "", "body").unwrap();
        store.delete_message(&local_id).unwrap();

        apply_op(&c, &store, &OutboxOp::Delete { id: local_id }).unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::Delete {
                id: "local:never".into(),
            },
        )
        .unwrap();

        assert!(srv.requests().is_empty());
    }

    // --- A flagged message's reminder (#2054) ------------------------------

    fn patch_route(path: &str) -> Route {
        Route {
            method: "PATCH".into(),
            path_prefix: path.into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }
    }

    #[test]
    fn dismiss_message_reminder_patches_the_resolved_id() {
        let srv = FakeServer::start(vec![patch_route("/me/messages/NEW1")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = store_moved_old1_to_new1();
        apply_op(
            &c,
            &store,
            &OutboxOp::DismissMessageReminder { id: "OLD1".into() },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(reqs[0].path, "/me/messages/NEW1");
        assert_eq!(
            reqs[0].body,
            r#"{"singleValueExtendedProperties":[{"id":"Boolean {00062008-0000-0000-C000-000000000046} Id 0x8503","value":"false"}]}"#
        );
    }

    #[test]
    fn snooze_message_reminder_patches_the_resolved_id() {
        let srv = FakeServer::start(vec![patch_route("/me/messages/NEW1")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = store_moved_old1_to_new1();
        apply_op(
            &c,
            &store,
            &OutboxOp::SnoozeMessageReminder {
                id: "OLD1".into(),
                new_reminder_utc: "2026-11-08T10:15:00Z".into(),
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].path, "/me/messages/NEW1");
        assert_eq!(
            reqs[0].body,
            r#"{"singleValueExtendedProperties":[{"id":"SystemTime {00062008-0000-0000-C000-000000000046} Id 0x8560","value":"2026-11-08T10:15:00Z"}]}"#
        );
    }

    #[test]
    fn message_reminder_op_of_a_local_id_makes_no_request() {
        let srv = FakeServer::start(vec![]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        for op in [
            OutboxOp::DismissMessageReminder {
                id: "local:never".into(),
            },
            OutboxOp::SnoozeMessageReminder {
                id: "local:never".into(),
                new_reminder_utc: "2026-11-08T10:15:00Z".into(),
            },
        ] {
            assert_eq!(apply_op(&c, &store, &op), Ok(Applied::Done));
        }
        assert!(srv.requests().is_empty());
    }

    // --- Contacts (#2063) ---------------------------------------------

    fn contact_route(method: &str, prefix: &str, status: u16, body: &str) -> Route {
        Route {
            method: method.into(),
            path_prefix: prefix.into(),
            status,
            headers: vec![],
            body: body.into(),
        }
    }

    fn ann_fields() -> crate::graph::model::ContactFields {
        crate::graph::model::ContactFields {
            given_name: "Ann".into(),
            surname: "Cole".into(),
            email_addresses: vec![Recipient {
                name: "Ann".into(),
                address: "ann@x.com".into(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn create_contact_reconciles_the_local_id() {
        let srv = FakeServer::start(vec![contact_route(
            "POST",
            "/me/contacts",
            201,
            r#"{"id":"G1","changeKey":"ck1","displayName":"Ann Cole"}"#,
        )]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local = store.create_local_contact(&ann_fields()).unwrap();
        let applied = apply_op(&c, &store, &OutboxOp::CreateContact { id: local.clone() }).unwrap();
        assert_eq!(
            applied,
            Applied::ContactCreated {
                local_id: local.clone(),
                id: "G1".into()
            }
        );
        let row = store.folder_contact("G1").unwrap().unwrap();
        assert_eq!(row.change_key, "ck1");
        assert_eq!(row.fields.display_name, "Ann Cole");
        assert_eq!(store.resolve_contact_id(&local).unwrap(), "G1");
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(sent.get("givenName").and_then(Value::as_str), Some("Ann"));
        assert!(
            sent.get("displayName").is_none(),
            "an empty property is not sent"
        );
    }

    #[test]
    fn create_contact_retry_after_reconcile_sends_nothing() {
        let srv = FakeServer::start(vec![]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local = store.create_local_contact(&ann_fields()).unwrap();
        let created = crate::graph::model::FolderContact {
            id: "G1".into(),
            ..Default::default()
        };
        store.reconcile_created_contact(&local, &created).unwrap();
        let applied = apply_op(&c, &store, &OutboxOp::CreateContact { id: local }).unwrap();
        assert_eq!(applied, Applied::Done);
        assert!(srv.requests().is_empty(), "no second POST");
    }

    #[test]
    fn create_contact_of_a_deleted_row_sends_nothing() {
        let srv = FakeServer::start(vec![]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local = store.create_local_contact(&ann_fields()).unwrap();
        store.delete_contact(&local).unwrap();
        let applied = apply_op(&c, &store, &OutboxOp::CreateContact { id: local }).unwrap();
        assert_eq!(applied, Applied::Done);
        assert!(srv.requests().is_empty());
    }

    #[test]
    fn update_contact_of_a_local_id_sends_nothing() {
        let srv = FakeServer::start(vec![]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local = store.create_local_contact(&ann_fields()).unwrap();
        let op = OutboxOp::UpdateContact {
            id: local.clone(),
            fields: ContactFieldSet::SURNAME,
        };
        apply_op(&c, &store, &op).unwrap();
        // Nor does a delete of one, nor an update of a row gone.
        apply_op(&c, &store, &OutboxOp::DeleteContact { id: local }).unwrap();
        let gone = OutboxOp::UpdateContact {
            id: "G9".into(),
            fields: ContactFieldSet::SURNAME,
        };
        apply_op(&c, &store, &gone).unwrap();
        assert!(srv.requests().is_empty());
    }

    #[test]
    fn update_contact_after_create_patches_the_graph_id() {
        let srv = FakeServer::start(vec![contact_route(
            "PATCH",
            "/me/contacts/G1",
            200,
            r#"{"id":"G1"}"#,
        )]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let local = store.create_local_contact(&ann_fields()).unwrap();
        let created = crate::graph::model::FolderContact {
            id: "G1".into(),
            ..Default::default()
        };
        store.reconcile_created_contact(&local, &created).unwrap();
        let mut f = ann_fields();
        f.surname = "Cole-Smith".into();
        f.job_title = "Lead".into();
        let changed = store.update_contact_fields(&local, &f).unwrap();
        apply_op(
            &c,
            &store,
            &OutboxOp::UpdateContact {
                id: local,
                fields: changed,
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(reqs[0].path, "/me/contacts/G1");
        assert_eq!(
            reqs[0].body,
            r#"{"surname":"Cole-Smith","jobTitle":"Lead"}"#
        );
    }

    #[test]
    fn delete_contact_404_is_success() {
        let srv = FakeServer::start(vec![contact_route("DELETE", "/me/contacts/G1", 404, "{}")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let store = Store::open_in_memory().unwrap();
        let applied = apply_op(&c, &store, &OutboxOp::DeleteContact { id: "G1".into() }).unwrap();
        assert_eq!(applied, Applied::Done);
        assert_eq!(srv.requests()[0].method, "DELETE");
    }
}
