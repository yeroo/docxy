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
//! would ever do it (unlike `Move`, Graph never reports a `local:` id back
//! through delta sync, so there's no later point where this reconciliation
//! would happen for free). So `apply_op` takes a `&Store` too, used only by
//! those two ops — plus `CreateEvent`/`UpdateEvent`, which need the same
//! store access for the same reason: `event_input_for` reads the event's
//! currently-stored fields to build the `EventInput` Graph is sent, and
//! `CreateEvent` reconciles a `local:` id to its Graph id afterward exactly
//! like `SaveDraft`/`SendDraft` do for drafts.

use crate::graph::client::{GraphClient, GraphError, RsvpKind};
use crate::graph::model::Recipient;
use crate::store::{OutboxOp, Store};

/// Dispatches `op` to the Graph mutation it represents.
///
/// `Move` returns Graph's newly-minted message id, but nothing here needs
/// it: the local row is reconciled by the next delta sync (which will see
/// the old id removed and the new one added), so it's discarded.
pub fn apply_op(client: &GraphClient, store: &Store, op: &OutboxOp) -> Result<(), GraphError> {
    match op {
        OutboxOp::MarkRead { id, read } => client.mark_read(id, *read),
        OutboxOp::SetFlag { id, flagged } => client.set_flag(id, *flagged),
        OutboxOp::SetCategories { id, categories } => client.set_message_categories(id, categories),
        OutboxOp::Move { id, dest } => client.move_message(id, dest).map(|_new_id| ()),
        OutboxOp::Delete { id } => client.delete_message(id),
        OutboxOp::SaveDraft { id } => ensure_draft_on_graph(client, store, id).map(|_id| ()),
        OutboxOp::SendDraft { id } => {
            let graph_id = ensure_draft_on_graph(client, store, id)?;
            // Upload each pending attachment to the (now-on-Graph) draft before
            // sending. A file-read or upload error returns here, so the drain's
            // retry/quarantine policy applies and the attachments are NOT
            // cleared — the send hasn't happened, so a retry is clean.
            for att in store
                .outbound_attachments(&graph_id)
                .map_err(|e| GraphError::Parse(e.to_string()))?
            {
                let bytes = std::fs::read(&att.path).map_err(|e| {
                    GraphError::Parse(format!("cannot read attachment {}: {e}", att.path))
                })?;
                client.add_attachment(
                    &graph_id,
                    &att.name,
                    &content_type_for(&att.name),
                    &bytes,
                )?;
            }
            client.send_draft(&graph_id)?;
            store
                .clear_outbound_attachments(&graph_id)
                .map_err(|e| GraphError::Parse(e.to_string()))?;
            Ok(())
        }
        OutboxOp::RespondEvent { id, kind, comment } => {
            // An unrecognized `kind` must NOT silently fall back to
            // accepting: that's the worst possible default for an action
            // with an external, user-visible side effect (a corrupt outbox
            // row would otherwise accept a meeting no one asked to accept).
            // Erroring here — rather than defaulting — makes the drain
            // loop's normal 4xx retry/quarantine policy handle it: the op
            // is retried, then quarantined after `MAX_OP_ATTEMPTS` like any
            // other op Graph keeps rejecting, without ever calling Graph
            // with a guessed action.
            let rsvp = rsvp_kind(kind)
                .ok_or_else(|| GraphError::Parse(format!("unrecognized RSVP kind: {kind}")))?;
            client.respond_event(id, rsvp, comment.as_deref(), true, None)
        }
        OutboxOp::CreateEvent { id } => {
            let input = event_input_for(store, id)?;
            let created = client.create_event(&input)?;
            store
                .reconcile_event_id(id, &created.id)
                .map_err(|e| GraphError::Parse(e.to_string()))?;
            Ok(())
        }
        OutboxOp::UpdateEvent { id } => {
            let input = event_input_for(store, id)?;
            client.update_event(id, &input)
        }
        OutboxOp::DeleteEvent { id } => {
            // A local:-only event never reached Graph; nothing to delete there.
            if id.starts_with("local:") {
                Ok(())
            } else {
                client.delete_event(id)
            }
        }
    }
}

