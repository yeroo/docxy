//! Pure grouping of `MessageRow`s into conversation `Thread`s for the
//! threaded message-list view. No DB, no I/O — takes rows (as the store
//! returns them) and returns grouped, self-consistently-ordered threads, so
//! it can be unit-tested without a store and never depends on the caller's
//! row ordering.

use crate::store::MessageRow;

/// The grouping key for a message: its `conversation_id` when non-empty,
/// else `msg:<id>` so a message Graph gave no conversation for becomes its
/// own singleton thread rather than being merged with every other blank one.
pub fn conv_key(m: &MessageRow) -> String {
    if m.conversation_id.is_empty() {
        format!("msg:{}", m.id)
    } else {
        m.conversation_id.clone()
    }
}

/// One conversation: its messages (oldest→newest) plus display aggregates.
#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    pub key: String,
    pub messages: Vec<MessageRow>,
    pub latest_received: String,
    pub unread_count: usize,
    pub any_flagged: bool,
    pub any_attachments: bool,
    pub subject: String,
    pub participants: Vec<String>,
}

/// Groups `rows` by `conv_key`. Within a thread, messages are sorted by
/// `received_at` ascending; threads are ordered by `latest_received`
/// descending, tie-broken by `key` ascending — so the result is independent
/// of the input ordering.
pub fn build_threads(rows: &[MessageRow]) -> Vec<Thread> {
    let mut groups: Vec<(String, Vec<MessageRow>)> = Vec::new();
    for m in rows {
        let key = conv_key(m);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, msgs)) => msgs.push(m.clone()),
            None => groups.push((key, vec![m.clone()])),
        }
    }

    let mut threads: Vec<Thread> = groups
        .into_iter()
        .map(|(key, mut messages)| {
            messages.sort_by(|a, b| a.received_at.cmp(&b.received_at));
            let latest_received = messages
                .last()
                .map(|m| m.received_at.clone())
                .unwrap_or_default();
            let unread_count = messages.iter().filter(|m| !m.is_read).count();
            let any_flagged = messages.iter().any(|m| m.is_flagged);
            let any_attachments = messages.iter().any(|m| m.has_attachments);
            // Latest non-empty subject (walk newest→oldest).
            let subject = messages
                .iter()
                .rev()
                .map(|m| m.subject.as_str())
                .find(|s| !s.is_empty())
                .unwrap_or("")
                .to_string();
            // Unique participant names, oldest→newest first-seen order.
            let mut participants: Vec<String> = Vec::new();
            for m in &messages {
                if !m.from_name.is_empty() && !participants.contains(&m.from_name) {
                    participants.push(m.from_name.clone());
                }
            }
            Thread {
                key,
                messages,
                latest_received,
                unread_count,
                any_flagged,
                any_attachments,
                subject,
                participants,
            }
        })
        .collect();

    threads.sort_by(|a, b| {
        b.latest_received
            .cmp(&a.latest_received)
            .then_with(|| a.key.cmp(&b.key))
    });
    threads
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MessageRow;

    fn row(id: &str, conv: &str, recv: &str, from: &str, subj: &str, read: bool) -> MessageRow {
        MessageRow {
            id: id.into(),
            folder_id: "inbox".into(),
            conversation_id: conv.into(),
            subject: subj.into(),
            from_name: from.into(),
            from_addr: format!("{from}@x"),
            to_recipients: String::new(),
            cc_recipients: String::new(),
            received_at: recv.into(),
            sent_at: String::new(),
            is_read: read,
            is_flagged: false,
            has_attachments: false,
            importance: "normal".into(),
            preview: String::new(),
            is_draft: false,
            bcc_recipients: String::new(),
            is_meeting_request: false,
            categories: Vec::new(),
        }
    }

    #[test]
    fn groups_by_conversation_and_derives_aggregates() {
        // Two conversations; c1 has three messages (two unread), c2 one.
        let rows = vec![
            row("a", "c1", "2026-07-10T09:00:00Z", "Ann", "Q3 plan", true),
            row("b", "c2", "2026-07-11T09:00:00Z", "Zed", "Lunch", false),
            row(
                "c",
                "c1",
                "2026-07-12T09:00:00Z",
                "Bob",
                "Re: Q3 plan",
                false,
            ),
            row("d", "c1", "2026-07-11T12:00:00Z", "Ann", "", false),
        ];
        let threads = build_threads(&rows);
        // c1 is most-recent (latest 07-12) so it sorts first.
        assert_eq!(threads.len(), 2);
        assert_eq!(threads[0].key, "c1");
        assert_eq!(threads[0].messages.len(), 3);
        // messages sorted oldest->newest by received_at
        let ids: Vec<&str> = threads[0].messages.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["a", "d", "c"]);
        assert_eq!(threads[0].latest_received, "2026-07-12T09:00:00Z");
        assert_eq!(threads[0].unread_count, 2);
        // participants: unique from_name in oldest->newest order
        assert_eq!(
            threads[0].participants,
            vec!["Ann".to_string(), "Bob".to_string()]
        );
        // subject: latest non-empty (message "c", since "d" is empty)
        assert_eq!(threads[0].subject, "Re: Q3 plan");
        assert_eq!(threads[1].key, "c2");
    }

    #[test]
    fn blank_conversation_id_is_a_singleton_keyed_by_id() {
        let rows = vec![
            row("x", "", "2026-07-10T09:00:00Z", "Ann", "one", false),
            row("y", "", "2026-07-11T09:00:00Z", "Bob", "two", false),
        ];
        let threads = build_threads(&rows);
        assert_eq!(threads.len(), 2);
        assert!(threads.iter().all(|t| t.messages.len() == 1));
        assert!(threads.iter().any(|t| t.key == "msg:x"));
        assert!(threads.iter().any(|t| t.key == "msg:y"));
    }

    #[test]
    fn flag_and_attachment_aggregate_across_the_thread() {
        let mut a = row("a", "c1", "2026-07-10T09:00:00Z", "Ann", "s", true);
        let mut b = row("b", "c1", "2026-07-11T09:00:00Z", "Bob", "s", true);
        a.is_flagged = true;
        b.has_attachments = true;
        let threads = build_threads(&[a, b]);
        assert_eq!(threads.len(), 1);
        assert!(threads[0].any_flagged);
        assert!(threads[0].any_attachments);
        assert_eq!(threads[0].unread_count, 0);
    }
}
