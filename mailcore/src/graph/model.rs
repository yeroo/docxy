//! Plain Rust structs mirroring the Microsoft Graph JSON fields lookxy
//! uses, plus `from_json` constructors that parse them out of an already
//! -parsed `crate::json::Value`. No HTTP here — see the REST client
//! elsewhere in `graph` for that.

use crate::json::Value;

/// A mail folder (e.g. Inbox, Sent Items, or a user-created folder).
#[derive(Debug, Clone, PartialEq)]
pub struct MailFolder {
    pub id: String,
    pub display_name: String,
    pub parent_id: Option<String>,
    pub total_count: i64,
    pub unread_count: i64,
    pub well_known_name: Option<String>,
}

impl MailFolder {
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(MailFolder {
            id: str_field(v, "id"),
            display_name: str_field(v, "displayName"),
            parent_id: opt_str_field(v, "parentFolderId"),
            total_count: v.get("totalItemCount").and_then(Value::as_i64).unwrap_or(0),
            unread_count: v
                .get("unreadItemCount")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            well_known_name: opt_str_field(v, "wellKnownName"),
        })
    }
}

/// An email address with an optional display name, as Graph's
/// `emailAddress` object.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Recipient {
    pub name: String,
    pub address: String,
}

impl Recipient {
    pub fn from_json(v: &Value) -> Option<Self> {
        let addr = v.get("emailAddress")?;
        Some(Recipient {
            name: str_field(addr, "name"),
            address: str_field(addr, "address"),
        })
    }
}

/// A mail message, as returned by Graph's `/messages` endpoints.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub subject: String,
    pub from: Recipient,
    pub to: Vec<Recipient>,
    pub cc: Vec<Recipient>,
    pub received: String,
    pub sent: String,
    pub is_read: bool,
    pub is_flagged: bool,
    pub has_attachments: bool,
    pub importance: String,
    pub preview: String,
    /// Graph's `isDraft`: true for messages still sitting in Drafts that
    /// haven't been sent. Mirrored locally as `messages.is_draft` so a
    /// draft displays like any other message in its folder.
    pub is_draft: bool,
}

impl Message {
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(Message {
            id: str_field(v, "id"),
            conversation_id: str_field(v, "conversationId"),
            subject: str_field(v, "subject"),
            from: v
                .get("from")
                .and_then(Recipient::from_json)
                .unwrap_or_default(),
            to: recipient_list(v, "toRecipients"),
            cc: recipient_list(v, "ccRecipients"),
            received: str_field(v, "receivedDateTime"),
            sent: str_field(v, "sentDateTime"),
            is_read: v.get("isRead").and_then(Value::as_bool).unwrap_or(false),
            is_flagged: parse_flag(v),
            has_attachments: v
                .get("hasAttachments")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            importance: str_field(v, "importance"),
            preview: str_field(v, "bodyPreview"),
            is_draft: v.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// A message body: its content plus whether that content is `text` or
/// `html`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Body {
    pub content_type: String,
    pub content: String,
}

impl Body {
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(Body {
            content_type: str_field(v, "contentType"),
            content: str_field(v, "content"),
        })
    }
}

/// Metadata for an attachment (no bytes — those are fetched separately).
#[derive(Debug, Clone, PartialEq)]
pub struct AttachmentMeta {
    pub id: String,
    pub name: String,
    pub content_type: String,
    pub size: i64,
    pub is_inline: bool,
}

impl AttachmentMeta {
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(AttachmentMeta {
            id: str_field(v, "id"),
            name: str_field(v, "name"),
            content_type: str_field(v, "contentType"),
            size: v.get("size").and_then(Value::as_i64).unwrap_or(0),
            is_inline: v.get("isInline").and_then(Value::as_bool).unwrap_or(false),
        })
    }
}

/// One entry of a delta sync page: either an upserted message or the id of
/// a message that was removed since the last sync.
#[derive(Debug, Clone, PartialEq)]
pub enum DeltaItem {
    Upsert(Message),
    Delete(String),
}

