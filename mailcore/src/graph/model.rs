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

/// The whole folder tree, as `GraphClient::list_folders` enumerated it
/// (#1886). `complete` is false when part of it could not be listed (a
/// `childFolders` call that failed, a listed folder without an id): the
/// folders that were listed are still here, but a folder missing from
/// them may still exist, so the sync engine removes nothing after it.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderListing {
    pub folders: Vec<MailFolder>,
    pub complete: bool,
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
    /// Graph `categories`: the message's assigned category names (color labels).
    /// Colors live separately in the master category list (`MasterCategory`).
    pub categories: Vec<String>,
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
            categories: v
                .get("categories")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

/// Which of a message's stored properties a Graph JSON object actually
/// carried, by key (a key holding JSON `null` counts as carried: Graph said
/// the value is empty). `Message::from_json` turns an absent key into an
/// empty value, which is right for a complete item but not for a partial
/// one: a delta round can return only the properties that changed (`{id,
/// isRead}`) or a bare `{id}` for an item created and changed between two
/// rounds (#1899). `Store::merge_message` writes only the columns whose
/// property is carried, so an absent property never blanks a stored one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MessageFields {
    pub conversation_id: bool,
    pub subject: bool,
    pub from: bool,
    pub to: bool,
    pub cc: bool,
    pub received: bool,
    pub sent: bool,
    pub is_read: bool,
    pub flag: bool,
    pub has_attachments: bool,
    pub importance: bool,
    pub preview: bool,
    pub is_draft: bool,
    /// `@odata.type`, from which `is_meeting_request` is read.
    pub odata_type: bool,
    pub categories: bool,
}

impl MessageFields {
    /// The keys `v` carries, by `Message::from_json`'s key names.
    pub fn of(v: &Value) -> Self {
        let has = |k: &str| v.get(k).is_some();
        MessageFields {
            conversation_id: has("conversationId"),
            subject: has("subject"),
            from: has("from"),
            to: has("toRecipients"),
            cc: has("ccRecipients"),
            received: has("receivedDateTime"),
            sent: has("sentDateTime"),
            is_read: has("isRead"),
            flag: has("flag"),
            has_attachments: has("hasAttachments"),
            importance: has("importance"),
            preview: has("bodyPreview"),
            is_draft: has("isDraft"),
            odata_type: has("@odata.type"),
            categories: has("categories"),
        }
    }

    /// Every property carried: what a merge of a `Message` built in code
    /// (not parsed) means.
    pub fn all() -> Self {
        MessageFields {
            conversation_id: true,
            subject: true,
            from: true,
            to: true,
            cc: true,
            received: true,
            sent: true,
            is_read: true,
            flag: true,
            has_attachments: true,
            importance: true,
            preview: true,
            is_draft: true,
            odata_type: true,
            categories: true,
        }
    }

    /// A partial item: one of the header keys (`receivedDateTime`,
    /// `subject`, `from`) is absent. Every message Graph has carries them
    /// when `MESSAGE_SELECT` asks for them, so their absence marks a
    /// partial representation.
    pub fn is_partial(&self) -> bool {
        !(self.received && self.subject && self.from)
    }

    /// Whether `is_meeting_request` is to be written. `@odata.type` is an
    /// annotation Graph emits only for a derived type, so a complete plain
    /// message has none and that means "not a meeting request", as
    /// `upsert_message` writes it. On a partial item its absence says
    /// nothing.
    pub fn writes_meeting_request(&self) -> bool {
        self.odata_type || !self.is_partial()
    }

    /// Whether `is_draft` is to be written. `isDraft` is not in
    /// `MESSAGE_SELECT`, so a complete delta item writes 0 as
    /// `upsert_message` does (unchanged behaviour); a partial item keeps
    /// the stored value unless it carries the key.
    pub fn writes_is_draft(&self) -> bool {
        self.is_draft || !self.is_partial()
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

/// Which body format the sync engine asks Graph for on a `FetchBody`
/// (#1901). `Text` sends `Prefer: outlook.body-content-type="text"`, so
/// every body comes back as plain text (lookxy, a terminal client). `Html`
/// sends no `Prefer`, so each body comes back in its *native* format: html
/// for an html message, but still text for a plain-text one. That is why
/// the store records the format a body was asked for (`bodies.fetched_as`),
/// not just the content type it came back as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BodyFormat {
    #[default]
    Text,
    Html,
}

impl BodyFormat {
    /// The value stored in `bodies.fetched_as`.
    pub fn as_str(self) -> &'static str {
        match self {
            BodyFormat::Text => "text",
            BodyFormat::Html => "html",
        }
    }

    /// The inverse of [`BodyFormat::as_str`]; `None` for anything else.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "text" => Some(BodyFormat::Text),
            "html" => Some(BodyFormat::Html),
            _ => None,
        }
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

/// Graph `automaticRepliesSetting.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OofStatus {
    Disabled,
    AlwaysEnabled,
    Scheduled,
}

impl OofStatus {
    pub fn as_wire(&self) -> &'static str {
        match self {
            OofStatus::Disabled => "disabled",
            OofStatus::AlwaysEnabled => "alwaysEnabled",
            OofStatus::Scheduled => "scheduled",
        }
    }
    /// Inverse of `as_wire`; an unrecognized value reads back as `Disabled`
    /// (the safe "auto-replies are off" default).
    pub fn from_wire(s: &str) -> OofStatus {
        match s {
            "alwaysEnabled" => OofStatus::AlwaysEnabled,
            "scheduled" => OofStatus::Scheduled,
            _ => OofStatus::Disabled,
        }
    }
}

/// Graph `automaticRepliesSetting.externalAudience`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalAudience {
    None,
    ContactsOnly,
    All,
}

impl ExternalAudience {
    pub fn as_wire(&self) -> &'static str {
        match self {
            ExternalAudience::None => "none",
            ExternalAudience::ContactsOnly => "contactsOnly",
            ExternalAudience::All => "all",
        }
    }
    /// Inverse of `as_wire`; unrecognized reads back as `All` (Graph's own
    /// default external audience).
    pub fn from_wire(s: &str) -> ExternalAudience {
        match s {
            "none" => ExternalAudience::None,
            "contactsOnly" => ExternalAudience::ContactsOnly,
            _ => ExternalAudience::All,
        }
    }
}

/// The mailbox's automatic-replies (out-of-office) configuration, parsed from
/// Graph's `mailboxSettings.automaticRepliesSetting`. Reply messages are held
/// as plain text (`html_to_plain` on read; `plain_to_html` on write — see
/// `graph::client::set_automatic_replies`). `scheduled_*_utc` are canonical
/// UTC only when `status == Scheduled`, else `""`.
#[derive(Debug, Clone, PartialEq)]
pub struct AutomaticReplies {
    pub status: OofStatus,
    pub external_audience: ExternalAudience,
    pub internal_message: String,
    pub external_message: String,
    pub scheduled_start_utc: String,
    pub scheduled_end_utc: String,
}

