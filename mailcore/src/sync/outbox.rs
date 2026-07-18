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

use crate::graph::client::{GraphClient, GraphError};
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
/// holds for a draft (compose stores whatever the user typed there, e.g.
/// `bob@x` or `bob@x; carol@x` — unlike a synced message's columns, there's
/// no `Name <addr>` structure to preserve since compose never captured a
/// display name) back into `Recipient`s for `create_draft`/`update_draft`.
/// Splits on both `;` and `,` since compose doesn't mandate one separator,
/// trims whitespace, and drops empty tokens (a trailing separator, or an
/// empty field) rather than sending a blank recipient to Graph.
fn parse_recipients(raw: &str) -> Vec<Recipient> {
    raw.split([';', ','])
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
}
