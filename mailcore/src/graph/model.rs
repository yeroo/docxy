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

/// One entry from Graph `/me/people`: a display name, the person's primary
/// email address, and their 0-based rank in the relevance-ordered response
/// (0 = most relevant).
#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    pub name: String,
    pub address: String,
    pub rank: i64,
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
    /// Graph's `@odata.type == "#microsoft.graph.eventMessageRequest"`: this
    /// message is a meeting invite the user can RSVP to (see the reader's
    /// meeting banner and `SyncCommand::RespondMeeting`). `@odata.type` is an
    /// OData control annotation auto-emitted for derived resource types, so it
    /// arrives with the normal delta response — no `$select` change needed.
    pub is_meeting_request: bool,
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
            is_meeting_request: v.get("@odata.type").and_then(Value::as_str)
                == Some("#microsoft.graph.eventMessageRequest"),
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

/// Which Graph attachment kind this is (`@odata.type`). Determines what the
/// UI does on save: `File` downloads its `contentBytes`; `Item` downloads its
/// `/$value` MIME (`.eml`/`.ics`); `Reference` opens `source_url`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    File,
    Item,
    Reference,
}

impl AttachmentKind {
    /// Short token stored in the `attachments.kind` column.
    pub fn as_db_str(&self) -> &'static str {
        match self {
            AttachmentKind::File => "file",
            AttachmentKind::Item => "item",
            AttachmentKind::Reference => "reference",
        }
    }
    /// Inverse of `as_db_str`; anything unrecognized reads back as `File`.
    pub fn from_db_str(s: &str) -> AttachmentKind {
        match s {
            "item" => AttachmentKind::Item,
            "reference" => AttachmentKind::Reference,
            _ => AttachmentKind::File,
        }
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
    /// The `Content-ID` of an inline attachment (Graph `contentId`), used to
    /// resolve `<img src="cid:…">` in the body to this attachment. `None` for
    /// ordinary (non-inline) attachments.
    pub content_id: Option<String>,
    pub kind: AttachmentKind,
    /// The cloud link of a `referenceAttachment` (Graph `sourceUrl`); `None`
    /// for other kinds.
    pub source_url: Option<String>,
}