impl AutomaticReplies {
    pub fn from_json(v: &Value) -> Option<Self> {
        let s = v.get("automaticRepliesSetting")?;
        let status = OofStatus::from_wire(&str_field(s, "status"));
        // Graph always echoes the scheduled datetimes (with `0001-01-01`
        // defaults when off); only keep them when actually Scheduled so the
        // form doesn't prefill a garbage window for a disabled mailbox.
        let (start, end) = if status == OofStatus::Scheduled {
            (
                s.get("scheduledStartDateTime")
                    .map(datetime_field_to_utc)
                    .unwrap_or_default(),
                s.get("scheduledEndDateTime")
                    .map(datetime_field_to_utc)
                    .unwrap_or_default(),
            )
        } else {
            (String::new(), String::new())
        };
        Some(AutomaticReplies {
            status,
            external_audience: ExternalAudience::from_wire(&str_field(s, "externalAudience")),
            internal_message: html_to_plain(&str_field(s, "internalReplyMessage")),
            external_message: html_to_plain(&str_field(s, "externalReplyMessage")),
            scheduled_start_utc: start,
            scheduled_end_utc: end,
        })
    }
}

/// The signed-in user's profile (`GET /me?$select=displayName,mail,userPrincipalName`,
/// `GraphClient::me`). `mail` is `""` when Graph sends `null` or leaves it
/// out, as it does for personal accounts and some tenants; [`Me::address`]
/// then falls back to the user principal name.
#[derive(Debug, Clone, PartialEq)]
pub struct Me {
    pub display_name: String,
    pub mail: String,
    pub user_principal_name: String,
}

impl Me {
    /// Reads a `/me` response; a missing or non-string field is `""`.
    pub fn from_json(v: &Value) -> Me {
        Me {
            display_name: str_field(v, "displayName"),
            mail: str_field(v, "mail"),
            user_principal_name: str_field(v, "userPrincipalName"),
        }
    }

    /// The address to show: `mail` when set, else `userPrincipalName`.
    pub fn address(&self) -> &str {
        if self.mail.trim().is_empty() {
            &self.user_principal_name
        } else {
            &self.mail
        }
    }
}

/// Best-effort conversion of an OOF HTML reply message to plain text: `<br>`,
/// `<p>`/`</p>`, and `<div>`/`</div>` become newlines; every other tag is
/// dropped; the common entities are decoded; runs of 3+ newlines collapse to
/// 2; both ends are trimmed. Rich formatting (tables, styling) is flattened to
/// its text content — see the design's fidelity note.
pub fn html_to_plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let after = &rest[lt + 1..];
        let Some(gt) = after.find('>') else {
            // Unclosed '<': keep the rest verbatim and stop tag-scanning.
            out.push_str(&rest[lt..]);
            rest = "";
            break;
        };
        let name = after[..gt]
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(name.as_str(), "br" | "p" | "div") {
            out.push('\n');
        }
        rest = &after[gt + 1..];
    }
    out.push_str(rest);

    // Decode entities — `&amp;` LAST so `&amp;lt;` doesn't double-decode to `<`.
    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");

    // Collapse 3+ consecutive newlines to 2.
    let mut collapsed = String::with_capacity(decoded.len());
    let mut nl = 0;
    for ch in decoded.chars() {
        if ch == '\n' {
            nl += 1;
            if nl <= 2 {
                collapsed.push('\n');
            }
        } else {
            nl = 0;
            collapsed.push(ch);
        }
    }
    collapsed.trim().to_string()
}

/// Inverse of `html_to_plain` for writing an OOF message: HTML-escape
/// `& < > "` and turn `\n` into `<br>` (dropping any `\r`). A message authored
/// in lookxy round-trips faithfully through this pair.
pub fn plain_to_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("<br>"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// One schedule's availability from Graph `getSchedule`: the mailbox address
/// (`scheduleId`) and its `availabilityView` digit string (one char per slot:
/// `0` free, `1` tentative, `2` busy, `3` out-of-office, `4` working-elsewhere).
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleEntry {
    pub email: String,
    pub availability: String,
}

impl ScheduleEntry {
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(ScheduleEntry {
            email: str_field(v, "scheduleId"),
            availability: str_field(v, "availabilityView"),
        })
    }
}

/// One entry of the mailbox's master category list (Graph `outlookCategory`):
/// a category's display name and its `color` (`"preset0"`…`"preset24"` or
/// `"none"`). The UI maps `color` to a terminal color; the name is what a
/// message's `categories` list references.
#[derive(Debug, Clone, PartialEq)]
pub struct MasterCategory {
    pub display_name: String,
    pub color: String,
}

impl MasterCategory {
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(MasterCategory {
            display_name: str_field(v, "displayName"),
            color: str_field(v, "color"),
        })
    }
}

/// One entry of a delta sync page: either an upserted message or the id of
/// a message that was removed since the last sync.
#[derive(Debug, Clone, PartialEq)]
pub enum DeltaItem {
    /// A message and which of its properties the item carried: a delta
    /// item may be partial (#1899), see `MessageFields`.
    Upsert(Message, MessageFields),
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
                    Message::from_json(item).map(|m| DeltaItem::Upsert(m, MessageFields::of(item)))
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
    /// Graph `attendees[].status.time`: when the attendee last responded,
    /// as Graph sends it. `""` when absent or Graph's "never"
    /// (`0001-01-01T00:00:00Z`; see `graph_date_or_empty`).
    pub response_time: String,
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
            response_time: graph_date_or_empty(
                v.get("status")
                    .and_then(|s| s.get("time"))
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            ),
        })
    }
}

/// Graph writes "no date" as the minimum date, `0001-01-01` (a `noEnd`
/// recurrence range's `endDate`, an attendee who never responded's
/// `status.time` `0001-01-01T00:00:00Z`). Any value starting `0001-01-01`
/// reads as `""`; every other value is kept as Graph sent it.
fn graph_date_or_empty(s: &str) -> String {
    if s.starts_with("0001-01-01") {
        String::new()
    } else {
        s.to_string()
    }
}

/// The recurrence pattern kind lookxy can create — a subset of Graph's
/// `recurrencePattern.type` (`absoluteMonthly` for `Monthly`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceKind {
    Daily,
    Weekly,
    Monthly,
}

impl RecurrenceKind {
    fn as_wire(&self) -> &'static str {
        match self {
            RecurrenceKind::Daily => "daily",
            RecurrenceKind::Weekly => "weekly",
            RecurrenceKind::Monthly => "absoluteMonthly",
        }
    }
    fn from_wire(s: &str) -> Option<RecurrenceKind> {
        match s {
            "daily" => Some(RecurrenceKind::Daily),
            "weekly" => Some(RecurrenceKind::Weekly),
            "absoluteMonthly" => Some(RecurrenceKind::Monthly),
            _ => None,
        }
    }
}