/// Reads the stored event `id` and builds the `EventInput` the create/update
/// calls take. Errors if the event isn't in the store (a corrupt/raced op).
fn event_input_for(
    store: &Store,
    id: &str,
) -> Result<crate::graph::client::EventInput, GraphError> {
    let d = store
        .event_for_send(id)
        .map_err(|e| GraphError::Parse(e.to_string()))?
        .ok_or_else(|| GraphError::Parse(format!("no local event stored for {id}")))?;
    Ok(crate::graph::client::EventInput {
        subject: d.subject,
        start_utc: d.start_utc,
        end_utc: d.end_utc,
        is_all_day: d.is_all_day,
        location: d.location,
        attendees: d.attendees,
        body_html: d.body_html,
        recurrence: d.recurrence,
    })
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

/// Maps the `response_status` vocabulary `Store::set_event_response` writes
/// locally — and `OutboxOp::RespondEvent`'s `kind` field carries — to the
/// RSVP action `GraphClient::respond_event` sends: `"accepted"` → `Accept`,
/// `"declined"` → `Decline`, `"tentativelyAccepted"` → `Tentative`. `None`
/// for anything else — see `apply_op`'s `RespondEvent` arm for why an
/// unrecognized value must error rather than silently pick one.
fn rsvp_kind(kind: &str) -> Option<RsvpKind> {
    match kind {
        "accepted" => Some(RsvpKind::Accept),
        "declined" => Some(RsvpKind::Decline),
        "tentativelyAccepted" => Some(RsvpKind::Tentative),
        _ => None,
    }
}

/// Ensures the draft addressed by `id` exists on Graph with its
/// currently-stored fields, returning the id it now lives under on Graph.
///
/// If `id` is a `local:` id (never yet pushed), loads the draft's stored
/// subject/recipients/body, `create_draft`s it, and reconciles the store
/// (`Store::reconcile_id`) from `id` to the Graph-minted id — so the local
/// row and body are addressable by the id the rest of Graph (and the next
/// `SendDraft`, if this was a `SaveDraft`) now knows it by. Otherwise (`id`
/// already is a Graph id) `update_draft`s it in place with whatever is
/// currently stored, and returns `id` unchanged.
///
/// Shared by `SaveDraft` (which only needs this) and `SendDraft` (which
/// needs it done first, then sends the result).
fn ensure_draft_on_graph(
    client: &GraphClient,
    store: &Store,
    id: &str,
) -> Result<String, GraphError> {
    let (row, body) = store
        .draft(id)
        .map_err(|e| GraphError::Parse(e.to_string()))?
        .ok_or_else(|| GraphError::Parse(format!("no local draft stored for {id}")))?;
    let to = parse_recipients(&row.to_recipients);
    let cc = parse_recipients(&row.cc_recipients);
    let bcc = parse_recipients(&row.bcc_recipients);

    if let Some(local_id) = id.strip_prefix("local:") {
        let created = client.create_draft(&body.content, &row.subject, &to, &cc, &bcc)?;
        store
            .reconcile_id(&format!("local:{local_id}"), &created.id)
            .map_err(|e| GraphError::Parse(e.to_string()))?;
        Ok(created.id)
    } else {
        client.update_draft(id, &body.content, &row.subject, &to, &cc, &bcc)?;
        Ok(id.to_string())
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
    fn apply_op_dispatches_move_and_discards_new_id() {
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
        assert_eq!(result, Ok(()));
        let reqs = srv.requests();
        assert_eq!(reqs[0].body, r#"{"destinationId":"DEST"}"#);
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
            },
        )
        .unwrap();
        let reqs = srv.requests();
        assert!(reqs[0].path.ends_with("/decline"));
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(sent.get("comment").and_then(Value::as_str), Some(""));
    }

    #[test]
    fn apply_op_respond_event_rejects_an_unrecognized_kind_without_calling_graph() {
        // A corrupt/unexpected outbox row (`kind` not one of the three
        // `Store::set_event_response` vocabulary values) must NOT default to
        // accepting the meeting — see `rsvp_kind`'s doc comment. No route is
        // stubbed at all: if this silently defaulted to `Accept` and called
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

        // Reconciled: the old local id is gone, the Graph id has the draft.
        assert!(store.draft(&local_id).unwrap().is_none());
        let (row, body) = store.draft("GRAPH-1").unwrap().unwrap();
        assert_eq!(row.subject, "Hi");
        assert_eq!(body.content, "<p>hello</p>");
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

        assert!(store.draft(&local_id).unwrap().is_none());
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
        assert!(!store.outbound_attachments("GRAPH-2").unwrap().is_empty());
        // And send was never reached.
        assert!(!srv.requests().iter().any(|r| r.path.ends_with("/send")));
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
            attendees: vec![("Bob".into(), "bob@x.com".into())],
            recurrence: None,
        }
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
}
