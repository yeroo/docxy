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
//! those two ops.

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
        OutboxOp::Move { id, dest } => client.move_message(id, dest).map(|_new_id| ()),
        OutboxOp::Delete { id } => client.delete_message(id),
        OutboxOp::SaveDraft { id } => ensure_draft_on_graph(client, store, id).map(|_id| ()),
        OutboxOp::SendDraft { id } => {
            let graph_id = ensure_draft_on_graph(client, store, id)?;
            client.send_draft(&graph_id)
        }
        OutboxOp::RespondEvent { id, kind, comment } => {
            client.respond_event(id, rsvp_kind(kind), comment.as_deref(), true)
        }
    }
}

/// Maps the `response_status` vocabulary `Store::set_event_response` writes
/// locally — and `OutboxOp::RespondEvent`'s `kind` field carries — to the
/// RSVP action `GraphClient::respond_event` sends: `"declined"` → `Decline`,
/// `"tentativelyAccepted"` → `Tentative`. Anything else (including the
/// common case, `"accepted"`) maps to `Accept` — a safe default for an
/// unrecognized value rather than silently dropping the RSVP (this crate's
/// usual "default rather than fail" convention; see e.g. `Event::from_json`).
fn rsvp_kind(kind: &str) -> RsvpKind {
    match kind {
        "declined" => RsvpKind::Decline,
        "tentativelyAccepted" => RsvpKind::Tentative,
        _ => RsvpKind::Accept,
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

    if let Some(local_id) = id.strip_prefix("local:") {
        let created = client.create_draft(&body.content, &row.subject, &to, &cc)?;
        store
            .reconcile_id(&format!("local:{local_id}"), &created.id)
            .map_err(|e| GraphError::Parse(e.to_string()))?;
        Ok(created.id)
    } else {
        client.update_draft(id, &body.content, &row.subject, &to, &cc)?;
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
/// `bob@x; carol@x` — no `Name <addr>` structure, since compose never
/// captured a display name there). Splits on both `;` and `,` since compose
/// doesn't mandate one separator, trims whitespace, and drops empty tokens
/// (a trailing separator, or an empty field) rather than sending a blank
/// recipient to Graph. Each non-empty part is then parsed by
/// `parse_one_recipient`, which handles both shapes.
fn parse_recipients(raw: &str) -> Vec<Recipient> {
    raw.split([';', ','])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_one_recipient)
        .collect()
}

/// Parses one recipient part, in either of the two shapes `parse_recipients`
/// can see: `Name <addr>` (a synced message's — or a reply/forward draft's —
/// column, from `store::encode_recipients`) or a bare `addr` (a from-scratch
/// local draft's flat compose input). Recognized as the former only when
/// `<` is followed later by a `>` (`part.find('<')` before `part.rfind('>')`)
/// — anything else (including a lone `<` or `>`, which a real address never
/// contains) falls back to treating the whole trimmed part as a bare
/// address with an empty name, same as before this parsed the wrapped form
/// at all.
fn parse_one_recipient(part: &str) -> Recipient {
    if let (Some(open), Some(close)) = (part.find('<'), part.rfind('>')) {
        if open < close {
            return Recipient {
                name: part[..open].trim().to_string(),
                address: part[open + 1..close].trim().to_string(),
            };
        }
    }
    Recipient {
        name: String::new(),
        address: part.to_string(),
    }
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
}