/// A recurring event's pattern + range, as lookxy creates it. Serializes to
/// Graph's `event.recurrence` (`to_json`); `from_json` round-trips it for the
/// store (see `Store::event_for_send`).
#[derive(Debug, Clone, PartialEq)]
pub struct Recurrence {
    pub kind: RecurrenceKind,
    pub interval: u32,
    pub days_of_week: Vec<String>, // "monday".."sunday" (weekly)
    pub day_of_month: u32,         // absoluteMonthly
    pub start_date: String,        // "YYYY-MM-DD" (range.startDate)
    pub until: Option<String>,     // "YYYY-MM-DD" (range endDate), None = noEnd
}

impl Recurrence {
    pub fn to_json(&self) -> Value {
        let mut pattern = vec![
            (
                "type".to_string(),
                Value::Str(self.kind.as_wire().to_string()),
            ),
            ("interval".to_string(), Value::Num(self.interval as f64)),
        ];
        if self.kind == RecurrenceKind::Weekly {
            pattern.push((
                "daysOfWeek".to_string(),
                Value::Array(
                    self.days_of_week
                        .iter()
                        .map(|d| Value::Str(d.clone()))
                        .collect(),
                ),
            ));
            pattern.push((
                "firstDayOfWeek".to_string(),
                Value::Str("sunday".to_string()),
            ));
        }
        if self.kind == RecurrenceKind::Monthly {
            pattern.push((
                "dayOfMonth".to_string(),
                Value::Num(self.day_of_month as f64),
            ));
        }
        let mut range = vec![
            (
                "type".to_string(),
                Value::Str(
                    if self.until.is_some() {
                        "endDate"
                    } else {
                        "noEnd"
                    }
                    .to_string(),
                ),
            ),
            ("startDate".to_string(), Value::Str(self.start_date.clone())),
        ];
        if let Some(end) = &self.until {
            range.push(("endDate".to_string(), Value::Str(end.clone())));
        }
        Value::Object(vec![
            ("pattern".to_string(), Value::Object(pattern)),
            ("range".to_string(), Value::Object(range)),
        ])
    }

    pub fn from_json(v: &Value) -> Option<Self> {
        let pattern = v.get("pattern")?;
        let range = v.get("range")?;
        let kind = RecurrenceKind::from_wire(pattern.get("type")?.as_str()?)?;
        let interval = pattern.get("interval").and_then(Value::as_i64).unwrap_or(1) as u32;
        let days_of_week = pattern
            .get("daysOfWeek")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let day_of_month = pattern
            .get("dayOfMonth")
            .and_then(Value::as_i64)
            .unwrap_or(0) as u32;
        let start_date = str_field(range, "startDate");
        let until = range
            .get("endDate")
            .and_then(Value::as_str)
            .map(str::to_string);
        Some(Recurrence {
            kind,
            interval,
            days_of_week,
            day_of_month,
            start_date,
            until,
        })
    }
}

/// A series' recurrence as Graph reports it (`event.recurrence`), read side
/// only (#1983). Lenient: every enum Graph uses is kept as its raw string,
/// so a value this crate does not know is kept, not dropped. Not the
/// create-side `Recurrence`: that one is what `Store::event_for_send` sends
/// back to Graph on every create and update, and its three kinds and
/// `to_json` (a `numbered` range becomes `noEnd`, `firstDayOfWeek` is forced
/// to `sunday`) would rewrite a synced series. The store keeps this one in
/// its own column, `events.graph_recurrence`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EventRecurrence {
    pub pattern: RecurrencePattern,
    pub range: RecurrenceRange,
}

/// Graph's `recurrencePattern`: `type` is `daily`, `weekly`,
/// `absoluteMonthly`, `relativeMonthly`, `absoluteYearly` or
/// `relativeYearly`; `index` is `first` .. `last`. Graph fills every field on
/// every type (`month: 0`, `index: "first"` where unused), and they are kept.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RecurrencePattern {
    pub r#type: String,
    pub interval: u32,
    pub month: u32,
    pub day_of_month: u32,
    pub days_of_week: Vec<String>,
    pub first_day_of_week: String,
    pub index: String,
}

/// Graph's `recurrenceRange`: `type` is `endDate`, `noEnd` or `numbered`.
/// Dates are `YYYY-MM-DD`; Graph's "no date" (`0001-01-01`, a `noEnd`
/// range's `endDate`) reads as `""` (`graph_date_or_empty`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RecurrenceRange {
    pub r#type: String,
    pub start_date: String,
    pub end_date: String,
    pub number_of_occurrences: u32,
    pub recurrence_time_zone: String,
}

impl EventRecurrence {
    /// `None` unless `v` is an object whose `pattern` is an object (Graph
    /// sends `"recurrence": null` for every non-series event). A missing or
    /// malformed `range` reads as an empty one.
    pub fn from_json(v: &Value) -> Option<Self> {
        let p = v.get("pattern").filter(|p| matches!(p, Value::Object(_)))?;
        let pattern = RecurrencePattern {
            r#type: str_field(p, "type"),
            interval: u32_field(p, "interval"),
            month: u32_field(p, "month"),
            day_of_month: u32_field(p, "dayOfMonth"),
            days_of_week: string_list(p, "daysOfWeek"),
            first_day_of_week: str_field(p, "firstDayOfWeek"),
            index: str_field(p, "index"),
        };
        let range = match v.get("range") {
            Some(r @ Value::Object(_)) => RecurrenceRange {
                r#type: str_field(r, "type"),
                start_date: graph_date_or_empty(&str_field(r, "startDate")),
                end_date: graph_date_or_empty(&str_field(r, "endDate")),
                number_of_occurrences: u32_field(r, "numberOfOccurrences"),
                recurrence_time_zone: str_field(r, "recurrenceTimeZone"),
            },
            _ => RecurrenceRange::default(),
        };
        Some(EventRecurrence { pattern, range })
    }