/// A page of results from Graph's delta query, plus the pagination/delta
/// tokens to continue syncing.
#[derive(Debug, Clone, PartialEq)]
pub struct DeltaPage {
    pub items: Vec<DeltaItem>,
    pub next_link: Option<String>,
    pub delta_link: Option<String>,
}

impl DeltaPage {
    pub fn from_json(v: &Value) -> Option<Self> {
        let items = v
            .get("value")?
            .as_array()?
            .iter()
            .filter_map(|item| {
                if item.get("@removed").is_some() {
                    let id = item.get("id").and_then(Value::as_str).unwrap_or("");
                    Some(DeltaItem::Delete(id.to_string()))
                } else {
                    Message::from_json(item).map(DeltaItem::Upsert)
                }
            })
            .collect();
        Some(DeltaPage {
            items,
            next_link: opt_str_field(v, "@odata.nextLink"),
            delta_link: opt_str_field(v, "@odata.deltaLink"),
        })
    }
}

/// Reads Graph's `flag.flagStatus` field, true when it equals `"flagged"`.
pub fn parse_flag(v: &Value) -> bool {
    v.get("flag")
        .and_then(|f| f.get("flagStatus"))
        .and_then(Value::as_str)
        == Some("flagged")
}

/// Reads a string field, defaulting to `""` when absent or not a string.
fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

/// Reads a string field as `Option<String>`, `None` when absent or not a
/// string.
fn opt_str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Reads a list of `Recipient`s from an array field, defaulting to empty
/// when absent.
fn recipient_list(v: &Value, key: &str) -> Vec<Recipient> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Recipient::from_json).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse;

    #[test]
    fn parses_message() {
        let v = parse(
            r#"{
          "id":"AAA","conversationId":"CID","subject":"Hi",
          "from":{"emailAddress":{"name":"A","address":"a@x"}},
          "toRecipients":[{"emailAddress":{"name":"B","address":"b@x"}}],
          "ccRecipients":[],
          "receivedDateTime":"2026-07-17T10:00:00Z","sentDateTime":"2026-07-17T09:59:00Z",
          "isRead":false,"hasAttachments":true,"importance":"normal",
          "bodyPreview":"hello",
          "flag":{"flagStatus":"flagged"},
          "isDraft":true
        }"#,
        )
        .unwrap();
        let m = Message::from_json(&v).unwrap();
        assert_eq!(m.id, "AAA");
        assert_eq!(m.from.address, "a@x");
        assert_eq!(m.to.len(), 1);
        assert!(!m.is_read);
        assert!(m.is_flagged);
        assert!(m.has_attachments);
        assert!(m.is_draft);
    }

    #[test]
    fn parses_delta_page_with_removed() {
        let v = parse(r#"{
          "value":[
            {"id":"M1","subject":"a","from":{"emailAddress":{"name":"","address":""}},"receivedDateTime":"","sentDateTime":"","isRead":true,"conversationId":"","importance":"normal","bodyPreview":""},
            {"id":"M2","@removed":{"reason":"deleted"}}
          ],
          "@odata.deltaLink":"https://graph/delta?token=xyz"
        }"#).unwrap();
        let page = DeltaPage::from_json(&v).unwrap();
        assert_eq!(page.items.len(), 2);
        assert!(matches!(page.items[1], DeltaItem::Delete(ref id) if id == "M2"));
        assert_eq!(
            page.delta_link.as_deref(),
            Some("https://graph/delta?token=xyz")
        );
        assert!(page.next_link.is_none());
    }

    #[test]
    fn parses_folder() {
        let v = parse(r#"{"id":"F","displayName":"Inbox","parentFolderId":"root","totalItemCount":10,"unreadItemCount":3,"wellKnownName":"inbox"}"#).unwrap();
        let f = MailFolder::from_json(&v).unwrap();
        assert_eq!(f.display_name, "Inbox");
        assert_eq!(f.unread_count, 3);
        assert_eq!(f.well_known_name.as_deref(), Some("inbox"));
    }
}