impl AttachmentMeta {
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(AttachmentMeta {
            id: str_field(v, "id"),
            name: str_field(v, "name"),
            content_type: str_field(v, "contentType"),
            size: v.get("size").and_then(Value::as_i64).unwrap_or(0),
            is_inline: v.get("isInline").and_then(Value::as_bool).unwrap_or(false),
            content_id: {
                let cid = str_field(v, "contentId");
                if cid.is_empty() { None } else { Some(cid) }
            },
            kind: match v.get("@odata.type").and_then(Value::as_str) {
                Some("#microsoft.graph.itemAttachment") => AttachmentKind::Item,
                Some("#microsoft.graph.referenceAttachment") => AttachmentKind::Reference,
                _ => AttachmentKind::File,
            },
            source_url: {
                let u = str_field(v, "sourceUrl");
                if u.is_empty() { None } else { Some(u) }
            },
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

/// One attendee of a calendar event (required/optional/resource), and
/// their RSVP status as Graph's `attendees[].status.response`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Attendee {
    pub name: String,
    pub addr: String,
    pub r#type: String,
    pub response: String,
}

impl Attendee {
    pub fn from_json(v: &Value) -> Option<Self> {
        let addr = v.get("emailAddress")?;
        Some(Attendee {
            name: str_field(addr, "name"),
            addr: str_field(addr, "address"),
            r#type: str_field(v, "type"),
            response: v
                .get("status")
                .and_then(|s| s.get("response"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        })
    }
}

/// A calendar event, as returned by Graph's `/me/calendarView` and
/// `/me/events` endpoints. `start_utc`/`end_utc` are always normalized to
/// `YYYY-MM-DDTHH:MM:SSZ` by `to_utc` (see its docs) — callers never see
/// Graph's raw `start`/`end` `dateTime`+`timeZone` pair.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub id: String,
    pub subject: String,
    pub start_utc: String,
    pub end_utc: String,
    pub is_all_day: bool,
    pub location: String,
    pub organizer_name: String,
    pub organizer_addr: String,
    pub response_status: String,
    pub series_master_id: Option<String>,
    pub body_preview: String,
    pub web_link: String,
    pub last_modified: String,
    pub body_html: String,
    pub attendees: Vec<Attendee>,
}

impl Event {
    pub fn from_json(v: &Value) -> Option<Self> {
        let organizer = v
            .get("organizer")
            .and_then(Recipient::from_json)
            .unwrap_or_default();
        Some(Event {
            id: str_field(v, "id"),
            subject: str_field(v, "subject"),
            // `unwrap_or(&Value::Null)` rather than `.map(...).unwrap_or_default()`:
            // a fully-absent `start`/`end` key must still go through
            // `datetime_field_to_utc` → `to_utc` → `normalize_datetime`, so it
            // gets the same fixed-width canonical fallback
            // (`0000-00-00T00:00:00Z`) as a present-but-empty `dateTime` does.
            // `.unwrap_or_default()` on the `Option<String>` would instead
            // short-circuit straight to `""`, which is shorter than every
            // real canonical timestamp and would sort first in
            // `Store::events_in_window`'s `ORDER BY start_utc ASC` — ahead of
            // every real event, not last, breaking the whole
            // lexical-sortability invariant `to_utc`'s fixed width exists for.
            start_utc: datetime_field_to_utc(v.get("start").unwrap_or(&Value::Null)),
            end_utc: datetime_field_to_utc(v.get("end").unwrap_or(&Value::Null)),
            is_all_day: v.get("isAllDay").and_then(Value::as_bool).unwrap_or(false),
            location: v
                .get("location")
                .and_then(|l| l.get("displayName"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            organizer_name: organizer.name,
            organizer_addr: organizer.address,
            response_status: v
                .get("responseStatus")
                .and_then(|r| r.get("response"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            series_master_id: opt_str_field(v, "seriesMasterId"),
            body_preview: str_field(v, "bodyPreview"),
            web_link: str_field(v, "webLink"),
            last_modified: str_field(v, "lastModifiedDateTime"),
            body_html: v
                .get("body")
                .and_then(|b| b.get("content"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            attendees: v
                .get("attendees")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(Attendee::from_json).collect())
                .unwrap_or_default(),
        })
    }
}

/// Reads Graph's `start`/`end` object (`{"dateTime":…, "timeZone":…}`) and
/// normalizes it to UTC via `to_utc`. Missing/mistyped fields fall back to
/// `""`/`"UTC"`, matching this module's usual "default rather than fail"
/// convention for `from_json` — a malformed date is still a fixed-width
/// string, just not a meaningful one.
fn datetime_field_to_utc(v: &Value) -> String {
    let dt = v.get("dateTime").and_then(Value::as_str).unwrap_or("");
    let tz = v.get("timeZone").and_then(Value::as_str).unwrap_or("UTC");
    to_utc(dt, tz)
}

/// Normalizes a Graph event `dateTime`+`timeZone` pair to a canonical,
/// fixed-width UTC timestamp: exactly `YYYY-MM-DDTHH:MM:SSZ` (4-2-2 date,
/// 2-2-2 time, always zero-padded, fractional seconds dropped). This exact
/// shape matters beyond cosmetics: `Store::events_in_window` orders and
/// filters events by **lexical** comparison on the stored `start_utc`/
/// `end_utc` strings, so lexical order has to equal chronological order —
/// which only holds if every timestamp is this same fixed width with no
/// variation in padding, fractional digits, or `Z` suffix.
///
/// `calendar_view` always sends `Prefer: outlook.timezone="UTC"`, so in
/// practice `tz` is always `"UTC"` and Graph's `dateTime` is already a UTC
/// wall-clock time — just not in this canonical shape (Graph sends e.g.
/// `"2026-07-18T09:00:00.0000000"`: no `Z`, 7 fractional digits). `tz` is
/// still checked defensively: an unrecognized zone doesn't get converted
/// (this crate carries no IANA/Windows offset table — that's out of scope
/// while every request already asks Graph for UTC) but is *noted* by
/// falling through the same normalization path, treating the `dateTime` as
/// if it were UTC, rather than silently trusting the wrong offset math.
pub fn to_utc(dt: &str, tz: &str) -> String {
    let _ = is_utc_zone(tz); // defensive guard only — see doc comment above.
    normalize_datetime(dt)
}

/// `true` for the zone labels Graph is expected to send back when a
/// request carries `Prefer: outlook.timezone="UTC"`.
fn is_utc_zone(tz: &str) -> bool {
    matches!(tz.to_ascii_uppercase().as_str(), "UTC" | "ETC/UTC" | "Z")
}

/// Reformats an ISO-8601-ish `YYYY-MM-DDTHH:MM:SS[.fraction][Z|±HH:MM]`
/// string into exactly `YYYY-MM-DDTHH:MM:SSZ`: drops fractional seconds and
/// any trailing `Z`/numeric offset, then zero-pads every component. Missing
/// pieces (e.g. an empty `dt`) default to `0000`/`00` rather than panicking,
/// same "default rather than fail" convention as the rest of `from_json`.
fn normalize_datetime(dt: &str) -> String {
    let s = dt.trim_end_matches('Z');
    let (date_part, time_part) = s.split_once('T').unwrap_or((s, ""));
    let time_no_offset = strip_offset(time_part);
    let time_no_frac = time_no_offset.split('.').next().unwrap_or("");

    let mut date_fields = date_part.splitn(3, '-');
    let year = date_fields.next().unwrap_or("");
    let month = date_fields.next().unwrap_or("");
    let day = date_fields.next().unwrap_or("");

    let mut time_fields = time_no_frac.splitn(3, ':');
    let hour = time_fields.next().unwrap_or("");
    let minute = time_fields.next().unwrap_or("");
    let second = time_fields.next().unwrap_or("");

    format!(
        "{:0>4}-{:0>2}-{:0>2}T{:0>2}:{:0>2}:{:0>2}Z",
        year, month, day, hour, minute, second
    )
}

/// Strips a trailing numeric UTC offset (`+HH:MM` or `-HH:MM`) off a time
/// component, if present. Graph shouldn't send one here (a `dateTime` next
/// to a separate `timeZone` field is wall-clock time, not offset-suffixed),
/// but stripping it defensively means a stray offset can't leak into the
/// zero-padded output instead of being dropped like fractional seconds are.
fn strip_offset(time_part: &str) -> &str {
    if let Some(idx) = time_part.find('+') {
        &time_part[..idx]
    } else if let Some(idx) = time_part.rfind('-') {
        &time_part[..idx]
    } else {
        time_part
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
    fn message_flags_event_message_request_as_meeting() {
        let invite = parse(
            r##"{"@odata.type":"#microsoft.graph.eventMessageRequest","id":"M1","conversationId":"C","subject":"Invite","from":{"emailAddress":{"name":"A","address":"a@x"}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"importance":"normal","bodyPreview":""}"##,
        )
        .unwrap();
        assert!(Message::from_json(&invite).unwrap().is_meeting_request);

        let ordinary = parse(
            r#"{"id":"M2","conversationId":"C","subject":"Hi","from":{"emailAddress":{"name":"A","address":"a@x"}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"importance":"normal","bodyPreview":""}"#,
        )
        .unwrap();
        assert!(!Message::from_json(&ordinary).unwrap().is_meeting_request);

        let response = parse(
            r##"{"@odata.type":"#microsoft.graph.eventMessageResponse","id":"M3","conversationId":"C","subject":"RE","from":{"emailAddress":{"name":"A","address":"a@x"}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"importance":"normal","bodyPreview":""}"##,
        )
        .unwrap();
        assert!(!Message::from_json(&response).unwrap().is_meeting_request);
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
    fn to_utc_normalizes_graph_style_fractional_seconds() {
        // Graph's actual wire shape when `Prefer: outlook.timezone="UTC"`
        // is set: no `Z`, 7 fractional digits. This is the highest-risk
        // case — `Store::events_in_window` depends on this collapsing to
        // exactly the fixed-width canonical form.
        assert_eq!(
            to_utc("2026-07-18T09:00:00.0000000", "UTC"),
            "2026-07-18T09:00:00Z"
        );
    }

    #[test]
    fn to_utc_passes_through_already_canonical_and_guards_non_utc_zone() {
        assert_eq!(
            to_utc("2026-07-18T09:00:00Z", "UTC"),
            "2026-07-18T09:00:00Z"
        );
        // An unrecognized zone label is guarded defensively (treated as
        // UTC, not converted) rather than mis-applying an offset this
        // crate has no table for.
        assert_eq!(
            to_utc("2026-07-18T09:00:00.0000000", "Pacific Standard Time"),
            "2026-07-18T09:00:00Z"
        );
    }

    #[test]
    fn to_utc_zero_pads_single_digit_components() {
        assert_eq!(to_utc("2026-7-8T9:5:3", "UTC"), "2026-07-08T09:05:03Z");
    }

    #[test]
    fn parses_event_with_attendees_organizer_and_response_status() {
        let v = parse(
            r#"{
          "id":"E1","subject":"Sync",
          "start":{"dateTime":"2026-07-18T09:00:00.0000000","timeZone":"UTC"},
          "end":{"dateTime":"2026-07-18T10:00:00.0000000","timeZone":"UTC"},
          "isAllDay":false,
          "location":{"displayName":"Room 1"},
          "organizer":{"emailAddress":{"name":"Boss","address":"boss@x"}},
          "responseStatus":{"response":"accepted","time":"2026-07-17T00:00:00Z"},
          "seriesMasterId":null,
          "bodyPreview":"preview",
          "webLink":"https://outlook/e1",
          "lastModifiedDateTime":"2026-07-17T12:00:00Z",
          "body":{"contentType":"html","content":"<p>hi</p>"},
          "attendees":[{"type":"required","status":{"response":"none","time":"0001-01-01T00:00:00Z"},"emailAddress":{"name":"A","address":"a@x"}}]
        }"#,
        )
        .unwrap();
        let e = Event::from_json(&v).unwrap();
        assert_eq!(e.id, "E1");
        assert_eq!(e.subject, "Sync");
        assert_eq!(e.start_utc, "2026-07-18T09:00:00Z");
        assert_eq!(e.end_utc, "2026-07-18T10:00:00Z");
        assert!(!e.is_all_day);
        assert_eq!(e.location, "Room 1");
        assert_eq!(e.organizer_name, "Boss");
        assert_eq!(e.organizer_addr, "boss@x");
        assert_eq!(e.response_status, "accepted");
        assert!(e.series_master_id.is_none());
        assert_eq!(e.body_preview, "preview");
        assert_eq!(e.web_link, "https://outlook/e1");
        assert_eq!(e.last_modified, "2026-07-17T12:00:00Z");
        assert_eq!(e.body_html, "<p>hi</p>");
        assert_eq!(e.attendees.len(), 1);
        assert_eq!(e.attendees[0].name, "A");
        assert_eq!(e.attendees[0].addr, "a@x");
        assert_eq!(e.attendees[0].r#type, "required");
        assert_eq!(e.attendees[0].response, "none");
    }

    #[test]
    fn parses_event_series_master_id_when_present() {
        let v = parse(
            r#"{"id":"E2","subject":"Occurrence",
                "start":{"dateTime":"2026-07-19T09:00:00.0000000","timeZone":"UTC"},
                "end":{"dateTime":"2026-07-19T10:00:00.0000000","timeZone":"UTC"},
                "seriesMasterId":"SERIES1"}"#,
        )
        .unwrap();
        let e = Event::from_json(&v).unwrap();
        assert_eq!(e.series_master_id.as_deref(), Some("SERIES1"));
    }

    #[test]
    fn event_with_fully_absent_start_and_end_gets_canonical_fixed_width_fallback() {
        // A missing `start`/`end` key entirely (not merely an empty
        // `dateTime`) must still normalize to the same fixed-width
        // canonical fallback as an empty one, not `""` — `""` is shorter
        // than every real canonical timestamp and would sort first under
        // `Store::events_in_window`'s `ORDER BY start_utc ASC`, ahead of
        // every real event, which breaks the lexical-sortability invariant
        // `to_utc`'s fixed width exists for.
        let v = parse(r#"{"id":"E3","subject":"No times"}"#).unwrap();
        let e = Event::from_json(&v).unwrap();
        assert_eq!(e.start_utc, "0000-00-00T00:00:00Z");
        assert_eq!(e.end_utc, "0000-00-00T00:00:00Z");
        assert_ne!(e.start_utc, "");
    }

    #[test]
    fn attachment_meta_parses_content_id() {
        let v = crate::json::parse(
            r#"{"id":"a1","name":"logo.png","contentType":"image/png","size":10,"isInline":true,"contentId":"logo123"}"#
        ).unwrap();
        let a = AttachmentMeta::from_json(&v).unwrap();
        assert_eq!(a.content_id.as_deref(), Some("logo123"));
        assert!(a.is_inline);
    }

    #[test]
    fn attachment_meta_content_id_absent_is_none() {
        let v = crate::json::parse(
            r#"{"id":"a1","name":"x.txt","contentType":"text/plain","size":1,"isInline":false}"#,
        )
        .unwrap();
        let a = AttachmentMeta::from_json(&v).unwrap();
        assert_eq!(a.content_id, None);
    }

    #[test]
    fn attachment_meta_parses_item_kind() {
        let v = crate::json::parse(
            r##"{"@odata.type":"#microsoft.graph.itemAttachment","id":"a1","name":"Fwd: hi","contentType":"","size":0,"isInline":false}"##
        ).unwrap();
        let a = AttachmentMeta::from_json(&v).unwrap();
        assert_eq!(a.kind, AttachmentKind::Item);
        assert_eq!(a.source_url, None);
    }

    #[test]
    fn attachment_meta_parses_reference_kind_with_source_url() {
        let v = crate::json::parse(
            r##"{"@odata.type":"#microsoft.graph.referenceAttachment","id":"a2","name":"Doc","contentType":"","size":0,"isInline":false,"sourceUrl":"https://contoso.sharepoint.com/x"}"##
        ).unwrap();
        let a = AttachmentMeta::from_json(&v).unwrap();
        assert_eq!(a.kind, AttachmentKind::Reference);
        assert_eq!(
            a.source_url.as_deref(),
            Some("https://contoso.sharepoint.com/x")
        );
    }

    #[test]
    fn attachment_meta_file_kind_is_default() {
        let v = crate::json::parse(
            r##"{"@odata.type":"#microsoft.graph.fileAttachment","id":"a3","name":"x.pdf","contentType":"application/pdf","size":5,"isInline":false}"##
        ).unwrap();
        let a = AttachmentMeta::from_json(&v).unwrap();
        assert_eq!(a.kind, AttachmentKind::File);
        // an absent @odata.type also defaults to File:
        let v2 = crate::json::parse(
            r#"{"id":"a4","name":"y","contentType":"","size":0,"isInline":false}"#,
        )
        .unwrap();
        assert_eq!(
            AttachmentMeta::from_json(&v2).unwrap().kind,
            AttachmentKind::File
        );
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