    /// Graph's `recurrence` shape, every field written (the store's
    /// `events.graph_recurrence` text; `from_json` reads it back equal).
    pub fn to_json(&self) -> Value {
        let s = |v: &str| Value::Str(v.to_string());
        let n = |v: u32| Value::Num(v as f64);
        let p = &self.pattern;
        let r = &self.range;
        let pattern = Value::Object(vec![
            ("type".to_string(), s(&p.r#type)),
            ("interval".to_string(), n(p.interval)),
            ("month".to_string(), n(p.month)),
            ("dayOfMonth".to_string(), n(p.day_of_month)),
            (
                "daysOfWeek".to_string(),
                Value::Array(p.days_of_week.iter().map(|d| s(d)).collect()),
            ),
            ("firstDayOfWeek".to_string(), s(&p.first_day_of_week)),
            ("index".to_string(), s(&p.index)),
        ]);
        let range = Value::Object(vec![
            ("type".to_string(), s(&r.r#type)),
            ("startDate".to_string(), s(&r.start_date)),
            ("endDate".to_string(), s(&r.end_date)),
            (
                "numberOfOccurrences".to_string(),
                n(r.number_of_occurrences),
            ),
            ("recurrenceTimeZone".to_string(), s(&r.recurrence_time_zone)),
        ]);
        Value::Object(vec![
            ("pattern".to_string(), pattern),
            ("range".to_string(), range),
        ])
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
    /// Graph `reminderMinutesBeforeStart`: minutes before `start` the reminder
    /// fires. `is_reminder_on` gates whether a reminder exists at all.
    pub reminder_minutes: i64,
    pub is_reminder_on: bool,
    /// Graph `showAs` as sent: `free`, `tentative`, `busy`, `oof`,
    /// `workingElsewhere` or `unknown` (`""` when absent).
    pub show_as: String,
    pub is_online_meeting: bool,
    /// Graph `onlineMeetingProvider` (`teamsForBusiness`, `skypeForBusiness`,
    /// `skypeForConsumer`, `unknown`).
    pub online_meeting_provider: String,
    /// The link to join the meeting: `onlineMeeting.joinUrl`, else
    /// `onlineMeetingUrl`, else a meeting link found in the location, else
    /// one found in the body (`meeting_link`); `""` when there is none.
    pub join_url: String,
    /// Assigned category names; colours are the master categories'.
    pub categories: Vec<String>,
    /// Graph `sensitivity`: `normal`, `personal`, `private`, `confidential`.
    pub sensitivity: String,
    /// Graph `importance`: `low`, `normal`, `high`.
    pub importance: String,
    /// Graph `type`: `singleInstance`, `occurrence`, `exception`,
    /// `seriesMaster`.
    pub event_type: String,
    /// A series master's pattern and range; `None` for every other event
    /// (`calendarView` returns occurrences, whose `recurrence` is null).
    pub recurrence: Option<EventRecurrence>,
    pub is_cancelled: bool,
    pub is_organizer: bool,
    /// `true` when absent: Graph's default.
    pub response_requested: bool,
    /// `true` when absent: Graph's default.
    pub allow_new_time_proposals: bool,
    pub has_attachments: bool,
    /// Graph `originalStartTimeZone` / `originalEndTimeZone`: the zone the
    /// organizer set the times in (to show "in another time zone").
    pub original_start_time_zone: String,
    pub original_end_time_zone: String,
    /// The calendar holding the event (`calendar.id`, when Graph expands
    /// it); `""` is the default calendar (#1984 fills it per calendar).
    pub calendar_id: String,
}

/// Every field empty, zero or `false`, except `response_requested` and
/// `allow_new_time_proposals`, which are `true` as in Graph (and in
/// `from_json` for a missing key, and in the store's `DEFAULT 1`).
impl Default for Event {
    fn default() -> Self {
        Event {
            id: String::new(),
            subject: String::new(),
            start_utc: String::new(),
            end_utc: String::new(),
            is_all_day: false,
            location: String::new(),
            organizer_name: String::new(),
            organizer_addr: String::new(),
            response_status: String::new(),
            series_master_id: None,
            body_preview: String::new(),
            web_link: String::new(),
            last_modified: String::new(),
            body_html: String::new(),
            attendees: Vec::new(),
            reminder_minutes: 0,
            is_reminder_on: false,
            show_as: String::new(),
            is_online_meeting: false,
            online_meeting_provider: String::new(),
            join_url: String::new(),
            categories: Vec::new(),
            sensitivity: String::new(),
            importance: String::new(),
            event_type: String::new(),
            recurrence: None,
            is_cancelled: false,
            is_organizer: false,
            response_requested: true,
            allow_new_time_proposals: true,
            has_attachments: false,
            original_start_time_zone: String::new(),
            original_end_time_zone: String::new(),
            calendar_id: String::new(),
        }
    }
}

impl Event {
    pub fn from_json(v: &Value) -> Option<Self> {
        let organizer = v
            .get("organizer")
            .and_then(Recipient::from_json)
            .unwrap_or_default();
        let location = v
            .get("location")
            .and_then(|l| l.get("displayName"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let body_html = v
            .get("body")
            .and_then(|b| b.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let join_url = v
            .get("onlineMeeting")
            .and_then(|m| m.get("joinUrl"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|u| !u.is_empty())
            .or_else(|| opt_str_field(v, "onlineMeetingUrl").filter(|u| !u.is_empty()))
            .or_else(|| meeting_link(&location))
            .or_else(|| meeting_link(&body_html))
            .unwrap_or_default();
        let flag = |key: &str, absent: bool| v.get(key).and_then(Value::as_bool).unwrap_or(absent);
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
            location,
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
            body_html,
            attendees: v
                .get("attendees")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(Attendee::from_json).collect())
                .unwrap_or_default(),
            reminder_minutes: v
                .get("reminderMinutesBeforeStart")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            is_reminder_on: flag("isReminderOn", false),
            show_as: str_field(v, "showAs"),
            is_online_meeting: flag("isOnlineMeeting", false),
            online_meeting_provider: str_field(v, "onlineMeetingProvider"),
            join_url,
            categories: string_list(v, "categories"),
            sensitivity: str_field(v, "sensitivity"),
            importance: str_field(v, "importance"),
            event_type: str_field(v, "type"),
            recurrence: v.get("recurrence").and_then(EventRecurrence::from_json),
            is_cancelled: flag("isCancelled", false),
            is_organizer: flag("isOrganizer", false),
            response_requested: flag("responseRequested", true),
            allow_new_time_proposals: flag("allowNewTimeProposals", true),
            has_attachments: flag("hasAttachments", false),
            original_start_time_zone: str_field(v, "originalStartTimeZone"),
            original_end_time_zone: str_field(v, "originalEndTimeZone"),
            calendar_id: v
                .get("calendar")
                .map(|c| str_field(c, "id"))
                .unwrap_or_default(),
        })
    }
}

/// The first link in `text` (an event's location or HTML/text body) that
/// joins a meeting on a known service, or `None`. A link starts at
/// `https://` (any case) and ends at a quote, `<`, `>`, whitespace or the
/// end; `&amp;` is decoded, then trailing prose punctuation (`.`, `,`, `;`,
/// `:`, `)`, `]`, `!`) is trimmed. Only the parsed host decides the service
/// (`is_meeting_url`), so a meeting host in another link's query, or a
/// lookalike host, is not taken. Safe Links-wrapped links
/// (`*.safelinks.protection.outlook.com/?url=...`) are not unwrapped.
fn meeting_link(text: &str) -> Option<String> {
    const SCHEME: &str = "https://";
    // ASCII lowercasing keeps every byte offset, so offsets in `lower`
    // index `text` too (and land on the ASCII `h`, a char boundary).
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(SCHEME) {
        let start = from + pos;
        let rest = &text[start..];
        let end = rest
            .find(|c: char| matches!(c, '"' | '\'' | '<' | '>') || c.is_whitespace())
            .unwrap_or(rest.len());
        let decoded = rest[..end].replace("&amp;", "&");
        let url = decoded.trim_end_matches(['.', ',', ';', ':', ')', ']', '!']);
        if is_meeting_url(&url[SCHEME.len()..]) {
            return Some(url.to_string());
        }
        from = start + SCHEME.len();
    }
    None
}

/// `after_scheme` (a link without its `https://`) is a meeting link: its
/// host (up to the first `/`, `?`, `#` or `:`, compared lowercased) and
/// path (as is) match a known meeting service. `base` or `*.base` hosts
/// match a `base` entry, so `notzoom.us` is not `zoom.us`.
fn is_meeting_url(after_scheme: &str) -> bool {
    let host_end = after_scheme
        .find(['/', '?', '#', ':'])
        .unwrap_or(after_scheme.len());
    let host = after_scheme[..host_end].to_ascii_lowercase();
    let mut path = &after_scheme[host_end..];
    if let Some(port) = path.strip_prefix(':') {
        path = &port[port.find(['/', '?', '#']).unwrap_or(port.len())..];
    }
    if host.is_empty() || host.contains('@') {
        return false;
    }
    let under = |base: &str| {
        host == base
            || host
                .strip_suffix(base)
                .is_some_and(|sub| sub.ends_with('.'))
    };
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| path.starts_with(p));
    let has_path = path.len() > 1;
    if host == "teams.microsoft.com" {
        starts(&["/l/meetup-join/", "/meet/"])
    } else if host == "teams.live.com" {
        starts(&["/meet/"])
    } else if under("zoom.us") || under("zoomgov.com") {
        starts(&["/j/", "/w/", "/my/"])
    } else if host == "meet.google.com" || host == "meet.goto.com" || host == "chime.aws" {
        has_path
    } else if under("webex.com") {
        has_path
    } else if host == "global.gotomeeting.com" {
        starts(&["/join/"])
    } else {
        false
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

/// Reads a non-negative integer field as `u32`, `0` when absent, not a
/// number or negative.
fn u32_field(v: &Value, key: &str) -> u32 {
    v.get(key)
        .and_then(Value::as_i64)
        .map(|n| n.clamp(0, u32::MAX as i64) as u32)
        .unwrap_or(0)
}

/// Reads an array of strings, skipping non-string items; empty when absent.
fn string_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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
    fn event_parses_reminder_fields() {
        let v = parse(
            r#"{"id":"E1","subject":"Sync",
                "start":{"dateTime":"2026-07-20T09:00:00.0000000","timeZone":"UTC"},
                "end":{"dateTime":"2026-07-20T10:00:00.0000000","timeZone":"UTC"},
                "isReminderOn":true,"reminderMinutesBeforeStart":15}"#,
        )
        .unwrap();
        let e = Event::from_json(&v).unwrap();
        assert_eq!(e.reminder_minutes, 15);
        assert!(e.is_reminder_on);

        let v2 = parse(
            r#"{"id":"E2","subject":"x",
                "start":{"dateTime":"2026-07-20T09:00:00.0000000","timeZone":"UTC"},
                "end":{"dateTime":"2026-07-20T10:00:00.0000000","timeZone":"UTC"}}"#,
        )
        .unwrap();
        let e2 = Event::from_json(&v2).unwrap();
        assert_eq!(e2.reminder_minutes, 0);
        assert!(!e2.is_reminder_on);
    }

    #[test]
    fn recurrence_weekly_to_json_round_trips() {
        let r = Recurrence {
            kind: RecurrenceKind::Weekly,
            interval: 2,
            days_of_week: vec!["monday".into(), "wednesday".into()],
            day_of_month: 0,
            start_date: "2026-07-20".into(),
            until: Some("2026-12-31".into()),
        };
        let v = r.to_json();
        let pat = v.get("pattern").unwrap();
        assert_eq!(pat.get("type").and_then(Value::as_str), Some("weekly"));
        assert_eq!(pat.get("interval").and_then(Value::as_i64), Some(2));
        assert_eq!(
            pat.get("daysOfWeek")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            pat.get("firstDayOfWeek").and_then(Value::as_str),
            Some("sunday")
        );
        let range = v.get("range").unwrap();
        assert_eq!(range.get("type").and_then(Value::as_str), Some("endDate"));
        assert_eq!(
            range.get("startDate").and_then(Value::as_str),
            Some("2026-07-20")
        );
        assert_eq!(
            range.get("endDate").and_then(Value::as_str),
            Some("2026-12-31")
        );
        assert_eq!(Recurrence::from_json(&v).unwrap(), r); // round-trip
    }

    #[test]
    fn recurrence_daily_and_monthly_shapes() {
        let daily = Recurrence {
            kind: RecurrenceKind::Daily,
            interval: 1,
            days_of_week: vec![],
            day_of_month: 0,
            start_date: "2026-07-20".into(),
            until: None,
        };
        let v = daily.to_json();
        assert_eq!(
            v.get("pattern")
                .unwrap()
                .get("type")
                .and_then(Value::as_str),
            Some("daily")
        );
        assert!(v.get("pattern").unwrap().get("daysOfWeek").is_none());
        assert_eq!(
            v.get("range").unwrap().get("type").and_then(Value::as_str),
            Some("noEnd")
        );
        assert!(v.get("range").unwrap().get("endDate").is_none());
        assert_eq!(Recurrence::from_json(&v).unwrap(), daily);

        let monthly = Recurrence {
            kind: RecurrenceKind::Monthly,
            interval: 1,
            days_of_week: vec![],
            day_of_month: 15,
            start_date: "2026-07-15".into(),
            until: None,
        };
        let v = monthly.to_json();
        assert_eq!(
            v.get("pattern")
                .unwrap()
                .get("type")
                .and_then(Value::as_str),
            Some("absoluteMonthly")
        );
        assert_eq!(
            v.get("pattern")
                .unwrap()
                .get("dayOfMonth")
                .and_then(Value::as_i64),
            Some(15)
        );
        assert_eq!(Recurrence::from_json(&v).unwrap(), monthly);
    }

    #[test]
    fn recurrence_from_json_rejects_unknown_type() {
        let v = crate::json::parse(
            r#"{"pattern":{"type":"yearly","interval":1},"range":{"type":"noEnd","startDate":"2026-01-01"}}"#,
        )
        .unwrap();
        assert!(Recurrence::from_json(&v).is_none());
    }

    #[test]
    fn schedule_entry_parses_id_and_view() {
        let v = parse(r#"{"scheduleId":"alice@x","availabilityView":"002200"}"#).unwrap();
        let e = ScheduleEntry::from_json(&v).unwrap();
        assert_eq!(e.email, "alice@x");
        assert_eq!(e.availability, "002200");
    }

    #[test]
    fn master_category_parses_name_and_color() {
        let v = parse(r#"{"id":"c1","displayName":"Work","color":"preset0"}"#).unwrap();
        let c = MasterCategory::from_json(&v).unwrap();
        assert_eq!(c.display_name, "Work");
        assert_eq!(c.color, "preset0");
    }

    #[test]
    fn message_parses_categories() {
        let with = parse(
            r#"{"id":"M1","conversationId":"C","subject":"s","from":{"emailAddress":{"name":"","address":""}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"importance":"normal","bodyPreview":"","categories":["Work","Urgent"]}"#,
        )
        .unwrap();
        assert_eq!(
            Message::from_json(&with).unwrap().categories,
            vec!["Work".to_string(), "Urgent".to_string()]
        );
        let without = parse(
            r#"{"id":"M2","conversationId":"C","subject":"s","from":{"emailAddress":{"name":"","address":""}},"toRecipients":[],"ccRecipients":[],"receivedDateTime":"","sentDateTime":"","isRead":false,"importance":"normal","bodyPreview":""}"#,
        )
        .unwrap();
        assert!(Message::from_json(&without).unwrap().categories.is_empty());
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
    fn message_fields_track_present_keys() {
        // A key holding `null` is carried (Graph said "empty"); an absent
        // key is not.
        let v = parse(r#"{"id":"M1","isRead":true,"from":null,"categories":[]}"#).unwrap();
        let f = MessageFields::of(&v);
        assert!(f.is_read && f.from && f.categories);
        assert!(!f.subject && !f.received && !f.preview && !f.flag && !f.odata_type);
        assert!(f.is_partial(), "no subject and no receivedDateTime");
        assert!(!f.writes_meeting_request() && !f.writes_is_draft());

        let complete = parse(
            r#"{"id":"M2","subject":"","from":null,"receivedDateTime":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let f = MessageFields::of(&complete);
        assert!(!f.is_partial());
        // A complete plain message writes `is_meeting_request` and
        // `is_draft` (as 0), as `upsert_message` does.
        assert!(f.writes_meeting_request() && f.writes_is_draft());

        for key in ["receivedDateTime", "subject", "from"] {
            let mut keys = vec![r#""id":"M3""#.to_string()];
            for k in ["receivedDateTime", "subject", "from"] {
                if k != key {
                    keys.push(format!(r#""{k}":null"#));
                }
            }
            let v = parse(&format!("{{{}}}", keys.join(","))).unwrap();
            assert!(MessageFields::of(&v).is_partial(), "{key} absent");
        }

        let invite = parse(
            r##"{"@odata.type":"#microsoft.graph.eventMessageRequest","id":"M4","isDraft":false}"##,
        )
        .unwrap();
        let f = MessageFields::of(&invite);
        assert!(f.is_partial() && f.writes_meeting_request() && f.writes_is_draft());
        assert!(!MessageFields::all().is_partial());
    }

    #[test]
    fn delta_page_marks_partial_items() {
        let v = parse(r#"{
          "value":[
            {"id":"M1","subject":"a","from":{"emailAddress":{"name":"","address":""}},"receivedDateTime":"2026-01-01T00:00:00Z","sentDateTime":"","isRead":true,"conversationId":"","importance":"normal","bodyPreview":""},
            {"id":"M2","isRead":true},
            {"id":"M3"}
          ]
        }"#).unwrap();
        let page = DeltaPage::from_json(&v).unwrap();
        let partial: Vec<(&str, bool)> = page
            .items
            .iter()
            .map(|i| match i {
                DeltaItem::Upsert(m, f) => (m.id.as_str(), f.is_partial()),
                DeltaItem::Delete(_) => panic!("no removals here"),
            })
            .collect();
        assert_eq!(partial, vec![("M1", false), ("M2", true), ("M3", true)]);
        let DeltaItem::Upsert(m2, f2) = &page.items[1] else {
            unreachable!()
        };
        assert!(m2.is_read && f2.is_read && !f2.subject);
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
    fn oof_status_and_audience_wire_round_trip() {
        for s in [
            OofStatus::Disabled,
            OofStatus::AlwaysEnabled,
            OofStatus::Scheduled,
        ] {
            assert_eq!(OofStatus::from_wire(s.as_wire()), s);
        }
        assert_eq!(OofStatus::from_wire("bogus"), OofStatus::Disabled);
        for a in [
            ExternalAudience::None,
            ExternalAudience::ContactsOnly,
            ExternalAudience::All,
        ] {
            assert_eq!(ExternalAudience::from_wire(a.as_wire()), a);
        }
        assert_eq!(ExternalAudience::from_wire("bogus"), ExternalAudience::All);
    }

    #[test]
    fn automatic_replies_parses_scheduled_setting() {
        let v = parse(
            r#"{"automaticRepliesSetting":{
                "status":"scheduled","externalAudience":"contactsOnly",
                "internalReplyMessage":"<p>Away &amp; back <b>Monday</b></p>",
                "externalReplyMessage":"Out<br>of office",
                "scheduledStartDateTime":{"dateTime":"2026-07-20T09:00:00.0000000","timeZone":"UTC"},
                "scheduledEndDateTime":{"dateTime":"2026-07-27T17:00:00.0000000","timeZone":"UTC"}
            }}"#,
        )
        .unwrap();
        let r = AutomaticReplies::from_json(&v).unwrap();
        assert_eq!(r.status, OofStatus::Scheduled);
        assert_eq!(r.external_audience, ExternalAudience::ContactsOnly);
        assert_eq!(r.internal_message, "Away & back Monday");
        assert_eq!(r.external_message, "Out\nof office");
        assert_eq!(r.scheduled_start_utc, "2026-07-20T09:00:00Z");
        assert_eq!(r.scheduled_end_utc, "2026-07-27T17:00:00Z");
    }

    #[test]
    fn automatic_replies_disabled_drops_schedule_even_when_wire_has_defaults() {
        let v = parse(
            r#"{"automaticRepliesSetting":{
                "status":"disabled","externalAudience":"all",
                "internalReplyMessage":"","externalReplyMessage":"",
                "scheduledStartDateTime":{"dateTime":"0001-01-01T00:00:00.0000000","timeZone":"UTC"},
                "scheduledEndDateTime":{"dateTime":"0001-01-01T00:00:00.0000000","timeZone":"UTC"}
            }}"#,
        )
        .unwrap();
        let r = AutomaticReplies::from_json(&v).unwrap();
        assert_eq!(r.status, OofStatus::Disabled);
        assert_eq!(r.scheduled_start_utc, ""); // dropped: only kept when Scheduled
        assert_eq!(r.scheduled_end_utc, "");
    }

    #[test]
    fn html_to_plain_strips_tags_decodes_entities_and_breaks() {
        // Adjacent block boundaries (`</div>` then `<p>`) each emit a newline,
        // so the two blocks are separated by a blank line — best-effort spacing.
        assert_eq!(
            html_to_plain("<div>Hi &amp; bye</div><p>line1</p>line2<br>line3"),
            "Hi & bye\n\nline1\nline2\nline3"
        );
        assert_eq!(html_to_plain("a<br/><br/><br/><br/>b"), "a\n\nb"); // 3+ newlines collapse to 2
        assert_eq!(
            html_to_plain("&lt;tag&gt; &quot;q&quot; &#39;s&#39;"),
            "<tag> \"q\" 's'"
        );
    }

    #[test]
    fn plain_to_html_escapes_and_encodes_newlines() {
        assert_eq!(
            plain_to_html("a & b < c > d \"e\"\nf"),
            "a &amp; b &lt; c &gt; d &quot;e&quot;<br>f"
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

    #[test]
    fn me_from_json_prefers_mail() {
        let v = parse(
            r#"{"displayName":"Ada Lovelace","mail":"ada@contoso.com","userPrincipalName":"ada@contoso.onmicrosoft.com"}"#,
        )
        .unwrap();
        let me = Me::from_json(&v);
        assert_eq!(me.display_name, "Ada Lovelace");
        assert_eq!(me.mail, "ada@contoso.com");
        assert_eq!(me.user_principal_name, "ada@contoso.onmicrosoft.com");
        assert_eq!(me.address(), "ada@contoso.com");
    }

    #[test]
    fn me_address_falls_back_to_upn() {
        for body in [
            r#"{"displayName":"Ada","mail":null,"userPrincipalName":"ada@outlook.com"}"#,
            r#"{"displayName":"Ada","userPrincipalName":"ada@outlook.com"}"#,
            r#"{"displayName":"Ada","mail":"","userPrincipalName":"ada@outlook.com"}"#,
        ] {
            let me = Me::from_json(&parse(body).unwrap());
            assert_eq!(me.mail, "", "{body}");
            assert_eq!(me.address(), "ada@outlook.com", "{body}");
        }
    }

    /// An event body around `extra`, the times every #1983 test shares.
    fn ev(extra: &str) -> Event {
        let json = format!(
            r#"{{"id":"E1","subject":"s",
                "start":{{"dateTime":"2026-07-20T09:00:00.0000000","timeZone":"UTC"}},
                "end":{{"dateTime":"2026-07-20T10:00:00.0000000","timeZone":"UTC"}}{extra}}}"#
        );
        Event::from_json(&parse(&json).unwrap()).unwrap()
    }

    #[test]
    fn event_reads_calendar_client_fields() {
        let e = ev(r#","showAs":"workingElsewhere","isOnlineMeeting":true,
            "onlineMeetingProvider":"teamsForBusiness",
            "onlineMeeting":{"joinUrl":"https://teams.microsoft.com/l/meetup-join/19%3a1"},
            "categories":["Red category","Plan, phase; 2"],
            "sensitivity":"private","importance":"high","type":"exception",
            "isCancelled":true,"isOrganizer":true,"responseRequested":false,
            "allowNewTimeProposals":false,"hasAttachments":true,
            "originalStartTimeZone":"Pacific Standard Time",
            "originalEndTimeZone":"Eastern Standard Time",
            "calendar":{"id":"CAL1"},
            "attendees":[{"type":"resource","status":{"response":"accepted",
                "time":"2026-07-18T08:01:02Z"},
                "emailAddress":{"name":"Room","address":"room@x"}}]"#);
        assert_eq!(e.show_as, "workingElsewhere");
        assert!(e.is_online_meeting);
        assert_eq!(e.online_meeting_provider, "teamsForBusiness");
        assert_eq!(
            e.join_url,
            "https://teams.microsoft.com/l/meetup-join/19%3a1"
        );
        assert_eq!(e.categories, vec!["Red category", "Plan, phase; 2"]);
        assert_eq!(e.sensitivity, "private");
        assert_eq!(e.importance, "high");
        assert_eq!(e.event_type, "exception");
        assert!(e.is_cancelled);
        assert!(e.is_organizer);
        assert!(!e.response_requested);
        assert!(!e.allow_new_time_proposals);
        assert!(e.has_attachments);
        assert_eq!(e.original_start_time_zone, "Pacific Standard Time");
        assert_eq!(e.original_end_time_zone, "Eastern Standard Time");
        assert_eq!(e.calendar_id, "CAL1");
        assert_eq!(e.attendees[0].r#type, "resource");
        assert_eq!(e.attendees[0].response, "accepted");
        assert_eq!(e.attendees[0].response_time, "2026-07-18T08:01:02Z");
    }

    #[test]
    fn event_defaults_when_fields_absent() {
        let e = ev("");
        assert_eq!(e.show_as, "");
        assert!(!e.is_online_meeting);
        assert_eq!(e.online_meeting_provider, "");
        assert_eq!(e.join_url, "");
        assert!(e.categories.is_empty());
        assert_eq!(e.sensitivity, "");
        assert_eq!(e.importance, "");
        assert_eq!(e.event_type, "");
        assert_eq!(e.recurrence, None);
        assert!(!e.is_cancelled);
        assert!(!e.is_organizer);
        assert!(e.response_requested, "Graph's default");
        assert!(e.allow_new_time_proposals, "Graph's default");
        assert!(!e.has_attachments);
        assert_eq!(e.original_start_time_zone, "");
        assert_eq!(e.original_end_time_zone, "");
        assert_eq!(e.calendar_id, "");
        assert_eq!(ev(r#","recurrence":null"#).recurrence, None);
    }

    #[test]
    fn default_event_agrees_with_graph_on_request_flags() {
        let d = Event::default();
        assert!(d.response_requested);
        assert!(d.allow_new_time_proposals);
        assert!(!d.is_organizer && !d.is_cancelled && !d.is_online_meeting);
    }

    #[test]
    fn event_join_url_sources() {
        let zoom_loc = r#","location":{"displayName":"Zoom: https://us02web.zoom.us/j/111"}"#;
        let body = r#","body":{"contentType":"html","content":"<a href=\"https://meet.google.com/abc-defg-hij\">Join</a>"}"#;
        // 1. onlineMeeting.joinUrl wins over everything.
        let all = format!(
            r#","onlineMeeting":{{"joinUrl":"https://teams.microsoft.com/l/meetup-join/A"}},
               "onlineMeetingUrl":"https://teams.microsoft.com/l/meetup-join/B"{zoom_loc}{body}"#
        );
        assert_eq!(
            ev(&all).join_url,
            "https://teams.microsoft.com/l/meetup-join/A"
        );
        // 2. onlineMeetingUrl, with onlineMeeting null.
        let no_om = format!(
            r#","onlineMeeting":null,
               "onlineMeetingUrl":"https://teams.microsoft.com/l/meetup-join/B"{zoom_loc}{body}"#
        );
        assert_eq!(
            ev(&no_om).join_url,
            "https://teams.microsoft.com/l/meetup-join/B"
        );
        // 3. the location, before the body; onlineMeeting absent.
        assert_eq!(
            ev(&format!("{zoom_loc}{body}")).join_url,
            "https://us02web.zoom.us/j/111"
        );
        // 4. the body.
        assert_eq!(ev(body).join_url, "https://meet.google.com/abc-defg-hij");
        // An empty joinUrl falls through.
        assert_eq!(
            ev(&format!(r#","onlineMeeting":{{"joinUrl":""}}{body}"#)).join_url,
            "https://meet.google.com/abc-defg-hij"
        );
        // Teams' short /meet/ link and zoomgov.com are meeting links.
        assert_eq!(
            ev(r#","location":{"displayName":"https://teams.microsoft.com/meet/2468?p=Xy"}"#)
                .join_url,
            "https://teams.microsoft.com/meet/2468?p=Xy"
        );
        assert_eq!(
            ev(r#","location":{"displayName":"https://agency.zoomgov.com/j/16"}"#).join_url,
            "https://agency.zoomgov.com/j/16"
        );
    }

    #[test]
    fn event_join_url_found_only_in_body() {
        let e = ev(
            r#","isOnlineMeeting":false,"location":{"displayName":"Room 4"},
            "body":{"contentType":"html","content":"<p>See <a href=\"https://example.com/agenda\">agenda</a>.</p><a href=\"https://teams.microsoft.com/l/meetup-join/19%3ameeting_X%40thread.v2/0?context=%7b%22Tid%22%7d&amp;btype=a&amp;role=a\">Join the meeting now</a>"}"#,
        );
        assert_eq!(
            e.join_url,
            "https://teams.microsoft.com/l/meetup-join/19%3ameeting_X%40thread.v2/0?context=%7b%22Tid%22%7d&btype=a&role=a"
        );
    }

    #[test]
    fn event_join_url_ignores_non_meeting_links() {
        let e = ev(r#","location":{"displayName":"https://maps.example/room"},
            "body":{"content":"<a href=\"https://example.com/x\">x</a> https://zoom.us/pricing https://www.webex.com/"}"#);
        assert_eq!(e.join_url, "");
    }

    #[test]
    fn event_join_url_trims_prose_punctuation() {
        let e = ev(
            r#","body":{"contentType":"text","content":"Join: https://zoom.us/j/123?pwd=a1. Thanks"}"#,
        );
        assert_eq!(e.join_url, "https://zoom.us/j/123?pwd=a1");
        let e = ev(r#","body":{"content":"(https://meet.google.com/abc-defg-hij), later"}"#);
        assert_eq!(e.join_url, "https://meet.google.com/abc-defg-hij");
    }

    #[test]
    fn event_join_url_rejects_lookalike_hosts() {
        for text in [
            "https://notzoom.us/j/1",
            "https://evil.example/?u=teams.microsoft.com/l/meetup-join/1",
            "https://evil.example/teams.microsoft.com/l/meetup-join/1",
            "https://teams.microsoft.com@evil.example/l/meetup-join/1",
            "https://zoom.us.evil.example/j/1",
            "http://zoom.us/j/1",
        ] {
            let e = ev(&format!(r#","location":{{"displayName":"{text}"}}"#));
            assert_eq!(e.join_url, "", "{text}");
        }
        // A real link after a rejected one is still found.
        let e = ev(r#","location":{"displayName":"https://notzoom.us/j/1 https://zoom.us/j/2"}"#);
        assert_eq!(e.join_url, "https://zoom.us/j/2");
    }

    #[test]
    fn event_join_url_scheme_and_host_case_insensitive() {
        let e =
            ev(r#","location":{"displayName":"HTTPS://Teams.Microsoft.COM/l/meetup-join/AbC"}"#);
        assert_eq!(e.join_url, "HTTPS://Teams.Microsoft.COM/l/meetup-join/AbC");
        // The path is matched as is.
        let e = ev(r#","location":{"displayName":"https://teams.microsoft.com/L/MEETUP-JOIN/x"}"#);
        assert_eq!(e.join_url, "");
    }

    #[test]
    fn event_recurrence_reads_every_pattern_and_range() {
        // A full Graph payload: every field present, unused ones at Graph's
        // fillers, and a noEnd range's 0001-01-01 end date.
        let e = ev(r#","type":"seriesMaster","recurrence":{
            "pattern":{"type":"relativeMonthly","interval":2,"month":0,"dayOfMonth":0,
                "daysOfWeek":["tuesday"],"firstDayOfWeek":"sunday","index":"second"},
            "range":{"type":"noEnd","startDate":"2026-07-14","endDate":"0001-01-01",
                "recurrenceTimeZone":"W. Europe Standard Time","numberOfOccurrences":0}}"#);
        let r = e.recurrence.clone().unwrap();
        assert_eq!(r.pattern.r#type, "relativeMonthly");
        assert_eq!(r.pattern.interval, 2);
        assert_eq!(r.pattern.days_of_week, vec!["tuesday"]);
        assert_eq!(r.pattern.first_day_of_week, "sunday");
        assert_eq!(r.pattern.index, "second");
        assert_eq!(r.range.r#type, "noEnd");
        assert_eq!(r.range.start_date, "2026-07-14");
        assert_eq!(r.range.end_date, "", "Graph's never date");
        assert_eq!(r.range.recurrence_time_zone, "W. Europe Standard Time");
        assert_eq!(EventRecurrence::from_json(&r.to_json()), Some(r));

        let yearly = ev(r#","recurrence":{
            "pattern":{"type":"absoluteYearly","interval":1,"month":3,"dayOfMonth":17,
                "daysOfWeek":[],"firstDayOfWeek":"monday","index":"first"},
            "range":{"type":"numbered","startDate":"2026-03-17","endDate":"0001-01-01",
                "recurrenceTimeZone":"","numberOfOccurrences":5}}"#)
        .recurrence
        .unwrap();
        assert_eq!(yearly.pattern.month, 3);
        assert_eq!(yearly.pattern.day_of_month, 17);
        assert_eq!(yearly.pattern.first_day_of_week, "monday");
        assert_eq!(yearly.range.r#type, "numbered");
        assert_eq!(yearly.range.number_of_occurrences, 5);
        assert_eq!(EventRecurrence::from_json(&yearly.to_json()), Some(yearly));

        let dated = ev(r#","recurrence":{"pattern":{"type":"daily","interval":1},
            "range":{"type":"endDate","startDate":"2026-07-01","endDate":"2026-07-31"}}"#)
        .recurrence
        .unwrap();
        assert_eq!(dated.range.end_date, "2026-07-31");
        assert_eq!(EventRecurrence::from_json(&dated.to_json()), Some(dated));

        // No pattern object: no recurrence.
        assert_eq!(ev(r#","recurrence":{"pattern":null}"#).recurrence, None);
    }

    #[test]
    fn attendee_response_time_and_never() {
        let e = ev(r#","attendees":[
            {"type":"required","status":{"response":"none","time":"0001-01-01T00:00:00Z"},
             "emailAddress":{"name":"A","address":"a@x"}},
            {"type":"optional","status":{"response":"declined"},
             "emailAddress":{"name":"B","address":"b@x"}}]"#);
        assert_eq!(e.attendees[0].r#type, "required");
        assert_eq!(e.attendees[0].response_time, "");
        assert_eq!(e.attendees[1].r#type, "optional");
        assert_eq!(e.attendees[1].response, "declined");
        assert_eq!(e.attendees[1].response_time, "");
    }
}
