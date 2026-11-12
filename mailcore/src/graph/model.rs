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

/// What Graph has under a folder a delete is about to take (#2107,
/// `GraphClient::folder_subtree_items`): every descendant folder's id, and
/// every message in the folder or a descendant, as `(id,
/// internetMessageId)` (`''` when Graph gives none).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubtreeItems {
    pub folders: Vec<String>,
    pub messages: Vec<(String, String)>,
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

/// One user of the organisation's directory, from Graph `/users?$search`
/// (`GraphClient::search_users`, #1942): what a To/Cc/Bcc completion shows.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectoryUser {
    /// `displayName`, trimmed; `""` when the directory has none.
    pub name: String,
    /// `mail`, or the `userPrincipalName` when there is no `mail` and the
    /// UPN looks like an address. A user without `mail` may have no mailbox
    /// (unlicensed, a service account), so a client may rank a UPN-only
    /// user below one with `mail`.
    pub address: String,
    /// `jobTitle`; `None` when absent, not a string or blank.
    pub job_title: Option<String>,
    /// `department`; `None` when absent, not a string or blank.
    pub department: Option<String>,
}

impl DirectoryUser {
    /// A directory user, or `None` when it has no address to complete: no
    /// (or a blank) `mail` and a `userPrincipalName` that does not look like
    /// an address ([`looks_like_address`]: a guest's `#EXT#` UPN does not).
    /// A UPN that does is kept, though its user may have no mailbox.
    pub fn from_json(v: &Value) -> Option<Self> {
        let address = match non_blank_str(v, "mail") {
            Some(mail) => mail,
            None => non_blank_str(v, "userPrincipalName").filter(|u| looks_like_address(u))?,
        };
        Some(DirectoryUser {
            name: non_blank_str(v, "displayName").unwrap_or_default(),
            address,
            job_title: non_blank_str(v, "jobTitle"),
            department: non_blank_str(v, "department"),
        })
    }
}

/// Whether a directory `userPrincipalName` can stand in for a missing `mail`
/// (#1942): exactly one `@` with a non-empty local part, a domain holding a
/// `.` that neither starts nor ends it, no whitespace or control character,
/// and not a guest's `#EXT#` UPN (`alice_contoso.com#EXT#@tenant…`), which
/// is no mailbox.
fn looks_like_address(s: &str) -> bool {
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    if s.to_ascii_uppercase().contains("#EXT#") {
        return false;
    }
    let mut parts = s.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// A postal address of a contact, as Graph's `physicalAddress` (#2063).
/// Every field is `""` when absent.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PhysicalAddress {
    pub street: String,
    pub city: String,
    pub state: String,
    pub postal_code: String,
    /// Graph's `countryOrRegion`.
    pub country_or_region: String,
}

impl PhysicalAddress {
    fn from_json(v: Option<&Value>) -> Self {
        let Some(v) = v else {
            return PhysicalAddress::default();
        };
        PhysicalAddress {
            street: str_field(v, "street"),
            city: str_field(v, "city"),
            state: str_field(v, "state"),
            postal_code: str_field(v, "postalCode"),
            country_or_region: str_field(v, "countryOrRegion"),
        }
    }

    /// Every field empty: Graph has no such address.
    pub fn is_empty(&self) -> bool {
        *self == PhysicalAddress::default()
    }
}

/// The fields of a contact Outlook's contact form edits (#2063), as the
/// store keeps them and the create / update calls send them. A string is
/// `""` and a list empty when Graph has none.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ContactFields {
    pub given_name: String,
    pub middle_name: String,
    pub surname: String,
    pub display_name: String,
    pub file_as: String,
    /// Graph's `emailAddresses` (up to three), each `{name, address}`.
    pub email_addresses: Vec<Recipient>,
    pub business_phones: Vec<String>,
    pub home_phones: Vec<String>,
    pub mobile_phone: String,
    pub company_name: String,
    pub job_title: String,
    pub department: String,
    pub business_address: PhysicalAddress,
    pub home_address: PhysicalAddress,
    /// `YYYY-MM-DD`, `""` for none. Graph's `birthday` is a DateTimeOffset:
    /// its date part is kept, and a write sends noon UTC of the day.
    pub birthday: String,
    pub personal_notes: String,
    pub categories: Vec<String>,
}

/// One contact of the user's default Contacts folder (`/me/contacts`,
/// #1934, #2063), keyed by Graph `id` (or a `local:` id before its create
/// reached Graph). What `Store::is_contact` answers from, apart from
/// `/me/people` and mined correspondents (`store::Contact`, the
/// autocomplete row, is a different thing).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FolderContact {
    pub id: String,
    /// Graph's `changeKey`. Stored, not used for concurrency: a write sends
    /// no `If-Match`, so the last writer wins.
    pub change_key: String,
    pub fields: ContactFields,
}

impl FolderContact {
    /// A contact as `/me/contacts` returns it; `None` without an `id`.
    /// Missing and null fields read as empty.
    pub fn from_json(v: &Value) -> Option<Self> {
        let id = str_field(v, "id");
        if id.is_empty() {
            return None;
        }
        let email_addresses = v
            .get("emailAddresses")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|e| Recipient {
                        name: str_field(e, "name"),
                        address: str_field(e, "address"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(FolderContact {
            id,
            change_key: str_field(v, "changeKey"),
            fields: ContactFields {
                given_name: str_field(v, "givenName"),
                middle_name: str_field(v, "middleName"),
                surname: str_field(v, "surname"),
                display_name: str_field(v, "displayName"),
                file_as: str_field(v, "fileAs"),
                email_addresses,
                business_phones: string_list(v, "businessPhones"),
                home_phones: string_list(v, "homePhones"),
                mobile_phone: str_field(v, "mobilePhone"),
                company_name: str_field(v, "companyName"),
                job_title: str_field(v, "jobTitle"),
                department: str_field(v, "department"),
                business_address: PhysicalAddress::from_json(v.get("businessAddress")),
                home_address: PhysicalAddress::from_json(v.get("homeAddress")),
                birthday: birthday_date(&str_field(v, "birthday")),
                personal_notes: str_field(v, "personalNotes"),
                categories: string_list(v, "categories"),
            },
        })
    }
}

/// The `YYYY-MM-DD` date part of Graph's `birthday` DateTimeOffset
/// (`1980-05-17T11:59:00Z`), `""` when it does not start with one.
fn birthday_date(s: &str) -> String {
    let d = s.get(..10).unwrap_or("");
    let b = d.as_bytes();
    let is_date = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    if is_date && !d.starts_with("0001-01-01") {
        d.to_string()
    } else {
        String::new()
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
    /// Graph's `@odata.type == "#microsoft.graph.eventMessageRequest"`: this
    /// message is a meeting invite the user can RSVP to (see the reader's
    /// meeting banner and `SyncCommand::RespondMeeting`). `@odata.type` is an
    /// OData control annotation auto-emitted for derived resource types, so it
    /// arrives with the normal delta response — no `$select` change needed.
    pub is_meeting_request: bool,
    /// Graph `categories`: the message's assigned category names (color labels).
    /// Colors live separately in the master category list (`MasterCategory`).
    pub categories: Vec<String>,
    /// Outlook's reminder on a flagged message is on (#2054):
    /// `PidLidReminderSet` true and at least one of its times readable. A
    /// reminder with no time cannot fire, so it reads as off.
    pub is_reminder_on: bool,
    /// `PidLidReminderTime`, the time the reminder was set for, as
    /// `YYYY-MM-DDTHH:MM:SSZ`; `""` when absent or unreadable.
    pub reminder_at: String,
    /// `PidLidReminderSignalTime`, when it fires next (Outlook moves it on
    /// Snooze), as `YYYY-MM-DDTHH:MM:SSZ`; `""` when absent or unreadable.
    pub reminder_signal_at: String,
    /// `flag.dueDateTime` in UTC (`YYYY-MM-DDTHH:MM:SSZ`); `""` when the
    /// flag has none, or a zone other than UTC (no zone database here).
    pub flag_due_at: String,
    /// What meeting message this is (#2081): a request, a cancellation or
    /// an attendee's response, `None` for any other message. Read from a
    /// top-level `meetingMessageType` when the item carries one (a plain GET
    /// of an `eventMessage`), else from its `PidTagMessageClass`
    /// ([`MESSAGE_CLASS_PROP`], which the delta's `$expand` brings): see
    /// [`parse_meeting_message_type`]. Lenient: an absent, unknown or
    /// `none` value is `None`.
    pub meeting_message_type: Option<MeetingMessageType>,
    /// The message's protection (#2088): its Sensitivity, its Microsoft
    /// Purview sensitivity label and whether it is rights-protected, read
    /// from its `singleValueExtendedProperties` (the delta's `$expand`
    /// brings them): see [`parse_message_protection`]. Lenient: absent or
    /// unreadable values are the default, unprotected.
    pub protection: MessageProtection,
    /// A meeting message's own reminder is set (#2089): the item is a
    /// meeting message and its `PidLidReminderSet` is true, read from its
    /// `singleValueExtendedProperties` (the delta's `$expand` brings it):
    /// see [`parse_meeting_reminder`]. What a list shows as classic
    /// Outlook's bell beside the meeting icon. Independent of the flag and
    /// of [`Message::is_reminder_on`], and it never fires a reminder.
    /// False for any other message.
    pub meeting_reminder_on: bool,
    /// Graph's `internetMessageId` (#2096): the message's RFC 5322
    /// `Message-ID`, `<…>` included, as Graph sends it. What a reply's
    /// In-Reply-To names. `""` when absent or not a string.
    pub internet_message_id: String,
    /// The message's In-Reply-To (#2096): `PidTagInReplyToId`
    /// ([`IN_REPLY_TO_PROP`], which the delta's `$expand` brings), the
    /// `internet_message_id` of the message a reply answers, as Graph sends
    /// it (not trimmed): see [`parse_in_reply_to`]. `""` when absent or not
    /// a string, and on anything that is no reply.
    pub in_reply_to: String,
    /// The message's size in bytes (#2123): `PidTagMessageSize`
    /// ([`MESSAGE_SIZE_PROP`], which the delta's `$expand` brings): see
    /// [`parse_message_size`]. `None` when absent or unreadable; a stored
    /// size is never cleared by a merge (`Store::merge_message`).
    pub size: Option<i64>,
}

impl Message {
    pub fn from_json(v: &Value) -> Option<Self> {
        let (is_reminder_on, reminder_at, reminder_signal_at) = parse_message_reminder(v);
        let (_, meeting_message_type) = parse_meeting_message_type(v);
        let (_, protection) = parse_message_protection(v);
        let (_, meeting_reminder_on) = parse_meeting_reminder(v);
        let (_, in_reply_to) = parse_in_reply_to(v);
        let size = parse_message_size(v);
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
            categories: string_list(v, "categories"),
            is_reminder_on,
            reminder_at,
            reminder_signal_at,
            flag_due_at: parse_flag_due(v),
            meeting_message_type,
            protection,
            meeting_reminder_on,
            internet_message_id: str_field(v, "internetMessageId"),
            in_reply_to,
            size,
        })
    }
}

/// `PidTagInReplyToId` (property tag `0x1042`, PT_UNICODE) as a Graph
/// extended property id (#2096): the Internet message id of the message a
/// reply answers, which Exchange sets on a `createReply` /
/// `createReplyAll` draft and on a reply received.
pub const IN_REPLY_TO_PROP: &str = "String 0x1042";

/// Whether `id` names [`IN_REPLY_TO_PROP`] (#2096): `String ` (any case)
/// and a hex tag equal to `0x1042`, so a lower-case echo (`string 0x1042`)
/// matches. Another type (`Integer 0x1042`) or tag does not.
pub fn is_in_reply_to_prop(id: &str) -> bool {
    is_tag_prop(id, "String", 0x1042)
}

/// A message's In-Reply-To (#2096), and whether the item carried it: the
/// string value of its `singleValueExtendedProperties` entry for
/// [`IN_REPLY_TO_PROP`], kept as Graph sends it (a `<…>` id compares
/// verbatim with `internetMessageId`). No array, no such entry, or a
/// non-string value is `(false, "")`, never an error. A stored value is
/// never cleared through a merge (`Store::merge_message`'s non-empty
/// guard), so not carried and carried `""` both keep it.
pub fn parse_in_reply_to(v: &Value) -> (bool, String) {
    v.get("singleValueExtendedProperties")
        .and_then(Value::as_array)
        .unwrap_or(&[])
        .iter()
        .find_map(|p| {
            let id = p.get("id").and_then(Value::as_str)?;
            if !is_in_reply_to_prop(id) {
                return None;
            }
            p.get("value").and_then(Value::as_str)
        })
        .map_or((false, String::new()), |s| (true, s.to_string()))
}

/// `PidTagMessageSize` (property tag `0x0E08`, PT_LONG) as a Graph extended
/// property id (#2123): a message's size in bytes, what classic Outlook's
/// Folder Size dialog sums.
pub const MESSAGE_SIZE_PROP: &str = "Integer 0x0E08";

/// `PidTagMessageSizeExtended` (property tag `0x0E08`, PT_I8) as a Graph
/// extended property id (#2123): on a mail folder, the size of the items it
/// holds, without its subfolders.
pub const FOLDER_SIZE_PROP: &str = "Long 0x0E08";

/// Whether `id` names [`MESSAGE_SIZE_PROP`] (#2123): `Integer ` (any case)
/// and a hex tag equal to `0x0E08`, so `Integer 0xe08` matches; `Long
/// 0x0E08` (the folder's) does not.
pub fn is_message_size_prop(id: &str) -> bool {
    is_tag_prop(id, "Integer", 0x0E08)
}

/// Whether `id` names [`FOLDER_SIZE_PROP`] (#2123): `Long ` (any case) and a
/// hex tag equal to `0x0E08`.
pub fn is_folder_size_prop(id: &str) -> bool {
    is_tag_prop(id, "Long", 0x0E08)
}

/// The value of the first `singleValueExtendedProperties` entry of `v` whose
/// id `is_prop` accepts, read as a non-negative integer: a JSON integer or a
/// string holding one (Graph sends extended values as strings). No array, no
/// such entry or any other value is `None`, never an error.
fn size_prop(v: &Value, is_prop: fn(&str) -> bool) -> Option<i64> {
    let prop = v
        .get("singleValueExtendedProperties")
        .and_then(Value::as_array)?
        .iter()
        .find(|p| p.get("id").and_then(Value::as_str).is_some_and(is_prop))?;
    let n = match prop.get("value")? {
        Value::Num(n) if n.fract() == 0.0 && *n >= 0.0 && *n <= i64::MAX as f64 => *n as i64,
        Value::Str(s) => s.trim().parse::<i64>().ok()?,
        _ => return None,
    };
    (n >= 0).then_some(n)
}

/// A message's size (#2123): its [`MESSAGE_SIZE_PROP`] entry as a
/// non-negative integer (string or number), else `None`.
pub fn parse_message_size(v: &Value) -> Option<i64> {
    size_prop(v, is_message_size_prop)
}

/// A mail folder's own size (#2123): its [`FOLDER_SIZE_PROP`] entry as a
/// non-negative integer (string or number), else `None`.
pub fn parse_folder_size(v: &Value) -> Option<i64> {
    size_prop(v, is_folder_size_prop)
}

/// `PidTagMessageClass` (property tag `0x001A`, PT_UNICODE) as a Graph
/// extended property id (#2081): every message's class, such as `IPM.Note`
/// or `IPM.Schedule.Meeting.Request`, from which its meeting message type
/// is read ([`MeetingMessageType::from_message_class`]).
pub const MESSAGE_CLASS_PROP: &str = "String 0x001A";

/// Whether `id` names [`MESSAGE_CLASS_PROP`] (#2081): `String ` (any case)
/// and a hex tag equal to `0x1A`, so the short form Graph may echo back
/// (`String 0x1a`) matches too. Another type (`Integer 0x001A`) or tag
/// does not.
pub fn is_message_class_prop(id: &str) -> bool {
    is_tag_prop(id, "String", 0x1A)
}

/// Whether `id` names the tagged (not named) extended property of type
/// `type_word` and property tag `tag`: `type_word` (ASCII case ignored),
/// one space, `0x` or `0X` and hex digits whose value is `tag`, so a short
/// or long form (`0x1a`, `0x001A`) matches and another type does not
/// (#2081, #2088).
fn is_tag_prop(id: &str, type_word: &str, tag: u32) -> bool {
    let n = type_word.len();
    let Some(rest) = id
        .get(..n)
        .filter(|p| p.eq_ignore_ascii_case(type_word))
        .and_then(|_| id.get(n..))
        .and_then(|r| r.strip_prefix(' '))
    else {
        return false;
    };
    let Some(hex) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) else {
        return false;
    };
    !hex.is_empty()
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
        && u32::from_str_radix(hex, 16) == Ok(tag)
}

/// `PidTagSensitivity` (property tag `0x0036`, PT_LONG) as a Graph extended
/// property id (#2088): the message's Sensitivity, 0 normal, 1 personal,
/// 2 private, 3 confidential (MS-OXCMSG 2.2.1.13).
pub const SENSITIVITY_PROP: &str = "Integer 0x0036";

/// The `msip_labels` internet header as a PS_INTERNET_HEADERS named
/// property (#2088): the message's Microsoft Purview sensitivity label
/// metadata (`MSIP_Label_<guid>_Enabled=true; …_Name=…; …`), the header
/// #2015 stores with a fetched body.
pub const MSIP_LABELS_PROP: &str = "String {00020386-0000-0000-C000-000000000046} Name msip_labels";

/// `PidNameContentClass` (PS_INTERNET_HEADERS, name `Content-Class`,
/// PT_UNICODE) as a Graph extended property id (#2088): `rpmsg.message` on
/// a rights-managed (IRM) message (MS-OXORMMS 2.2.2.1).
pub const CONTENT_CLASS_PROP: &str =
    "String {00020386-0000-0000-C000-000000000046} Name Content-Class";

/// A message's Sensitivity (`PidTagSensitivity`, #2088). An absent or
/// unknown value is `Normal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sensitivity {
    #[default]
    Normal,
    Personal,
    Private,
    Confidential,
}

impl Sensitivity {
    /// MAPI's value: 0 normal, 1 personal, 2 private, 3 confidential;
    /// anything else is `Normal`, never an error.
    pub fn from_mapi(n: i64) -> Sensitivity {
        match n {
            1 => Sensitivity::Personal,
            2 => Sensitivity::Private,
            3 => Sensitivity::Confidential,
            _ => Sensitivity::Normal,
        }
    }

    /// MAPI's value, as [`Sensitivity::from_mapi`] reads it back: what the
    /// store keeps in `messages.sensitivity`.
    pub fn as_mapi(self) -> i64 {
        match self {
            Sensitivity::Normal => 0,
            Sensitivity::Personal => 1,
            Sensitivity::Private => 2,
            Sensitivity::Confidential => 3,
        }
    }
}

/// What a message list needs to show a protected message without opening
/// it (#2088). The default is an unprotected message.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MessageProtection {
    /// `PidTagSensitivity`.
    pub sensitivity: Sensitivity,
    /// The message carries an enabled Microsoft Purview sensitivity label
    /// (its `msip_labels` metadata names one with `Enabled=true`).
    pub has_label: bool,
    /// That label's name (`MSIP_Label_<guid>_Name`). `""` with `has_label`
    /// means labelled, but the metadata does not name it (newer clients may
    /// leave `_Name` out); `""` without it means no label.
    pub label_name: String,
    /// The message is rights-protected (IRM): its `Content-Class` is
    /// `rpmsg.message`.
    pub is_rights_protected: bool,
}

/// The enabled sensitivity label in an `msip_labels` value (#2088), and
/// its name (`""` when the metadata names none): `None` when no label is
/// enabled. The value is `;`-separated `key=value` pairs, each key
/// `MSIP_Label_<guid>_<field>`; the GUID holds no `_`, so a key splits at
/// its last `_`. Keys, the `Enabled` field's `true` and GUIDs compare
/// ignoring ASCII case; a value may be wrapped in one pair of `"`. The
/// first GUID (in order of appearance) enabled wins: a message can carry a
/// sender tenant's label besides ours, and the tenant is not known here.
/// Lenient: anything unreadable is skipped.
pub(crate) fn parse_msip_labels(value: &str) -> Option<String> {
    let mut fields: Vec<(&str, &str, &str)> = Vec::new();
    for part in value.split(';') {
        let Some((key, val)) = part.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let val = val.trim();
        let val = val
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(val);
        const PREFIX: &str = "MSIP_Label_";
        let Some(rest) = key
            .get(..PREFIX.len())
            .filter(|p| p.eq_ignore_ascii_case(PREFIX))
            .and_then(|_| key.get(PREFIX.len()..))
        else {
            continue;
        };
        let Some((guid, field)) = rest.rsplit_once('_') else {
            continue;
        };
        if guid.is_empty() {
            continue;
        }
        fields.push((guid, field, val));
    }
    let (guid, _, _) = fields.iter().find(|(_, field, val)| {
        field.eq_ignore_ascii_case("Enabled") && val.eq_ignore_ascii_case("true")
    })?;
    let name = fields
        .iter()
        .find(|(g, field, _)| g.eq_ignore_ascii_case(guid) && field.eq_ignore_ascii_case("Name"))
        .map(|(_, _, val)| (*val).to_string())
        .unwrap_or_default();
    Some(name)
}

/// A message's protection (#2088), and whether the item carried it: one
/// pass over its `singleValueExtendedProperties`. Lenient, never an error.
///
/// Carried means the array holds a [`MESSAGE_CLASS_PROP`] entry. The three
/// properties read here are each absent on most mail, so their absence
/// alone cannot tell "none" from "not asked"; every message has a class
/// (#2081), and the class rides the same `$expand`, so an item carrying it
/// was read with the expand, and an absent Sensitivity, label or content
/// class then means none. An item without the class (partial, read without
/// the expand, a silently dropped expand) is `(false, default)`, and a
/// stored protection is kept (`MessageFields::writes_protection`).
///
/// - Sensitivity ([`SENSITIVITY_PROP`]): a JSON integer or a string holding
///   one ([`Sensitivity::from_mapi`]); anything else is `Normal`.
/// - Label ([`MSIP_LABELS_PROP`]): [`parse_msip_labels`] of a string value.
/// - IRM ([`CONTENT_CLASS_PROP`]): the value trimmed is `rpmsg.message`,
///   ASCII case ignored.
///
/// Named ids compare ASCII-case-insensitively (Graph may lower-case the
/// GUID, and PS_INTERNET_HEADERS names are header names); the tag id
/// matches its short form too (`Integer 0x36`).
pub fn parse_message_protection(v: &Value) -> (bool, MessageProtection) {
    let mut carried = false;
    let mut p = MessageProtection::default();
    let props = v
        .get("singleValueExtendedProperties")
        .and_then(Value::as_array)
        .unwrap_or(&[]);
    for prop in props {
        let Some(id) = prop.get("id").and_then(Value::as_str) else {
            continue;
        };
        let value = prop.get("value");
        if is_message_class_prop(id) {
            carried = true;
        } else if is_tag_prop(id, "Integer", 0x36) {
            let n = match value {
                Some(Value::Num(n)) if n.fract() == 0.0 => Some(*n as i64),
                Some(Value::Str(s)) => s.trim().parse::<i64>().ok(),
                _ => None,
            };
            p.sensitivity = n.map(Sensitivity::from_mapi).unwrap_or_default();
        } else if id.eq_ignore_ascii_case(MSIP_LABELS_PROP) {
            let label = value.and_then(Value::as_str).and_then(parse_msip_labels);
            p.has_label = label.is_some();
            p.label_name = label.unwrap_or_default();
        } else if id.eq_ignore_ascii_case(CONTENT_CLASS_PROP) {
            p.is_rights_protected = value
                .and_then(Value::as_str)
                .is_some_and(|c| c.trim().eq_ignore_ascii_case("rpmsg.message"));
        }
    }
    if carried {
        (true, p)
    } else {
        (false, MessageProtection::default())
    }
}

/// A message's meeting message type (#2081), and whether the item carried
/// it: a top-level `meetingMessageType` string (read by
/// [`MeetingMessageType::from_graph`]) wins, else a
/// `singleValueExtendedProperties` entry for [`MESSAGE_CLASS_PROP`] with a
/// string value ([`MeetingMessageType::from_message_class`]). Neither, or
/// only non-string values, is not carried: `(false, None)`, so a stored
/// type is kept (`MessageFields::writes_meeting_type`). Never an error.
pub fn parse_meeting_message_type(v: &Value) -> (bool, Option<MeetingMessageType>) {
    if let Some(s) = v.get("meetingMessageType").and_then(Value::as_str) {
        return (true, MeetingMessageType::from_graph(s));
    }
    let class = v
        .get("singleValueExtendedProperties")
        .and_then(Value::as_array)
        .unwrap_or(&[])
        .iter()
        .find_map(|p| {
            let id = p.get("id").and_then(Value::as_str)?;
            if !is_message_class_prop(id) {
                return None;
            }
            p.get("value").and_then(Value::as_str)
        });
    match class {
        Some(c) => (true, MeetingMessageType::from_message_class(c)),
        None => (false, None),
    }
}

/// `PidLidReminderSet` (PSETID_Common LID `0x8503`, PT_BOOLEAN) as a Graph
/// extended property id (#2054): a message's reminder is on.
pub const REMINDER_SET_PROP: &str = "Boolean {00062008-0000-0000-C000-000000000046} Id 0x8503";
/// `PidLidReminderTime` (PSETID_Common LID `0x8502`, PT_SYSTIME): the time
/// the reminder was set for.
pub const REMINDER_TIME_PROP: &str = "SystemTime {00062008-0000-0000-C000-000000000046} Id 0x8502";
/// `PidLidReminderSignalTime` (PSETID_Common LID `0x8560`, PT_SYSTIME): when
/// the reminder fires next.
pub const REMINDER_SIGNAL_TIME_PROP: &str =
    "SystemTime {00062008-0000-0000-C000-000000000046} Id 0x8560";

/// Reads a message's reminder from its `singleValueExtendedProperties`
/// (#2054): `(on, reminder time, signal time)`. Lenient, never an error:
/// anything that is not an array of `{id, value}` objects is no property, a
/// property id is matched ASCII-case-insensitively (Graph may lower-case
/// the GUID), the boolean is on only for `"true"` (any case) or JSON `true`,
/// and a time is kept only when [`reminder_time`] reads it. On means set and
/// at least one readable time.
pub fn parse_message_reminder(v: &Value) -> (bool, String, String) {
    let mut set = false;
    let mut at = String::new();
    let mut signal = String::new();
    let props = v
        .get("singleValueExtendedProperties")
        .and_then(Value::as_array)
        .unwrap_or(&[]);
    for p in props {
        let Some(id) = p.get("id").and_then(Value::as_str) else {
            continue;
        };
        let value = p.get("value");
        if id.eq_ignore_ascii_case(REMINDER_SET_PROP) {
            set = reminder_set(value);
        } else if id.eq_ignore_ascii_case(REMINDER_TIME_PROP) {
            at = value
                .and_then(Value::as_str)
                .map(reminder_time)
                .unwrap_or_default();
        } else if id.eq_ignore_ascii_case(REMINDER_SIGNAL_TIME_PROP) {
            signal = value
                .and_then(Value::as_str)
                .map(reminder_time)
                .unwrap_or_default();
        }
    }
    let on = set && !(at.is_empty() && signal.is_empty());
    (on, at, signal)
}

/// A [`REMINDER_SET_PROP`] value read as a boolean (#2054): on only for
/// JSON `true` or the string `"true"` (any case); anything else, or no
/// value, is off. Shared by [`parse_message_reminder`] and
/// [`parse_meeting_reminder`] so both read the bit alike.
fn reminder_set(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Str(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

/// Whether a `PidTagMessageClass` names a meeting message (#2089):
/// `IPM.Schedule.Meeting` itself, or it followed by `.` and a subclass of
/// at least one character, ASCII case ignored and not trimmed: the rule
/// [`MeetingMessageType::from_message_class`] applies to each of its
/// classes. So a request, a cancellation, a response and a meeting forward
/// notification (`IPM.Schedule.Meeting.Notification.Forward`, which has no
/// `MeetingMessageType`) are meeting classes; `IPM.Schedule.Meeting.` and
/// `IPM.Schedule.MeetingX` are not (though [`parse_meeting_reminder`] still
/// calls an item a meeting message by its `@odata.type`).
fn is_meeting_class(class: &str) -> bool {
    const BASE: &str = "IPM.Schedule.Meeting";
    let Some(head) = class.get(..BASE.len()) else {
        return false;
    };
    let tail = &class[BASE.len()..];
    head.eq_ignore_ascii_case(BASE)
        && (tail.is_empty() || (tail.len() > 1 && tail.starts_with('.')))
}

/// A meeting message's own reminder (#2089), and whether the item carried
/// it: `(carried, on)`, one pass over its `singleValueExtendedProperties`.
/// Lenient, never an error.
///
/// Carried means the array holds a [`MESSAGE_CLASS_PROP`] entry, whatever
/// its value: the rule [`parse_message_protection`] uses, for the same
/// reason. `PidLidReminderSet` is absent on most mail, so its absence alone
/// cannot tell "not set" from "not asked"; the class rides the same
/// `$expand`, so an item carrying it was read with the expand. An item
/// without it (partial, read without the expand, a refused or silently
/// dropped expand, a plain GET with only a top-level `meetingMessageType`)
/// is `(false, false)`, and a stored value is kept
/// (`MessageFields::writes_meeting_reminder`).
///
/// On means carried, a meeting message and the set bit
/// ([`REMINDER_SET_PROP`], read as [`parse_message_reminder`] reads it). A
/// meeting message is an `@odata.type` starting with
/// `#microsoft.graph.eventMessage` or a string class
/// [`is_meeting_class`] accepts. No reminder time is needed: unlike the
/// flag's reminder this one never fires, and the bell is the set bit.
pub fn parse_meeting_reminder(v: &Value) -> (bool, bool) {
    let mut carried = false;
    let mut meeting_class = false;
    let mut set = false;
    let props = v
        .get("singleValueExtendedProperties")
        .and_then(Value::as_array)
        .unwrap_or(&[]);
    for p in props {
        let Some(id) = p.get("id").and_then(Value::as_str) else {
            continue;
        };
        let value = p.get("value");
        if is_message_class_prop(id) {
            carried = true;
            meeting_class = value.and_then(Value::as_str).is_some_and(is_meeting_class);
        } else if id.eq_ignore_ascii_case(REMINDER_SET_PROP) {
            set = reminder_set(value);
        }
    }
    if !carried {
        return (false, false);
    }
    let meeting = meeting_class
        || v.get("@odata.type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.starts_with("#microsoft.graph.eventMessage"));
    (true, meeting && set)
}

/// A UTC time Graph sent (an extended property's SystemTime, a followup
/// flag's `dateTime`) as `YYYY-MM-DDTHH:MM:SSZ`, or `""` when it cannot be
/// read (#2054). Only a time ending in `Z` or with nothing after its
/// seconds (a fraction allowed) is read: `to_utc` drops a numeric offset
/// without applying it, which would give the wrong time.
pub(crate) fn reminder_time(s: &str) -> String {
    let s = s.trim();
    let bare = s.strip_suffix('Z').unwrap_or(s);
    let Some((_, time)) = bare.split_once('T') else {
        return String::new();
    };
    if !time
        .chars()
        .all(|c| c.is_ascii_digit() || c == ':' || c == '.')
    {
        return String::new();
    }
    let utc = to_utc(bare, "UTC");
    if crate::store::iso8601_to_unix_secs(&utc).is_some() {
        utc
    } else {
        String::new()
    }
}

/// `flag.dueDateTime` in UTC (#2054), `""` when absent, unreadable or in a
/// zone other than UTC (an empty zone is read as UTC).
fn parse_flag_due(v: &Value) -> String {
    let Some(due) = v.get("flag").and_then(|f| f.get("dueDateTime")) else {
        return String::new();
    };
    let zone = due.get("timeZone").and_then(Value::as_str).unwrap_or("");
    if !zone.is_empty() && !is_utc_zone(zone) {
        return String::new();
    }
    due.get("dateTime")
        .and_then(Value::as_str)
        .map(reminder_time)
        .unwrap_or_default()
}

/// Graph `eventMessage.meetingMessageType` (#2024): what a meeting message
/// in mail is. Graph's `none` (an `eventMessage` that is none of these) is
/// no type at all, as is an absent or unknown value: see
/// [`MeetingMessageType::from_graph`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeetingMessageType {
    /// `meetingRequest`: an invite the user can respond to.
    Request,
    /// `meetingCancelled`: the organizer cancelled the meeting; the user
    /// may remove it from the calendar (`SyncCommand::RemoveFromCalendar`).
    Cancelled,
    /// `meetingAccepted`: an attendee's acceptance.
    Accepted,
    /// `meetingTenativelyAccepted` (Graph's spelling): an attendee's
    /// tentative acceptance.
    TentativelyAccepted,
    /// `meetingDeclined`: an attendee's decline.
    Declined,
}

impl MeetingMessageType {
    /// Maps Graph's value. `meetingTenativelyAccepted` is the v1.0 enum's
    /// own spelling; the corrected `meetingTentativelyAccepted` is read too.
    /// `none`, an unknown value and `""` are `None`: lenient, never an error.
    pub fn from_graph(s: &str) -> Option<MeetingMessageType> {
        match s {
            "meetingRequest" => Some(MeetingMessageType::Request),
            "meetingCancelled" => Some(MeetingMessageType::Cancelled),
            "meetingAccepted" => Some(MeetingMessageType::Accepted),
            "meetingTenativelyAccepted" | "meetingTentativelyAccepted" => {
                Some(MeetingMessageType::TentativelyAccepted)
            }
            "meetingDeclined" => Some(MeetingMessageType::Declined),
            _ => None,
        }
    }

    /// Graph's value for the type, as [`MeetingMessageType::from_graph`]
    /// reads it back: what the store keeps in `messages.meeting_message_type`
    /// (#2081). Tentative is the v1.0 enum's own spelling.
    pub fn as_graph(self) -> &'static str {
        match self {
            MeetingMessageType::Request => "meetingRequest",
            MeetingMessageType::Cancelled => "meetingCancelled",
            MeetingMessageType::Accepted => "meetingAccepted",
            MeetingMessageType::TentativelyAccepted => "meetingTenativelyAccepted",
            MeetingMessageType::Declined => "meetingDeclined",
        }
    }

    /// The type a message class names (#2081, Outlook's meeting classes,
    /// MS-OXOCAL): `IPM.Schedule.Meeting.Request`, `.Canceled`,
    /// `.Resp.Pos`, `.Resp.Tent` and `.Resp.Neg`. MAPI compares classes
    /// ASCII-case-insensitively, and a class may be subclassed (`.` and a
    /// suffix), which keeps its base's type. Anything else (`IPM.Note`, a
    /// meeting forward notification, `""`) is `None`.
    pub fn from_message_class(class: &str) -> Option<MeetingMessageType> {
        const CLASSES: [(&str, MeetingMessageType); 5] = [
            ("IPM.Schedule.Meeting.Request", MeetingMessageType::Request),
            (
                "IPM.Schedule.Meeting.Canceled",
                MeetingMessageType::Cancelled,
            ),
            (
                "IPM.Schedule.Meeting.Resp.Pos",
                MeetingMessageType::Accepted,
            ),
            (
                "IPM.Schedule.Meeting.Resp.Tent",
                MeetingMessageType::TentativelyAccepted,
            ),
            (
                "IPM.Schedule.Meeting.Resp.Neg",
                MeetingMessageType::Declined,
            ),
        ];
        CLASSES.iter().find_map(|&(base, t)| {
            let head = class.get(..base.len())?;
            let tail = &class[base.len()..];
            let subclass = tail.len() > 1 && tail.starts_with('.');
            (head.eq_ignore_ascii_case(base) && (tail.is_empty() || subclass)).then_some(t)
        })
    }
}

/// A message's meeting facts (#2024), from
/// `GraphClient::meeting_message`: its `meetingMessageType` and the
/// calendar event it refers to (the expanded `eventMessage/event`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MeetingMessage {
    /// `None` for a message that is not a meeting message (no
    /// `meetingMessageType`, or Graph's `none`).
    pub message_type: Option<MeetingMessageType>,
    /// The referred event's id. `None` when the message carries no `event`:
    /// not a meeting message, or an event already removed from the calendar.
    pub event_id: Option<String>,
    /// The referred event's `isOrganizer`; false when absent or with no
    /// event. Remove from Calendar refuses the organizer's event, whose
    /// delete would send cancellations.
    pub is_organizer: bool,
}

impl MeetingMessage {
    /// Reads a message GET with the `eventMessage/event` expand. Never
    /// fails: a plain message is all `None` / false.
    pub fn from_json(v: &Value) -> MeetingMessage {
        let event = v.get("event");
        MeetingMessage {
            message_type: v
                .get("meetingMessageType")
                .and_then(Value::as_str)
                .and_then(MeetingMessageType::from_graph),
            event_id: event
                .and_then(|e| e.get("id"))
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
            is_organizer: event
                .and_then(|e| e.get("isOrganizer"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
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
    /// `singleValueExtendedProperties`, from which the reminder is read
    /// (#2054).
    pub reminder: bool,
    /// Not a key: the item came from a request that did not ask for the
    /// reminder (#2054: Graph refused the `$expand`, or a delta link minted
    /// from such a call), so its lack of `singleValueExtendedProperties`
    /// says nothing. Never set by `of` or `all`.
    pub reminder_unasked: bool,
    /// The meeting message type (#2081): a top-level `meetingMessageType`
    /// string or a `PidTagMessageClass` entry with a string value
    /// (`parse_meeting_message_type`'s carried flag).
    pub meeting_type: bool,
    /// The protection (#2088): the item carried its `PidTagMessageClass`
    /// entry, so it was read with the `$expand` that asks for the
    /// protection properties too (`parse_message_protection`'s carried
    /// flag).
    pub protection: bool,
    /// A meeting message's own reminder (#2089): the item carried its
    /// `PidTagMessageClass` entry (`parse_meeting_reminder`'s carried flag,
    /// the same rule as `protection`).
    pub meeting_reminder: bool,
    /// `internetMessageId` as a string (#2096).
    pub internet_message_id: bool,
    /// The In-Reply-To (#2096): a string `PidTagInReplyToId` entry
    /// (`parse_in_reply_to`'s carried flag).
    pub in_reply_to: bool,
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
            reminder: has("singleValueExtendedProperties"),
            reminder_unasked: false,
            meeting_type: parse_meeting_message_type(v).0,
            protection: parse_message_protection(v).0,
            meeting_reminder: parse_meeting_reminder(v).0,
            internet_message_id: v.get("internetMessageId").and_then(Value::as_str).is_some(),
            in_reply_to: parse_in_reply_to(v).0,
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
            reminder: true,
            reminder_unasked: false,
            meeting_type: true,
            protection: true,
            meeting_reminder: true,
            internet_message_id: true,
            in_reply_to: true,
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

    /// Whether `is_draft` is to be written. `isDraft` is in
    /// `MESSAGE_SELECT` (#2096), so a complete delta item carries Graph's
    /// value; a complete item without the key (a `Message` built in code,
    /// as `upsert_message` merges) writes its parsed `is_draft`, and a
    /// partial item keeps the stored value unless it carries the key.
    pub fn writes_is_draft(&self) -> bool {
        self.is_draft || !self.is_partial()
    }

    /// Whether the reminder columns are to be written (#2054). Graph
    /// leaves `singleValueExtendedProperties` out when the expand's filter
    /// matches nothing, so a complete item without it means "no
    /// reminder"; on a partial item, or one read without asking for the
    /// reminder (`reminder_unasked`), its absence says nothing, and the
    /// stored reminder is kept. An item carrying the key always writes.
    pub fn writes_reminder(&self) -> bool {
        self.reminder || (!self.is_partial() && !self.reminder_unasked)
    }

    /// Whether `meeting_message_type` is to be written (#2081): only when
    /// the item carried it. Every message has a class, so a request that
    /// asked for it always gets it back; an item without it (partial, read
    /// without the `$expand`, or a silently dropped expand) says nothing,
    /// and the stored type, perhaps one `FetchMeetingMessage` wrote, is
    /// kept. A carried `IPM.Note` writes `None`.
    pub fn writes_meeting_type(&self) -> bool {
        self.meeting_type
    }

    /// Whether the protection columns are to be written (#2088): only when
    /// the item carried its message class, which proves the `$expand` ran
    /// on it (see `parse_message_protection`). An item without it says
    /// nothing, and the stored protection is kept; a carried item without
    /// any protection property writes the default.
    pub fn writes_protection(&self) -> bool {
        self.protection
    }

    /// Whether `meeting_reminder_on` is to be written (#2089): only when
    /// the item carried its message class, as for the protection. An item
    /// without it says nothing, and the stored value is kept; a carried
    /// item that is not a meeting message, or has no set bit, writes false.
    pub fn writes_meeting_reminder(&self) -> bool {
        self.meeting_reminder
    }

    /// Whether `internet_message_id` may be written (#2096): only when the
    /// item carried it. The store writes it only when non-empty too, so a
    /// stored id is never cleared.
    pub fn writes_internet_message_id(&self) -> bool {
        self.internet_message_id
    }

    /// Whether `in_reply_to` may be written (#2096): only when the item
    /// carried a string `PidTagInReplyToId`. As for the Internet message
    /// id, the store never clears a stored value.
    pub fn writes_in_reply_to(&self) -> bool {
        self.in_reply_to
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

/// The internet headers a fetched body brings along that the store keeps
/// with the message. Matched ignoring ASCII case. Only these, never the
/// whole header block:
///
/// - Exchange's own stamps of where a message came from (#1933), so a
///   caller can tell mail from inside the tenant (`AuthAs: Internal`,
///   `MessageDirectionality: Originating`) from outside mail (`Anonymous`,
///   `Incoming`) without trusting the sender's domain.
/// - `msip_labels`, the message's Microsoft Purview sensitivity label
///   (#2015): `MSIP_Label_<guid>_Name=Confidential; ...` fields, kept as
///   Graph sends them (trimmed) for the caller to parse; mailcore does not.
pub const STORED_HEADERS: &[&str] = &[
    "X-MS-Exchange-Organization-AuthAs",
    "X-MS-Exchange-Organization-MessageDirectionality",
    "msip_labels",
];

/// Whether `name` is one of [`STORED_HEADERS`], ignoring ASCII case.
pub fn is_stored_header(name: &str) -> bool {
    STORED_HEADERS.iter().any(|h| h.eq_ignore_ascii_case(name))
}

/// One internet message header (`internetMessageHeaders` entry) of a
/// fetched body (#1933): the name as Graph sent it, the value trimmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageHeader {
    pub name: String,
    pub value: String,
}

impl MessageHeader {
    /// The [`STORED_HEADERS`] in a message response's
    /// `internetMessageHeaders`, values trimmed, the first of each name
    /// (ignoring case) kept. Lenient: no field, a null or non-array one, or
    /// an entry without a string `name` and `value` gives no header for it,
    /// never an error, so a body still stores without them.
    pub fn list_from_json(v: &Value) -> Vec<MessageHeader> {
        let mut out: Vec<MessageHeader> = Vec::new();
        let Some(items) = v.get("internetMessageHeaders").and_then(Value::as_array) else {
            return out;
        };
        for item in items {
            let (Some(name), Some(value)) = (
                item.get("name").and_then(Value::as_str),
                item.get("value").and_then(Value::as_str),
            ) else {
                continue;
            };
            if !is_stored_header(name) || out.iter().any(|h| h.name.eq_ignore_ascii_case(name)) {
                continue;
            }
            out.push(MessageHeader {
                name: name.to_string(),
                value: value.trim().to_string(),
            });
        }
        out
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

/// One message a server search (`GraphClient::search_messages`, #2006)
/// found, or one `GraphClient::get_message_hit` fetched: the folder it is
/// filed in (`parentFolderId`, empty when Graph sent none) and what a delta
/// row carries (`DeltaItem::Upsert`), so a client can scope the hits and
/// show where each one lives, and the engine can store an opened one as a
/// delta would.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub folder_id: String,
    pub message: Message,
    pub fields: MessageFields,
}

impl SearchHit {
    /// `None` for an item without an id (`Message::from_json` takes any
    /// object, with an empty id when it has none).
    pub fn from_json(v: &Value) -> Option<Self> {
        let message = Message::from_json(v)?;
        if message.id.is_empty() {
            return None;
        }
        Some(SearchHit {
            folder_id: str_field(v, "parentFolderId"),
            message,
            fields: MessageFields::of(v),
        })
    }
}

/// A page of results from Graph's delta query, plus the pagination/delta
/// tokens to continue syncing.
#[derive(Debug, Clone, PartialEq)]
pub struct DeltaPage {
    pub items: Vec<DeltaItem>,
    pub next_link: Option<String>,
    pub delta_link: Option<String>,
    /// The first call asked for the reminder `$expand` and Graph refused it
    /// (#2054): the page came from the call sent again without it, and
    /// every upsert's `MessageFields::reminder_unasked` is set. The engine
    /// keeps the refusal with the walk and stores it with the walk's link
    /// (`Store::set_reminder_expand_refused`), as the links Graph mints
    /// from this call carry no expand either.
    pub expand_refused: bool,
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
            expand_refused: false,
        })
    }

    /// Marks every upsert as read without the reminder `$expand`
    /// (`MessageFields::reminder_unasked`, #2054).
    pub fn mark_reminder_unasked(&mut self) {
        for item in &mut self.items {
            if let DeltaItem::Upsert(_, fields) = item {
                fields.reminder_unasked = true;
            }
        }
    }
}

/// One entry of a calendarView delta page (#2013): an added or changed
/// occurrence, or the id of one removed since the last round (an item
/// with `@removed`). What a removal deletes is the store's rule
/// (`apply_calendar_delta_on`): only rows in the window, and a series
/// master's occurrences unless the round names some of them.
#[derive(Debug, Clone, PartialEq)]
pub enum EventDeltaItem {
    Upsert(Event),
    Removed(String),
}

/// A page of `/me/calendars/{id}/calendarView/delta` (#2013), with the
/// `nextLink` to the next page or, on the last page, the `deltaLink` the
/// next round starts from.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDeltaPage {
    pub items: Vec<EventDeltaItem>,
    pub next_link: Option<String>,
    pub delta_link: Option<String>,
}

impl EventDeltaPage {
    /// `None` without a `value` array. A removed item without an id is
    /// dropped (there is nothing to remove).
    pub fn from_json(v: &Value) -> Option<Self> {
        let items = v
            .get("value")?
            .as_array()?
            .iter()
            .filter_map(|item| {
                if item.get("@removed").is_some() {
                    opt_str_field(item, "id")
                        .filter(|id| !id.is_empty())
                        .map(EventDeltaItem::Removed)
                } else {
                    Event::from_json(item).map(EventDeltaItem::Upsert)
                }
            })
            .collect();
        Some(EventDeltaPage {
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
    /// The new time this attendee proposed (Graph
    /// `attendees[].proposedNewTime`, the organizer's view, #2023), in
    /// canonical UTC. Both `""` when they proposed none: a null, absent or
    /// half slot is no proposal.
    pub proposed_start_utc: String,
    pub proposed_end_utc: String,
}

impl Attendee {
    pub fn from_json(v: &Value) -> Option<Self> {
        let addr = v.get("emailAddress")?;
        let (proposed_start_utc, proposed_end_utc) = proposed_slot(v);
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
            proposed_start_utc,
            proposed_end_utc,
        })
    }
}

/// An attendee's `proposedNewTime` slot as canonical UTC start and end, or
/// `("", "")` unless both `start.dateTime` and `end.dateTime` are strings:
/// `datetime_field_to_utc` on a missing side would read as
/// `0000-00-00T00:00:00Z`, and half a slot is no proposal.
fn proposed_slot(v: &Value) -> (String, String) {
    let side = |k: &str| {
        v.get("proposedNewTime")
            .and_then(|p| p.get(k))
            .filter(|d| d.get("dateTime").and_then(Value::as_str).is_some())
    };
    match (side("start"), side("end")) {
        (Some(s), Some(e)) => (datetime_field_to_utc(s), datetime_field_to_utc(e)),
        _ => (String::new(), String::new()),
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

/// A UTC time Graph sent as a `DateTimeOffset` (`responseStatus.time`,
/// `createdDateTime`, #2068) as `YYYY-MM-DDTHH:MM:SSZ`, or `""` for Graph's
/// "never" (`0001-01-01…`, caught first: it is otherwise a readable date)
/// and for anything `reminder_time` cannot read (a numeric offset, garbage).
fn graph_utc_or_empty(s: &str) -> String {
    reminder_time(&graph_date_or_empty(s))
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
        let days_of_week = string_list(pattern, "daysOfWeek");
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

/// A series' recurrence in Graph's whole shape (`event.recurrence`): read
/// from Graph (#1983), sent by a series edit (#2022) and by the create of a
/// local event given it as `LocalEventFields::series_recurrence` (#2034),
/// both with `to_send_json`. Lenient: every enum Graph uses is kept as its
/// raw string, so a value this crate does not know is kept, not dropped.
/// Not the create-side `Recurrence`, whose three kinds and `to_json` (a
/// `numbered` range becomes `noEnd`, `firstDayOfWeek` is forced to `sunday`)
/// would rewrite a synced series. The store keeps what Graph reports in its
/// own column, `events.graph_recurrence`, which is never re-sent; what a
/// create sends is in `events.recurrence` (`Store::event_for_send`).
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

    /// Graph's `recurrence` as a write sends it (#2022, a series edit):
    /// `to_json`'s column text writes `""` dates and zeros, which Graph
    /// refuses on a write (`"endDate":""` is not a Date). So the pattern
    /// sends `type` and `interval`, and `month`, `dayOfMonth`, `daysOfWeek`,
    /// `firstDayOfWeek` and `index` only when non-zero / non-empty (as
    /// given: nothing is defaulted, so a `monday` week stays `monday`); the
    /// range sends `type` and `startDate`, `endDate` only for an `endDate`
    /// range, `numberOfOccurrences` only for a `numbered` one, and
    /// `recurrenceTimeZone` only when non-empty. Lenient: an unknown `type`
    /// is sent as is, and nothing here errors.
    pub fn to_send_json(&self) -> Value {
        let s = |v: &str| Value::Str(v.to_string());
        let n = |v: u32| Value::Num(v as f64);
        let p = &self.pattern;
        let r = &self.range;
        let mut pattern = vec![
            ("type".to_string(), s(&p.r#type)),
            ("interval".to_string(), n(p.interval)),
        ];
        if p.month != 0 {
            pattern.push(("month".to_string(), n(p.month)));
        }
        if p.day_of_month != 0 {
            pattern.push(("dayOfMonth".to_string(), n(p.day_of_month)));
        }
        if !p.days_of_week.is_empty() {
            pattern.push((
                "daysOfWeek".to_string(),
                Value::Array(p.days_of_week.iter().map(|d| s(d)).collect()),
            ));
        }
        if !p.first_day_of_week.is_empty() {
            pattern.push(("firstDayOfWeek".to_string(), s(&p.first_day_of_week)));
        }
        if !p.index.is_empty() {
            pattern.push(("index".to_string(), s(&p.index)));
        }
        let mut range = vec![
            ("type".to_string(), s(&r.r#type)),
            ("startDate".to_string(), s(&r.start_date)),
        ];
        if r.r#type == "endDate" && !r.end_date.is_empty() {
            range.push(("endDate".to_string(), s(&r.end_date)));
        }
        if r.r#type == "numbered" {
            range.push((
                "numberOfOccurrences".to_string(),
                n(r.number_of_occurrences),
            ));
        }
        if !r.recurrence_time_zone.is_empty() {
            range.push(("recurrenceTimeZone".to_string(), s(&r.recurrence_time_zone)));
        }
        Value::Object(vec![
            ("pattern".to_string(), Value::Object(pattern)),
            ("range".to_string(), Value::Object(range)),
        ])
    }
}

/// One of the user's calendars, as `GET /me/calendars` lists it (#1984).
/// Missing fields read as `""` / `false`; `hex_color` is `""` when Graph
/// has no explicit colour (`color` is then `auto` or a preset name).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Calendar {
    pub id: String,
    pub name: String,
    /// Graph `color`: a preset name (`lightBlue`, ..., `auto`).
    pub color: String,
    /// Graph `hexColor` (`#RRGGBB`), `""` when absent.
    pub hex_color: String,
    pub is_default: bool,
    pub can_edit: bool,
    /// Graph `owner` (`{name, address}`, an `emailAddress`, not a
    /// `recipient`).
    pub owner_name: String,
    pub owner_addr: String,
}

impl Calendar {
    /// `None` for an item without a non-empty `id`.
    pub fn from_json(v: &Value) -> Option<Self> {
        let id = str_field(v, "id");
        if id.is_empty() {
            return None;
        }
        let owner = v.get("owner");
        let owner_field = |key: &str| owner.map(|o| str_field(o, key)).unwrap_or_default();
        let flag = |key: &str| v.get(key).and_then(Value::as_bool).unwrap_or(false);
        Some(Calendar {
            id,
            name: str_field(v, "name"),
            color: str_field(v, "color"),
            hex_color: str_field(v, "hexColor"),
            is_default: flag("isDefaultCalendar"),
            can_edit: flag("canEdit"),
            owner_name: owner_field("name"),
            owner_addr: owner_field("address"),
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
    /// Graph `responseStatus.time`: when the signed-in user answered the
    /// event (#2068), canonical UTC `YYYY-MM-DDTHH:MM:SSZ`; `""` when never
    /// answered (Graph's `0001-01-01T00:00:00Z`), absent or unreadable
    /// (`graph_utc_or_empty`).
    pub response_time: String,
    /// Graph `createdDateTime`: when the event was created, which a calendar
    /// client shows as a meeting's "Sent" date (#2068); canonical UTC, `""`
    /// when absent or unreadable.
    pub created_utc: String,
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
    /// The calendar holding the event: `calendar.id` when Graph expands it,
    /// else `""`. The engine sets it to the calendar whose `calendarView`
    /// returned the event (#1984), so a stored event always carries the id
    /// of the calendar it was fetched from.
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
            response_time: String::new(),
            created_utc: String::new(),
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
            .or_else(|| meeting_link_in(&location, &body_html))
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
            response_time: v
                .get("responseStatus")
                .and_then(|r| r.get("time"))
                .and_then(Value::as_str)
                .map(graph_utc_or_empty)
                .unwrap_or_default(),
            created_utc: v
                .get("createdDateTime")
                .and_then(Value::as_str)
                .map(graph_utc_or_empty)
                .unwrap_or_default(),
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

/// The meeting link an event's location or body holds: the location's
/// first, else the body's (`meeting_link`). The location goes first: it is a
/// field someone set, while a body can quote an old invite. `Event::from_json`
/// falls back to it after Graph's own join link, and the store derives a local
/// event's `join_url` with it on create and edit.
pub(crate) fn meeting_link_in(location: &str, body: &str) -> Option<String> {
    meeting_link(location).or_else(|| meeting_link(body))
}

/// The first link in `text` (an event's location or HTML/text body) that
/// joins a meeting on a known service, or `None`. A link starts at
/// `https://` (any case) and ends at a quote, `<`, `>`, whitespace, an
/// escaped bracket or quote (`&lt;`, `&gt;`, `&quot;`, `&#39;`, `&#x27;`) or
/// the end; `&amp;` is decoded, then trailing prose punctuation (`.`, `,`,
/// `;`, `:`, `)`, `]`, `!`) is trimmed. Only the parsed host decides the
/// service (`is_meeting_url`), and the scan resumes after a rejected link's
/// end, so a meeting link inside another link's query or path, or a
/// lookalike host, is not taken, and each byte is scanned once (a body is
/// sender-controlled: a long run of `https://` must stay linear). Safe
/// Links-wrapped links (`*.safelinks.protection.outlook.com/?url=...`) are
/// not unwrapped.
fn meeting_link(text: &str) -> Option<String> {
    const SCHEME: &str = "https://";
    // ASCII lowercasing keeps every byte offset, so offsets in `lower`
    // index `text` too (and land on the ASCII `h`, a char boundary).
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(SCHEME) {
        let start = from + pos;
        let end = link_end(text, start + SCHEME.len());
        let decoded = text[start..end].replace("&amp;", "&");
        let url = decoded.trim_end_matches(['.', ',', ';', ':', ')', ']', '!']);
        if is_meeting_url(&url[SCHEME.len()..]) {
            return Some(url.to_string());
        }
        from = end;
    }
    None
}

/// The byte offset where the link whose scheme ends at `from` ends: the
/// first quote, `<`, `>`, whitespace or HTML entity other than `&amp;`
/// (`is_entity_end`) at or after `from`, else `text.len()`.
fn link_end(text: &str, from: usize) -> usize {
    text[from..]
        .char_indices()
        .find(|&(i, c)| {
            matches!(c, '"' | '\'' | '<' | '>')
                || c.is_whitespace()
                || (c == '&' && is_entity_end(&text[from + i..]))
        })
        .map_or(text.len(), |(i, _)| from + i)
}

/// `at` (text starting at a `&`) starts an HTML entity that ends a link:
/// a numeric one (`&#…`: `&#160;`, `&#x27;`, `&#34;`) or a named one (an
/// ASCII letter, then up to 31 letters or digits, then `;`: `&nbsp;`,
/// `&lt;`, `&quot;`), except `&amp;`, which a link's query holds and
/// `meeting_link` decodes. A raw `&` not shaped like an entity
/// (`?pwd=x&y=1` in a plain-text location) stays in the link. The lookahead
/// is bounded, so the scan stays linear.
fn is_entity_end(at: &str) -> bool {
    let rest = &at.as_bytes()[1..];
    if at.starts_with("&amp;") {
        return false;
    }
    if rest.first() == Some(&b'#') {
        return true;
    }
    if !rest.first().is_some_and(u8::is_ascii_alphabetic) {
        return false;
    }
    let name = rest
        .iter()
        .take(33)
        .take_while(|b| b.is_ascii_alphanumeric())
        .count();
    name <= 32 && rest.get(name) == Some(&b';')
}

/// `after_scheme` (a link without its `https://`) is a meeting link: its
/// host and path (as is) match a known meeting service. The authority runs
/// to the first `/`, `?`, `#` or `\` (a browser reads `\` as `/` in an
/// `https` link, so `https://evil.example\.zoom.us/` is `evil.example`). An
/// authority with userinfo (`@`, as in `teams.microsoft.com:443@evil.example`)
/// is refused; a `:port` must be digits and is dropped; the host left must be
/// ASCII letters, digits, `-` and `.`, and is compared lowercased. `base` or
/// `*.base` hosts match a `base` entry, so `notzoom.us` is not `zoom.us`.
fn is_meeting_url(after_scheme: &str) -> bool {
    let authority_end = after_scheme
        .find(['/', '?', '#', '\\'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    let path = &after_scheme[authority_end..];
    if authority.contains('@') {
        return false;
    }
    let host = match authority.split_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        Some(_) => return false,
        None => authority,
    };
    let host = host.to_ascii_lowercase();
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
    {
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
    } else if host == "meet.google.com"
        || host == "meet.goto.com"
        || host == "chime.aws"
        || under("webex.com")
    {
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

/// Reads a string field trimmed, `None` when absent, not a string or blank.
fn non_blank_str(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
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

    fn user(raw: &str) -> Option<DirectoryUser> {
        DirectoryUser::from_json(&parse(raw).unwrap())
    }

    #[test]
    fn directory_user_from_json_reads_name_address_title_department() {
        assert_eq!(
            user(
                r#"{"displayName":" Jo Smith ","mail":" jo@contoso.com ","userPrincipalName":"jsmith@contoso.onmicrosoft.com","jobTitle":"Engineer","department":"R&D"}"#
            ),
            Some(DirectoryUser {
                name: "Jo Smith".into(),
                address: "jo@contoso.com".into(),
                job_title: Some("Engineer".into()),
                department: Some("R&D".into()),
            })
        );
    }

    #[test]
    fn directory_user_from_json_absent_null_or_blank_title_and_department_are_none() {
        for raw in [
            r#"{"displayName":"Jo","mail":"jo@contoso.com"}"#,
            r#"{"displayName":"Jo","mail":"jo@contoso.com","jobTitle":null,"department":null}"#,
            r#"{"displayName":"Jo","mail":"jo@contoso.com","jobTitle":"  ","department":""}"#,
            r#"{"displayName":"Jo","mail":"jo@contoso.com","jobTitle":7,"department":true}"#,
        ] {
            let u = user(raw).unwrap();
            assert_eq!((u.job_title, u.department), (None, None), "{raw}");
        }
    }

    #[test]
    fn directory_user_from_json_falls_back_to_an_address_like_upn() {
        for mail in ["", r#","mail":null"#, r#","mail":"  ""#] {
            let raw =
                format!(r#"{{"displayName":"Jo","userPrincipalName":"jo@contoso.com"{mail}}}"#);
            assert_eq!(user(&raw).unwrap().address, "jo@contoso.com", "{raw}");
        }
    }

    #[test]
    fn directory_user_from_json_without_an_address_is_none() {
        for raw in [
            r#"{"displayName":"Room"}"#,
            r#"{"displayName":"Room","mail":null,"userPrincipalName":null}"#,
            r#"{"displayName":"Room","userPrincipalName":"not-an-address"}"#,
            // A guest's UPN is no mailbox.
            r#"{"displayName":"Guest","userPrincipalName":"guest_fabrikam.com#EXT#@contoso.onmicrosoft.com"}"#,
        ] {
            assert_eq!(user(raw), None, "{raw}");
        }
        // A guest with a `mail` is kept by it.
        assert_eq!(
            user(r#"{"displayName":"Guest","mail":"guest@fabrikam.com","userPrincipalName":"guest_fabrikam.com#EXT#@contoso.onmicrosoft.com"}"#)
                .unwrap()
                .address,
            "guest@fabrikam.com"
        );
    }

    #[test]
    fn directory_user_from_json_without_a_name_keeps_an_empty_one() {
        assert_eq!(user(r#"{"mail":"jo@contoso.com"}"#).unwrap().name, "");
    }

    #[test]
    fn looks_like_address_cases() {
        for ok in ["jo@contoso.com", "j.o+x@mail.contoso.co.uk"] {
            assert!(looks_like_address(ok), "{ok}");
        }
        for bad in [
            "",
            "jo",
            "@contoso.com",
            "jo@",
            "jo@contoso",
            "jo@.contoso.com",
            "jo@contoso.com.",
            "jo@a@contoso.com",
            "jo smith@contoso.com",
            "jo@contoso.com\n",
            "jo_x.com#EXT#@contoso.onmicrosoft.com",
            "jo_x.com#ext#@contoso.onmicrosoft.com",
        ] {
            assert!(!looks_like_address(bad), "{bad:?}");
        }
    }

    #[test]
    fn search_hit_from_json_carries_folder_and_delta_fields() {
        let v = parse(
            r#"{"id":"M1","parentFolderId":"F2","subject":"Budget","isRead":false,"receivedDateTime":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let hit = SearchHit::from_json(&v).unwrap();
        assert_eq!(hit.folder_id, "F2");
        assert_eq!(hit.message, Message::from_json(&v).unwrap());
        assert_eq!(hit.fields, MessageFields::of(&v));
        assert!(hit.fields.subject && !hit.fields.from, "{:?}", hit.fields);
    }

    #[test]
    fn search_hit_from_json_without_parent_folder_has_an_empty_folder() {
        let v = parse(r#"{"id":"M1","parentFolderId":null}"#).unwrap();
        assert_eq!(SearchHit::from_json(&v).unwrap().folder_id, "");
        let v = parse(r#"{"id":"M1"}"#).unwrap();
        assert_eq!(SearchHit::from_json(&v).unwrap().folder_id, "");
    }

    #[test]
    fn search_hit_from_json_without_an_id_is_none() {
        for raw in [r#"{"subject":"x"}"#, r#"{"id":"","subject":"x"}"#] {
            assert_eq!(SearchHit::from_json(&parse(raw).unwrap()), None, "{raw}");
        }
    }

    #[test]
    fn meeting_message_type_maps_graph_values_and_is_lenient() {
        use MeetingMessageType as T;
        let cases = [
            ("meetingRequest", Some(T::Request)),
            ("meetingCancelled", Some(T::Cancelled)),
            ("meetingAccepted", Some(T::Accepted)),
            // Graph v1.0's own spelling, and the corrected one.
            ("meetingTenativelyAccepted", Some(T::TentativelyAccepted)),
            ("meetingTentativelyAccepted", Some(T::TentativelyAccepted)),
            ("meetingDeclined", Some(T::Declined)),
            ("none", None),
            ("", None),
            ("meetingSomethingNew", None),
            ("MeetingCancelled", None),
        ];
        for (wire, want) in cases {
            assert_eq!(MeetingMessageType::from_graph(wire), want, "{wire:?}");
        }
    }

    #[test]
    fn meeting_message_type_round_trips_its_graph_name() {
        use MeetingMessageType as T;
        for t in [
            T::Request,
            T::Cancelled,
            T::Accepted,
            T::TentativelyAccepted,
            T::Declined,
        ] {
            assert_eq!(MeetingMessageType::from_graph(t.as_graph()), Some(t));
        }
    }

    #[test]
    fn meeting_message_type_from_message_class() {
        use MeetingMessageType as T;
        let cases = [
            ("IPM.Schedule.Meeting.Request", Some(T::Request)),
            ("IPM.Schedule.Meeting.Canceled", Some(T::Cancelled)),
            ("IPM.Schedule.Meeting.Resp.Pos", Some(T::Accepted)),
            (
                "IPM.Schedule.Meeting.Resp.Tent",
                Some(T::TentativelyAccepted),
            ),
            ("IPM.Schedule.Meeting.Resp.Neg", Some(T::Declined)),
            // MAPI compares classes ignoring ASCII case.
            ("ipm.schedule.meeting.request", Some(T::Request)),
            ("IPM.SCHEDULE.MEETING.RESP.NEG", Some(T::Declined)),
            // A subclass keeps its base's type.
            ("IPM.Schedule.Meeting.Request.x", Some(T::Request)),
            ("IPM.Schedule.Meeting.Canceled.SMIME", Some(T::Cancelled)),
            // Anything else is no meeting message type.
            ("IPM.Note", None),
            ("IPM.Schedule.Meeting.Notification.Forward", None),
            ("IPM.Schedule.Meeting.RequestX", None),
            ("IPM.Schedule.Meeting.Request.", None),
            ("IPM.Schedule.Meeting", None),
            ("IPM.Schedule.Meeting.Resp", None),
            ("", None),
        ];
        for (class, want) in cases {
            assert_eq!(
                MeetingMessageType::from_message_class(class),
                want,
                "{class:?}"
            );
        }
    }

    #[test]
    fn message_class_prop_id_matches_its_forms_only() {
        for id in [
            MESSAGE_CLASS_PROP,
            "String 0x001A",
            "String 0x1A",
            "string 0x1a",
            "STRING 0X001a",
        ] {
            assert!(is_message_class_prop(id), "{id:?}");
        }
        for id in [
            "Integer 0x001A",
            "String 0x001AB",
            "String 0x1B",
            "String 0x",
            "String 0x+1A",
            "String 001A",
            "String  0x1A",
            "String 0x1A ",
            "String {00062008-0000-0000-C000-000000000046} Id 0x001A",
            "",
        ] {
            assert!(!is_message_class_prop(id), "{id:?}");
        }
    }

    #[test]
    fn message_reads_meeting_type_from_class_and_top_level() {
        use MeetingMessageType as T;
        let read = |json: &str| {
            let v = parse(json).unwrap();
            let m = Message::from_json(&v).unwrap();
            let f = MessageFields::of(&v);
            (m.meeting_message_type, f.meeting_type)
        };
        let class = |id: &str, value: &str| {
            format!(
                r#"{{"id":"M1","singleValueExtendedProperties":[{{"id":"{id}","value":{value}}}]}}"#
            )
        };
        // The class, in the long, short and lower-case id forms.
        assert_eq!(
            read(&class(
                "String 0x001A",
                r#""IPM.Schedule.Meeting.Canceled""#
            )),
            (Some(T::Cancelled), true)
        );
        assert_eq!(
            read(&class("String 0x1A", r#""IPM.Schedule.Meeting.Resp.Pos""#)),
            (Some(T::Accepted), true)
        );
        assert_eq!(
            read(&class("string 0x1a", r#""IPM.Schedule.Meeting.Resp.Neg""#)),
            (Some(T::Declined), true)
        );
        // A plain class is carried, and is no type.
        assert_eq!(read(&class("String 0x001A", r#""IPM.Note""#)), (None, true));
        // A non-string value is no class, and not carried.
        assert_eq!(read(&class("String 0x001A", "42")), (None, false));
        assert_eq!(read(&class("String 0x001A", "null")), (None, false));
        // Beside the reminder properties.
        let both = r#"{"id":"M1","singleValueExtendedProperties":[
            {"id":"Boolean {00062008-0000-0000-C000-000000000046} Id 0x8503","value":"true"},
            {"id":"String 0x1a","value":"IPM.Schedule.Meeting.Request"}]}"#;
        assert_eq!(read(both), (Some(T::Request), true));
        // A top-level `meetingMessageType` wins over the class, `none` too.
        let top = r#"{"id":"M1","meetingMessageType":"meetingTenativelyAccepted",
            "singleValueExtendedProperties":[{"id":"String 0x001A","value":"IPM.Note"}]}"#;
        assert_eq!(read(top), (Some(T::TentativelyAccepted), true));
        let none = r#"{"id":"M1","meetingMessageType":"none",
            "singleValueExtendedProperties":[
              {"id":"String 0x001A","value":"IPM.Schedule.Meeting.Request"}]}"#;
        assert_eq!(read(none), (None, true));
        // A non-string top-level value falls back to the class.
        let odd = r#"{"id":"M1","meetingMessageType":7,
            "singleValueExtendedProperties":[
              {"id":"String 0x001A","value":"IPM.Schedule.Meeting.Request"}]}"#;
        assert_eq!(read(odd), (Some(T::Request), true));
        // Neither, an unknown value, or an empty array: nothing carried
        // but what the key says.
        assert_eq!(read(r#"{"id":"M1","subject":"s"}"#), (None, false));
        assert_eq!(
            read(r#"{"id":"M1","meetingMessageType":"meetingX"}"#),
            (None, true)
        );
        assert_eq!(
            read(r#"{"id":"M1","singleValueExtendedProperties":[]}"#),
            (None, false)
        );
        assert_eq!(
            read(r#"{"id":"M1","singleValueExtendedProperties":"junk"}"#),
            (None, false)
        );
    }

    #[test]
    fn writes_meeting_type_only_when_carried() {
        let fields = |json: &str| MessageFields::of(&parse(json).unwrap());
        let complete = r#""subject":"s","from":{"emailAddress":{"address":"a@x"}},
            "receivedDateTime":"2026-11-10T00:00:00Z""#;
        // A carried meeting class, or a carried `IPM.Note`, writes.
        let meeting = format!(
            r#"{{"id":"M1",{complete},"singleValueExtendedProperties":[
                {{"id":"String 0x001A","value":"IPM.Schedule.Meeting.Request"}}]}}"#
        );
        assert!(fields(&meeting).writes_meeting_type());
        let plain = format!(
            r#"{{"id":"M1",{complete},"singleValueExtendedProperties":[
                {{"id":"String 0x001A","value":"IPM.Note"}}]}}"#
        );
        assert!(fields(&plain).writes_meeting_type());
        // A complete item asked for the class that came without it keeps
        // the stored type (an expand dropped without a 400).
        let bare = fields(&format!(r#"{{"id":"M1",{complete}}}"#));
        assert!(!bare.is_partial() && !bare.reminder_unasked);
        assert!(bare.writes_reminder(), "the reminder is still written");
        assert!(!bare.writes_meeting_type());
        // A partial item, an unasked one, a non-string class: kept.
        assert!(!fields(r#"{"id":"M1","isRead":true}"#).writes_meeting_type());
        let mut unasked = bare;
        unasked.reminder_unasked = true;
        assert!(!unasked.writes_meeting_type());
        let odd = format!(
            r#"{{"id":"M1",{complete},"singleValueExtendedProperties":[
                {{"id":"String 0x001A","value":1}}]}}"#
        );
        assert!(!fields(&odd).writes_meeting_type());
        // A merge of a `Message` built in code writes it.
        assert!(MessageFields::all().writes_meeting_type());
    }

    #[test]
    fn sensitivity_from_mapi_reads_the_four_values_and_defaults_the_rest() {
        use Sensitivity as S;
        for (n, s) in [
            (0, S::Normal),
            (1, S::Personal),
            (2, S::Private),
            (3, S::Confidential),
        ] {
            assert_eq!(S::from_mapi(n), s);
            assert_eq!(s.as_mapi(), n);
        }
        for n in [-1, 4, 9, i64::MAX, i64::MIN] {
            assert_eq!(S::from_mapi(n), S::Normal, "{n}");
        }
        assert_eq!(S::default(), S::Normal);
    }

    #[test]
    fn msip_labels_reads_the_enabled_label_and_its_name() {
        const G1: &str = "f42aa342-8706-4288-bd11-ebb85995028c";
        const G2: &str = "1b2c3d4e-0000-0000-0000-000000000002";
        // Enabled, with its name and the other fields Purview writes.
        let full = format!(
            "MSIP_Label_{G1}_Enabled=true; MSIP_Label_{G1}_SetDate=2026-11-10T00:00:00Z; \
             MSIP_Label_{G1}_Method=Standard; MSIP_Label_{G1}_Name=Confidential; \
             MSIP_Label_{G1}_SiteId=72f988bf-0000-0000-0000-000000000000; \
             MSIP_Label_{G1}_ActionId=0; MSIP_Label_{G1}_ContentBits=0"
        );
        assert_eq!(parse_msip_labels(&full).as_deref(), Some("Confidential"));
        // Enabled without a name: labelled, unnamed.
        assert_eq!(
            parse_msip_labels(&format!("MSIP_Label_{G1}_Enabled=true")).as_deref(),
            Some("")
        );
        // Not enabled: no label, whatever its name.
        assert_eq!(
            parse_msip_labels(&format!(
                "MSIP_Label_{G1}_Enabled=False;MSIP_Label_{G1}_Name=Secret"
            )),
            None
        );
        // Two labels, the first disabled: the second's name.
        let two = format!(
            "MSIP_Label_{G1}_Enabled=false; MSIP_Label_{G1}_Name=Old; \
             MSIP_Label_{G2}_Name=General; MSIP_Label_{G2}_Enabled=true"
        );
        assert_eq!(parse_msip_labels(&two).as_deref(), Some("General"));
        // Two enabled: the first wins.
        let both = format!(
            "MSIP_Label_{G2}_Enabled=true; MSIP_Label_{G2}_Name=Theirs; \
             MSIP_Label_{G1}_Enabled=true; MSIP_Label_{G1}_Name=Ours"
        );
        assert_eq!(parse_msip_labels(&both).as_deref(), Some("Theirs"));
        // Case of key, field, `True` and GUID; spaces; quoted values.
        let odd = format!(
            "  msip_label_{G1}_ENABLED = \"True\" ;MSIP_LABEL_{}_name=\"Highly Confidential\"; ",
            G1.to_ascii_uppercase()
        );
        assert_eq!(
            parse_msip_labels(&odd).as_deref(),
            Some("Highly Confidential")
        );
        // Garbage, empty, a key without a GUID: no label.
        for v in [
            "",
            ";;;",
            "garbage",
            "Enabled=true",
            "MSIP_Label__Enabled=true",
            "MSIP_Label_Enabled=true",
            "MSIP_Labelx_Enabled=true",
        ] {
            assert_eq!(parse_msip_labels(v), None, "{v:?}");
        }
    }

    #[test]
    fn message_protection_reads_the_three_properties() {
        let with = |props: &str| {
            parse_message_protection(
                &parse(&format!(
                    r#"{{"id":"M1","singleValueExtendedProperties":[
                        {{"id":"String 0x001A","value":"IPM.Note"}}{props}]}}"#
                ))
                .unwrap(),
            )
        };
        // Sensitivity as a string (Graph's way) or a JSON number, by its
        // long or short id.
        for props in [
            r#",{"id":"Integer 0x0036","value":"3"}"#,
            r#",{"id":"integer 0x36","value":" 3 "}"#,
            r#",{"id":"Integer 0x36","value":3}"#,
        ] {
            let (carried, p) = with(props);
            assert!(carried);
            assert_eq!(p.sensitivity, Sensitivity::Confidential, "{props}");
        }
        for props in [
            r#",{"id":"Integer 0x0036","value":"x"}"#,
            r#",{"id":"Integer 0x0036","value":2.5}"#,
            r#",{"id":"Integer 0x0036","value":true}"#,
            r#",{"id":"String 0x0036","value":"3"}"#,
        ] {
            assert_eq!(with(props).1.sensitivity, Sensitivity::Normal, "{props}");
        }
        assert_eq!(
            with(r#",{"id":"Integer 0x0036","value":"2"}"#)
                .1
                .sensitivity,
            Sensitivity::Private
        );
        // The label, by the id Graph may echo with a lower-cased GUID.
        let (_, p) = with(
            r#",{"id":"string {00020386-0000-0000-c000-000000000046} name MSIP_LABELS",
                 "value":"MSIP_Label_a_Enabled=true;MSIP_Label_a_Name=General"}"#,
        );
        assert!(p.has_label);
        assert_eq!(p.label_name, "General");
        assert_eq!(
            with(&format!(
                r#",{{"id":"{MSIP_LABELS_PROP}","value":"MSIP_Label_a_Enabled=false"}}"#
            ))
            .1,
            MessageProtection::default()
        );
        assert_eq!(
            with(&format!(r#",{{"id":"{MSIP_LABELS_PROP}","value":7}}"#)).1,
            MessageProtection::default()
        );
        // IRM, the content class in any case; another class is not.
        for value in ["rpmsg.message", " RPMSG.Message "] {
            let (_, p) = with(&format!(
                r#",{{"id":"{CONTENT_CLASS_PROP}","value":"{value}"}}"#
            ));
            assert!(p.is_rights_protected, "{value:?}");
        }
        for value in ["urn:content-classes:message", "rpmsg.message.x", ""] {
            let (_, p) = with(&format!(
                r#",{{"id":"{CONTENT_CLASS_PROP}","value":"{value}"}}"#
            ));
            assert!(!p.is_rights_protected, "{value:?}");
        }
        // All three at once, and `Message::from_json` reads the same.
        let all = format!(
            r#"{{"id":"M1","singleValueExtendedProperties":[
                {{"id":"{SENSITIVITY_PROP}","value":"1"}},
                {{"id":"{MSIP_LABELS_PROP}","value":"MSIP_Label_a_Enabled=true"}},
                {{"id":"{CONTENT_CLASS_PROP}","value":"rpmsg.message"}},
                {{"id":"{MESSAGE_CLASS_PROP}","value":"IPM.Note"}}]}}"#
        );
        let want = MessageProtection {
            sensitivity: Sensitivity::Personal,
            has_label: true,
            label_name: String::new(),
            is_rights_protected: true,
        };
        let v = parse(&all).unwrap();
        assert_eq!(parse_message_protection(&v), (true, want.clone()));
        assert_eq!(Message::from_json(&v).unwrap().protection, want);
    }

    #[test]
    fn message_protection_is_carried_only_with_the_class() {
        let carried = |json: &str| parse_message_protection(&parse(json).unwrap());
        let complete = r#""subject":"s","from":{"emailAddress":{"address":"a@x"}},
            "receivedDateTime":"2026-11-10T00:00:00Z""#;
        // The class alone: carried, unprotected.
        let plain = format!(
            r#"{{"id":"M1",{complete},"singleValueExtendedProperties":[
                {{"id":"String 0x1a","value":"IPM.Note"}}]}}"#
        );
        assert_eq!(carried(&plain), (true, MessageProtection::default()));
        assert!(MessageFields::of(&parse(&plain).unwrap()).writes_protection());
        // No array, an array without the class (even with a label), a
        // top-level `meetingMessageType` only, a partial item: not carried,
        // and the protection read is the default.
        for json in [
            format!(r#"{{"id":"M1",{complete}}}"#),
            format!(
                r#"{{"id":"M1",{complete},"singleValueExtendedProperties":[
                    {{"id":"{MSIP_LABELS_PROP}","value":"MSIP_Label_a_Enabled=true"}},
                    {{"id":"{SENSITIVITY_PROP}","value":"3"}}]}}"#
            ),
            format!(r#"{{"id":"M1",{complete},"meetingMessageType":"meetingRequest"}}"#),
            r#"{"id":"M1","isRead":true}"#.to_string(),
            r#"{"id":"M1","singleValueExtendedProperties":"x"}"#.to_string(),
        ] {
            assert_eq!(
                carried(&json),
                (false, MessageProtection::default()),
                "{json}"
            );
            assert!(
                !MessageFields::of(&parse(&json).unwrap()).writes_protection(),
                "{json}"
            );
        }
        // A merge of a `Message` built in code writes it.
        assert!(MessageFields::all().writes_protection());
    }

    #[test]
    fn protection_terms_leave_the_reminder_rule_alone() {
        // A complete item whose array carries only the class and a label
        // (no reminder property) still writes "no reminder" (#2054's rule:
        // the array is there, so the expand ran).
        let v = parse(&format!(
            r#"{{"id":"M1","subject":"s","from":{{"emailAddress":{{"address":"a@x"}}}},
                "receivedDateTime":"2026-11-10T00:00:00Z","singleValueExtendedProperties":[
                {{"id":"String 0x001A","value":"IPM.Note"}},
                {{"id":"{MSIP_LABELS_PROP}","value":"MSIP_Label_a_Enabled=true"}}]}}"#
        ))
        .unwrap();
        let f = MessageFields::of(&v);
        assert!(f.writes_reminder() && f.writes_protection());
        let m = Message::from_json(&v).unwrap();
        assert!(!m.is_reminder_on);
        assert_eq!(
            (m.reminder_at.as_str(), m.reminder_signal_at.as_str()),
            ("", "")
        );
        assert!(m.protection.has_label);
    }

    #[test]
    fn sensitivity_prop_id_matches_its_forms_only() {
        for id in [SENSITIVITY_PROP, "Integer 0x36", "integer 0X0036"] {
            assert!(is_tag_prop(id, "Integer", 0x36), "{id:?}");
        }
        for id in [
            "String 0x0036",
            "Integer 0x0037",
            "Integer  0x36",
            "Integer0x36",
            "Integer 36",
            "Integer 0x36 ",
            "",
        ] {
            assert!(!is_tag_prop(id, "Integer", 0x36), "{id:?}");
        }
    }

    #[test]
    fn meeting_message_reads_type_and_event() {
        let v = parse(
            r#"{"id":"M1","meetingMessageType":"meetingCancelled",
                "event":{"id":"E9","isOrganizer":false}}"#,
        )
        .unwrap();
        assert_eq!(
            MeetingMessage::from_json(&v),
            MeetingMessage {
                message_type: Some(MeetingMessageType::Cancelled),
                event_id: Some("E9".into()),
                is_organizer: false,
            }
        );
        let v = parse(
            r#"{"meetingMessageType":"meetingRequest","event":{"id":"E1","isOrganizer":true}}"#,
        )
        .unwrap();
        assert!(MeetingMessage::from_json(&v).is_organizer);
        // An event without `isOrganizer` is not the organizer's.
        let v = parse(r#"{"meetingMessageType":"meetingAccepted","event":{"id":"E2"}}"#).unwrap();
        let m = MeetingMessage::from_json(&v);
        assert_eq!(m.message_type, Some(MeetingMessageType::Accepted));
        assert_eq!(m.event_id.as_deref(), Some("E2"));
        assert!(!m.is_organizer);
        // A plain message, a `none` one, an unknown type, a null event and
        // an empty event id: all lenient.
        for body in [
            r#"{"id":"M2"}"#,
            r#"{"meetingMessageType":"none"}"#,
            r#"{"meetingMessageType":"other","event":null}"#,
            r#"{"meetingMessageType":7,"event":{"id":""}}"#,
        ] {
            let v = parse(body).unwrap();
            assert_eq!(
                MeetingMessage::from_json(&v),
                MeetingMessage::default(),
                "{body}"
            );
        }
    }

    #[test]
    fn msip_labels_is_a_stored_header() {
        // #2015: the sensitivity label is kept with a body, in any case.
        for name in ["msip_labels", "MSIP_LABELS", "Msip_Labels"] {
            assert!(is_stored_header(name), "{name}");
        }
        assert!(!is_stored_header("msip_label"));
        assert!(!is_stored_header("Received"));
    }

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

    /// An event JSON with `extra` spliced in after its times (#2068).
    fn event_with(extra: &str) -> Event {
        let v = parse(&format!(
            r#"{{"id":"E1","subject":"Sync",
                "start":{{"dateTime":"2026-08-20T09:00:00.0000000","timeZone":"UTC"}},
                "end":{{"dateTime":"2026-08-20T10:00:00.0000000","timeZone":"UTC"}}{extra}}}"#
        ))
        .unwrap();
        Event::from_json(&v).unwrap()
    }

    #[test]
    fn event_own_response_time_and_created_utc() {
        let e = event_with(
            r#","responseStatus":{"response":"accepted","time":"2026-08-13T19:19:00.1234567Z"},
               "createdDateTime":"2026-08-01T08:00:05.52Z""#,
        );
        assert_eq!(e.response_status, "accepted");
        assert_eq!(e.response_time, "2026-08-13T19:19:00Z");
        assert_eq!(e.created_utc, "2026-08-01T08:00:05Z");
        // Without a fraction, and without the `Z` (read as UTC).
        let e = event_with(
            r#","responseStatus":{"response":"declined","time":"2026-08-13T19:19:00Z"},
               "createdDateTime":"2026-08-01T08:00:05""#,
        );
        assert_eq!(e.response_time, "2026-08-13T19:19:00Z");
        assert_eq!(e.created_utc, "2026-08-01T08:00:05Z");
    }

    #[test]
    fn event_response_time_never_and_absent_are_empty() {
        for never in ["0001-01-01T00:00:00Z", "0001-01-01T00:00:00.0000000Z"] {
            let e = event_with(&format!(
                r#","responseStatus":{{"response":"none","time":"{never}"}},
                   "createdDateTime":"{never}""#
            ));
            assert_eq!(e.response_time, "", "{never}");
            assert_eq!(e.created_utc, "", "{never}");
        }
        for extra in [
            "",
            r#","responseStatus":{"response":"none"}"#,
            r#","responseStatus":{"response":"none","time":null},"createdDateTime":null"#,
            r#","responseStatus":{"response":"none","time":5},"createdDateTime":7"#,
            r#","responseStatus":"accepted""#,
            r#","responseStatus":{"time":"2026-08-13T21:19:00+02:00"},
               "createdDateTime":"2026-08-01T10:00:05+02:00""#,
            r#","responseStatus":{"time":"soon"},"createdDateTime":"2026-08-01""#,
        ] {
            let e = event_with(extra);
            assert_eq!(e.response_time, "", "{extra}");
            assert_eq!(e.created_utc, "", "{extra}");
        }
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
    fn event_delta_page_reads_upserts_removed_and_links() {
        let v = parse(
            r#"{
          "value":[
            {"id":"E1","subject":"Standup","start":{"dateTime":"2026-07-18T09:00:00.0000000","timeZone":"UTC"},"end":{"dateTime":"2026-07-18T09:30:00.0000000","timeZone":"UTC"}},
            {"id":"E2","@removed":{"reason":"deleted"}},
            {"@removed":{"reason":"deleted"}}
          ],
          "@odata.deltaLink":"https://graph.example/delta?$deltatoken=T"
        }"#,
        )
        .unwrap();
        let page = EventDeltaPage::from_json(&v).unwrap();
        assert_eq!(page.items.len(), 2);
        let EventDeltaItem::Upsert(e) = &page.items[0] else {
            panic!("an upsert first")
        };
        assert_eq!(
            (e.id.as_str(), e.subject.as_str(), e.start_utc.as_str()),
            ("E1", "Standup", "2026-07-18T09:00:00Z")
        );
        assert_eq!(page.items[1], EventDeltaItem::Removed("E2".to_string()));
        assert_eq!(page.next_link, None);
        assert_eq!(
            page.delta_link.as_deref(),
            Some("https://graph.example/delta?$deltatoken=T")
        );
    }

    #[test]
    fn event_delta_page_without_value_is_none() {
        let v = parse(r#"{"@odata.deltaLink":"x"}"#).unwrap();
        assert!(EventDeltaPage::from_json(&v).is_none());
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

    #[test]
    fn calendar_reads_graph_fields() {
        let c = Calendar::from_json(
            &parse(
                r##"{"id":"C1","name":"Work","color":"lightBlue","hexColor":"#3366ff",
                    "isDefaultCalendar":true,"canEdit":true,
                    "owner":{"name":"Ann","address":"ann@x.com"}}"##,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            c,
            Calendar {
                id: "C1".into(),
                name: "Work".into(),
                color: "lightBlue".into(),
                hex_color: "#3366ff".into(),
                is_default: true,
                can_edit: true,
                owner_name: "Ann".into(),
                owner_addr: "ann@x.com".into(),
            }
        );
        // Missing fields default; no id, or an empty one, is skipped.
        let bare = Calendar::from_json(&parse(r#"{"id":"C2"}"#).unwrap()).unwrap();
        assert_eq!(
            bare,
            Calendar {
                id: "C2".into(),
                ..Calendar::default()
            }
        );
        assert!(Calendar::from_json(&parse(r#"{"name":"x"}"#).unwrap()).is_none());
        assert!(Calendar::from_json(&parse(r#"{"id":""}"#).unwrap()).is_none());
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
            // Userinfo after a port colon: the browser opens evil.example.
            "https://teams.microsoft.com:443@evil.example/l/meetup-join/1",
            "https://zoom.us:x@evil.example/j/1",
            // A backslash is a `/` to the browser: the host is evil.example.
            "https://evil.example\\\\.zoom.us/j/1",
            "https://evil.example\\\\.webex.com/meet/x",
            // A port that is not digits, a host with other characters.
            "https://zoom.us:44x/j/1",
            "https://zo%6Fm.us/j/1",
            // A meeting link nested in another link's query or path.
            "https://evil.example/?u=https://zoom.us/j/1",
            "https://evil.example/r/https://teams.microsoft.com/l/meetup-join/1",
        ] {
            let e = ev(&format!(r#","location":{{"displayName":"{text}"}}"#));
            assert_eq!(e.join_url, "", "{text}");
        }
        // A real link after a rejected one is still found.
        let e = ev(r#","location":{"displayName":"https://notzoom.us/j/1 https://zoom.us/j/2"}"#);
        assert_eq!(e.join_url, "https://zoom.us/j/2");
        let e = ev(
            r#","location":{"displayName":"https://teams.microsoft.com:443@evil.example/l/meetup-join/1 https://teams.microsoft.com:443/l/meetup-join/2"}"#,
        );
        assert_eq!(
            e.join_url,
            "https://teams.microsoft.com:443/l/meetup-join/2"
        );
    }

    #[test]
    fn event_join_url_ends_at_escaped_brackets_and_quotes() {
        for (body, want) in [
            (
                "Join: &lt;https://zoom.us/j/7&gt; now",
                "https://zoom.us/j/7",
            ),
            ("&quot;https://zoom.us/j/8&quot;", "https://zoom.us/j/8"),
            ("&#39;https://zoom.us/j/9&#39;", "https://zoom.us/j/9"),
            ("&#x27;https://zoom.us/j/10&#x27;", "https://zoom.us/j/10"),
            (
                "https://zoom.us/j/11?a=1&amp;b=2&gt;",
                "https://zoom.us/j/11?a=1&b=2",
            ),
            // Other named and numeric entities end a link too.
            ("Join: https://zoom.us/j/1&nbsp;</p>", "https://zoom.us/j/1"),
            (
                "https://aka.ms/x&nbsp;https://zoom.us/j/1",
                "https://zoom.us/j/1",
            ),
            ("&#34;https://zoom.us/j/2&#34;", "https://zoom.us/j/2"),
            ("https://zoom.us/j/3&#xa0;more", "https://zoom.us/j/3"),
            // A raw & not shaped like an entity stays in the link.
            (
                "https://zoom.us/j/4?pwd=x&y=1 next",
                "https://zoom.us/j/4?pwd=x&y=1",
            ),
            ("https://zoom.us/j/5?a&b;c", "https://zoom.us/j/5?a"),
        ] {
            let e = ev(&format!(r#","body":{{"content":"{body}"}}"#));
            assert_eq!(e.join_url, want, "{body}");
        }
    }

    /// A sender-controlled body of `https://` with no whitespace is scanned
    /// once, not once per `https://` (that took minutes at this size); a
    /// link after the run is still found. Finishing is the check.
    #[test]
    fn event_join_url_scan_is_linear_on_a_long_scheme_run() {
        let run = "https://".repeat(200_000);
        assert_eq!(meeting_link(&run), None);
        let text = format!("{run} https://zoom.us/j/1");
        assert_eq!(meeting_link(&text).as_deref(), Some("https://zoom.us/j/1"));
        let e = ev(&format!(r#","body":{{"content":"{run}"}}"#));
        assert_eq!(e.join_url, "");
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
    fn event_recurrence_send_json_drops_what_graph_refuses() {
        let rec = |json: &str| {
            EventRecurrence::from_json(&parse(json).unwrap())
                .unwrap()
                .to_send_json()
                .to_string()
        };
        // noEnd: Graph's never date and zeros are not sent; a monday week
        // stays monday, the unused `index` filler is kept (Graph ignores it).
        assert_eq!(
            rec(
                r#"{"pattern":{"type":"weekly","interval":1,"month":0,"dayOfMonth":0,
                "daysOfWeek":["monday","wednesday"],"firstDayOfWeek":"monday","index":"first"},
                "range":{"type":"noEnd","startDate":"2026-07-13","endDate":"0001-01-01",
                "recurrenceTimeZone":"","numberOfOccurrences":0}}"#
            ),
            r#"{"pattern":{"type":"weekly","interval":1,"daysOfWeek":["monday","wednesday"],"firstDayOfWeek":"monday","index":"first"},"range":{"type":"noEnd","startDate":"2026-07-13"}}"#
        );
        // numbered: the count, never an end date.
        assert_eq!(
            rec(
                r#"{"pattern":{"type":"relativeMonthly","interval":2,"month":0,"dayOfMonth":0,
                "daysOfWeek":["tuesday"],"firstDayOfWeek":"sunday","index":"second"},
                "range":{"type":"numbered","startDate":"2026-07-14","endDate":"0001-01-01",
                "recurrenceTimeZone":"W. Europe Standard Time","numberOfOccurrences":6}}"#
            ),
            r#"{"pattern":{"type":"relativeMonthly","interval":2,"daysOfWeek":["tuesday"],"firstDayOfWeek":"sunday","index":"second"},"range":{"type":"numbered","startDate":"2026-07-14","numberOfOccurrences":6,"recurrenceTimeZone":"W. Europe Standard Time"}}"#
        );
        // endDate: the end date, no count; a yearly month and day.
        assert_eq!(
            rec(
                r#"{"pattern":{"type":"absoluteYearly","interval":1,"month":3,"dayOfMonth":17},
                "range":{"type":"endDate","startDate":"2026-03-17","endDate":"2030-03-17",
                "numberOfOccurrences":0}}"#
            ),
            r#"{"pattern":{"type":"absoluteYearly","interval":1,"month":3,"dayOfMonth":17},"range":{"type":"endDate","startDate":"2026-03-17","endDate":"2030-03-17"}}"#
        );
        // An unknown type is sent as is.
        assert_eq!(
            rec(r#"{"pattern":{"type":"fortnightlyish","interval":1},
                "range":{"type":"someday","startDate":"2026-01-01"}}"#),
            r#"{"pattern":{"type":"fortnightlyish","interval":1},"range":{"type":"someday","startDate":"2026-01-01"}}"#
        );
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

    /// The organizer's view of a proposal (#2023): a full slot reads as
    /// canonical UTC; null, absent and half slots read as no proposal.
    #[test]
    fn attendee_proposed_new_time() {
        let e = ev(r#","attendees":[
            {"type":"required","status":{"response":"tentativelyAccepted"},
             "proposedNewTime":{
               "start":{"dateTime":"2026-07-21T14:00:00.0000000","timeZone":"UTC"},
               "end":{"dateTime":"2026-07-21T15:30:00.0000000","timeZone":"UTC"}},
             "emailAddress":{"name":"Full","address":"full@x"}},
            {"type":"required","status":{"response":"accepted"},"proposedNewTime":null,
             "emailAddress":{"name":"Null","address":"null@x"}},
            {"type":"required","status":{"response":"accepted"},
             "emailAddress":{"name":"Absent","address":"absent@x"}},
            {"type":"required","status":{"response":"declined"},
             "proposedNewTime":{
               "start":{"dateTime":"2026-07-21T14:00:00.0000000","timeZone":"UTC"},
               "end":null},
             "emailAddress":{"name":"Half","address":"half@x"}}]"#);
        let slot = |i: usize| {
            (
                e.attendees[i].proposed_start_utc.as_str(),
                e.attendees[i].proposed_end_utc.as_str(),
            )
        };
        assert_eq!(slot(0), ("2026-07-21T14:00:00Z", "2026-07-21T15:30:00Z"));
        assert_eq!(slot(1), ("", ""), "null");
        assert_eq!(slot(2), ("", ""), "absent");
        assert_eq!(slot(3), ("", ""), "half a slot is no proposal");
    }

    // --- A flagged message's reminder (#2054) ------------------------------

    /// A complete flagged message carrying `props` as its
    /// `singleValueExtendedProperties` (raw JSON), or none when `None`.
    fn flagged_with(props: Option<&str>) -> Value {
        let props = props
            .map(|p| format!(r#","singleValueExtendedProperties":{p}"#))
            .unwrap_or_default();
        parse(&format!(
            r#"{{"id":"M1","subject":"s","from":{{"emailAddress":{{"address":"a@x"}}}},
                "receivedDateTime":"2026-11-01T00:00:00Z",
                "flag":{{"flagStatus":"flagged"}}{props}}}"#
        ))
        .unwrap()
    }

    fn prop(id: &str, value: &str) -> String {
        format!(r#"{{"id":"{id}","value":{value}}}"#)
    }

    fn three(set: &str, at: &str, signal: &str) -> String {
        format!(
            "[{},{},{}]",
            prop(REMINDER_SET_PROP, set),
            prop(REMINDER_TIME_PROP, at),
            prop(REMINDER_SIGNAL_TIME_PROP, signal)
        )
    }

    #[test]
    fn message_reminder_parses_the_three_properties() {
        let v = flagged_with(Some(&three(
            r#""true""#,
            r#""2026-11-08T10:00:00Z""#,
            r#""2026-11-08T10:15:00.0000000Z""#,
        )));
        let m = Message::from_json(&v).unwrap();
        assert!(m.is_reminder_on);
        assert_eq!(m.reminder_at, "2026-11-08T10:00:00Z");
        assert_eq!(m.reminder_signal_at, "2026-11-08T10:15:00Z");
        assert!(MessageFields::of(&v).reminder);

        // Graph may lower-case the GUID; a JSON boolean and "TRUE" are on too.
        let lower = |id: &str| id.to_ascii_lowercase();
        let props = format!(
            "[{},{}]",
            prop(&lower(REMINDER_SET_PROP), "true"),
            prop(
                &lower(REMINDER_SIGNAL_TIME_PROP),
                r#""2026-11-08T10:15:00Z""#
            )
        );
        let m = Message::from_json(&flagged_with(Some(&props))).unwrap();
        assert!(m.is_reminder_on);
        assert_eq!(m.reminder_at, "");
        assert_eq!(m.reminder_signal_at, "2026-11-08T10:15:00Z");
        let props = format!(
            "[{},{}]",
            prop(REMINDER_SET_PROP, r#""TRUE""#),
            prop(REMINDER_TIME_PROP, r#""2026-11-08T10:00:00Z""#)
        );
        assert!(
            Message::from_json(&flagged_with(Some(&props)))
                .unwrap()
                .is_reminder_on
        );
    }

    #[test]
    fn message_without_reminder_properties_has_none() {
        for props in [
            None,
            Some("[]"),
            Some("null"),
            Some("{}"),
            Some(r#"["x",3]"#),
        ] {
            let v = flagged_with(props);
            let m = Message::from_json(&v).unwrap();
            assert!(!m.is_reminder_on, "{props:?}");
            assert_eq!(m.reminder_at, "", "{props:?}");
            assert_eq!(m.reminder_signal_at, "", "{props:?}");
        }
        assert!(!MessageFields::of(&flagged_with(None)).reminder);
        assert!(MessageFields::of(&flagged_with(Some("[]"))).reminder);
        // Another extended property is not the reminder.
        let other = format!(
            "[{}]",
            prop(
                "Boolean {00062008-0000-0000-C000-000000000046} Id 0x8504",
                r#""true""#
            )
        );
        assert!(
            !Message::from_json(&flagged_with(Some(&other)))
                .unwrap()
                .is_reminder_on
        );
    }

    #[test]
    fn message_reminder_false_is_off() {
        for set in [
            r#""false""#,
            "false",
            r#""yes""#,
            r#""1""#,
            "1",
            "null",
            r#""""#,
        ] {
            let v = flagged_with(Some(&three(
                set,
                r#""2026-11-08T10:00:00Z""#,
                r#""2026-11-08T10:15:00Z""#,
            )));
            let m = Message::from_json(&v).unwrap();
            assert!(!m.is_reminder_on, "{set}");
            // The times are still read: a client may show them.
            assert_eq!(m.reminder_signal_at, "2026-11-08T10:15:00Z", "{set}");
        }
    }

    #[test]
    fn message_reminder_with_garbage_times_is_off() {
        for t in [
            r#""garbage""#,
            r#""""#,
            "null",
            "12",
            r#""2026-13-08T10:00:00Z""#,
            r#""2026-02-30T10:00:00Z""#,
            r#""2026-11-08""#,
            r#""0000-00-00T00:00:00Z""#,
        ] {
            let v = flagged_with(Some(&three(r#""true""#, t, t)));
            let m = Message::from_json(&v).expect("never None");
            assert!(!m.is_reminder_on, "{t}");
            assert_eq!(m.reminder_at, "", "{t}");
            assert_eq!(m.reminder_signal_at, "", "{t}");
        }
        // One readable time is enough.
        let v = flagged_with(Some(&three(
            r#""true""#,
            r#""garbage""#,
            r#""2026-11-08T10:15:00Z""#,
        )));
        let m = Message::from_json(&v).unwrap();
        assert!(m.is_reminder_on);
        assert_eq!(m.reminder_at, "");
    }

    #[test]
    fn message_reminder_with_an_offset_time_is_unreadable() {
        // `to_utc` would drop the offset without applying it: the wrong time.
        for t in [
            r#""2026-11-08T10:15:00+02:00""#,
            r#""2026-11-08T10:15:00-05:00""#,
        ] {
            let v = flagged_with(Some(&three(r#""true""#, t, t)));
            let m = Message::from_json(&v).unwrap();
            assert!(!m.is_reminder_on, "{t}");
            assert_eq!(m.reminder_signal_at, "", "{t}");
        }
        // No suffix reads as UTC; Outlook's "never" sentinel parses.
        let v = flagged_with(Some(&three(
            r#""true""#,
            r#""2026-11-08T10:00:00""#,
            r#""4501-01-01T00:00:00Z""#,
        )));
        let m = Message::from_json(&v).unwrap();
        assert_eq!(m.reminder_at, "2026-11-08T10:00:00Z");
        assert_eq!(m.reminder_signal_at, "4501-01-01T00:00:00Z");
    }

    #[test]
    fn flag_due_date_time_is_normalised_utc() {
        for zone in [r#""UTC""#, r#""utc""#, r#""Etc/UTC""#, r#""""#] {
            let v = parse(&format!(
                r#"{{"id":"M1","flag":{{"flagStatus":"flagged",
                    "dueDateTime":{{"dateTime":"2026-11-09T17:00:00.0000000","timeZone":{zone}}}}}}}"#
            ))
            .unwrap();
            assert_eq!(
                Message::from_json(&v).unwrap().flag_due_at,
                "2026-11-09T17:00:00Z",
                "{zone}"
            );
        }
        let v = parse(r#"{"id":"M1","flag":{"flagStatus":"flagged"}}"#).unwrap();
        assert_eq!(Message::from_json(&v).unwrap().flag_due_at, "");
        let v = parse(r#"{"id":"M1","flag":{"dueDateTime":{"dateTime":"soon","timeZone":"UTC"}}}"#)
            .unwrap();
        assert_eq!(Message::from_json(&v).unwrap().flag_due_at, "");
    }

    #[test]
    fn flag_due_in_a_named_zone_is_empty() {
        let v = parse(
            r#"{"id":"M1","flag":{"flagStatus":"flagged",
                "dueDateTime":{"dateTime":"2026-11-09T17:00:00","timeZone":"Pacific Standard Time"}}}"#,
        )
        .unwrap();
        assert_eq!(Message::from_json(&v).unwrap().flag_due_at, "");
    }

    #[test]
    fn message_fields_reminder_written_only_when_carried_or_complete() {
        // A complete item without the key: written (no reminder).
        assert!(MessageFields::of(&flagged_with(None)).writes_reminder());
        // A partial item without it: kept.
        let partial = parse(r#"{"id":"M1","isRead":true}"#).unwrap();
        assert!(!MessageFields::of(&partial).writes_reminder());
        // A partial item with it: written.
        let partial = parse(&format!(
            r#"{{"id":"M1","singleValueExtendedProperties":{}}}"#,
            three(r#""false""#, "null", "null")
        ))
        .unwrap();
        assert!(MessageFields::of(&partial).writes_reminder());
        assert!(MessageFields::all().reminder);
        // Read without asking (#2054: the expand refused): a complete item
        // without the key keeps the stored reminder; one with it writes.
        let mut unasked = MessageFields::of(&flagged_with(None));
        unasked.reminder_unasked = true;
        assert!(!unasked.writes_reminder());
        let mut unasked = MessageFields::of(&flagged_with(Some("[]")));
        unasked.reminder_unasked = true;
        assert!(unasked.writes_reminder());
        assert!(!MessageFields::of(&flagged_with(None)).reminder_unasked);
        assert!(!MessageFields::all().reminder_unasked);
    }

    /// A message as a delta round brings it (#2089): the header keys, an
    /// optional `@odata.type`, and `props` as its extended properties
    /// array (none when `None`).
    fn delta_item(odata: Option<&str>, props: Option<&str>) -> Value {
        let odata = odata
            .map(|t| format!(r#""@odata.type":"{t}","#))
            .unwrap_or_default();
        let props = props
            .map(|p| format!(r#","singleValueExtendedProperties":{p}"#))
            .unwrap_or_default();
        parse(&format!(
            r#"{{{odata}"id":"M1","subject":"s","from":{{"emailAddress":{{"address":"a@x"}}}},
                "receivedDateTime":"2026-11-01T00:00:00Z"{props}}}"#
        ))
        .unwrap()
    }

    fn class(c: &str) -> String {
        prop(MESSAGE_CLASS_PROP, &format!(r#""{c}""#))
    }

    #[test]
    fn meeting_reminder_reads_the_set_bit_of_a_meeting_message() {
        let req = Some("#microsoft.graph.eventMessageRequest");
        let set = |v: &str| prop(REMINDER_SET_PROP, v);
        // Each meeting class, with the bit and no reminder time.
        for c in [
            "IPM.Schedule.Meeting.Request",
            "IPM.Schedule.Meeting.Canceled",
            "IPM.Schedule.Meeting.Resp.Pos",
            "IPM.Schedule.Meeting.Resp.Tent",
            "IPM.Schedule.Meeting.Resp.Neg",
            "IPM.Schedule.Meeting.Notification.Forward",
            "ipm.schedule.meeting.request",
            "IPM.Schedule.Meeting",
        ] {
            let v = delta_item(None, Some(&format!("[{},{}]", class(c), set(r#""true""#))));
            assert_eq!(parse_meeting_reminder(&v), (true, true), "{c}");
            assert!(Message::from_json(&v).unwrap().meeting_reminder_on, "{c}");
        }
        // The bit as Graph may send it: any case, a JSON boolean, a
        // lower-cased GUID; with the type annotation too.
        for bit in [
            set(r#""TRUE""#),
            set("true"),
            prop(&REMINDER_SET_PROP.to_ascii_lowercase(), r#""true""#),
        ] {
            let v = delta_item(
                req,
                Some(&format!(
                    "[{},{bit}]",
                    class("IPM.Schedule.Meeting.Request")
                )),
            );
            assert_eq!(parse_meeting_reminder(&v), (true, true), "{bit}");
        }
        // Off: "false", false, another value, no bit at all.
        for bits in [
            format!(",{}", set(r#""false""#)),
            format!(",{}", set("false")),
            format!(",{}", set(r#""yes""#)),
            format!(",{}", set("null")),
            String::new(),
        ] {
            let v = delta_item(
                req,
                Some(&format!(
                    "[{}{bits}]",
                    class("IPM.Schedule.Meeting.Request")
                )),
            );
            assert_eq!(parse_meeting_reminder(&v), (true, false), "{bits}");
            assert!(!Message::from_json(&v).unwrap().meeting_reminder_on);
        }
    }

    #[test]
    fn meeting_reminder_takes_the_type_when_the_class_has_no_string() {
        // The class entry is there (so the expand ran) but its value is not
        // a string: the `@odata.type` says it is a meeting message.
        let props = format!(
            "[{},{}]",
            prop(MESSAGE_CLASS_PROP, "null"),
            prop(REMINDER_SET_PROP, r#""true""#)
        );
        for t in [
            "#microsoft.graph.eventMessage",
            "#microsoft.graph.eventMessageRequest",
            "#microsoft.graph.eventMessageResponse",
        ] {
            let v = delta_item(Some(t), Some(&props));
            assert_eq!(parse_meeting_reminder(&v), (true, true), "{t}");
        }
        // Without the type it is not a meeting message.
        let v = delta_item(None, Some(&props));
        assert_eq!(parse_meeting_reminder(&v), (true, false));
        let v = delta_item(Some("#microsoft.graph.message"), Some(&props));
        assert_eq!(parse_meeting_reminder(&v), (true, false));
        // The type wins over a string class that is not a meeting class:
        // the two halves are ORed.
        for c in ["IPM.Note", "IPM.Schedule.MeetingX"] {
            let props = format!("[{},{}]", class(c), prop(REMINDER_SET_PROP, r#""true""#));
            let v = delta_item(Some("#microsoft.graph.eventMessageRequest"), Some(&props));
            assert_eq!(parse_meeting_reminder(&v), (true, true), "{c}");
            assert!(Message::from_json(&v).unwrap().meeting_reminder_on, "{c}");
        }
    }

    #[test]
    fn meeting_reminder_is_false_on_a_plain_message() {
        // An unflagged IPM.Note with the bit and readable times: no meeting
        // reminder, and no flag reminder either, as before.
        let set = format!(
            "{},{},{}",
            prop(REMINDER_SET_PROP, r#""true""#),
            prop(REMINDER_TIME_PROP, r#""2026-11-08T10:00:00Z""#),
            prop(REMINDER_SIGNAL_TIME_PROP, r#""2026-11-08T10:00:00Z""#)
        );
        for c in [
            "IPM.Note",
            "IPM.Schedule.Meeting.",
            "IPM.Schedule.MeetingX",
            "IPM.Schedule.Meetin",
            "IPM.Appointment",
            " IPM.Schedule.Meeting.Request",
            "",
        ] {
            let v = delta_item(None, Some(&format!("[{},{set}]", class(c))));
            assert_eq!(parse_meeting_reminder(&v), (true, false), "{c:?}");
            let m = Message::from_json(&v).unwrap();
            assert!(!m.meeting_reminder_on, "{c:?}");
            assert!(m.is_reminder_on, "#2054 reads the bit as before: {c:?}");
        }
    }

    #[test]
    fn meeting_reminder_is_carried_only_with_the_class() {
        let bit = prop(REMINDER_SET_PROP, r#""true""#);
        let req = Some("#microsoft.graph.eventMessageRequest");
        // The class alone: carried, off.
        let v = delta_item(req, Some(&format!("[{}]", class("IPM.Note"))));
        assert_eq!(parse_meeting_reminder(&v), (true, false));
        assert!(MessageFields::of(&v).writes_meeting_reminder());
        // No array, an array without the class (even with the bit and a
        // meeting type annotation), a top-level `meetingMessageType` only,
        // a partial item, a non-array: not carried, and off.
        let top = parse(
            r#"{"id":"M1","subject":"s","receivedDateTime":"2026-11-01T00:00:00Z",
                "from":{"emailAddress":{"address":"a@x"}},"meetingMessageType":"meetingRequest"}"#,
        )
        .unwrap();
        for v in [
            delta_item(req, None),
            delta_item(req, Some(&format!("[{bit}]"))),
            delta_item(req, Some(r#""x""#)),
            top,
            parse(r#"{"id":"M1","isRead":true}"#).unwrap(),
        ] {
            assert_eq!(parse_meeting_reminder(&v), (false, false), "{v}");
            assert!(!MessageFields::of(&v).writes_meeting_reminder(), "{v}");
            assert!(!Message::from_json(&v).unwrap().meeting_reminder_on);
        }
        // A merge of a `Message` built in code writes it.
        assert!(MessageFields::all().writes_meeting_reminder());
    }

    #[test]
    fn meeting_reminder_is_carried_exactly_when_the_protection_is() {
        // Both rest on the class proving the expand ran; a change to one
        // rule must show here.
        let bit = prop(REMINDER_SET_PROP, r#""true""#);
        for v in [
            delta_item(None, None),
            delta_item(None, Some("[]")),
            delta_item(None, Some(&format!("[{bit}]"))),
            delta_item(None, Some(&format!("[{}]", class("IPM.Note")))),
            delta_item(
                None,
                Some(&format!("[{},{bit}]", prop("String 0x1a", "null"))),
            ),
            delta_item(
                None,
                Some(&format!("[{}]", prop("Integer 0x001A", r#""IPM.Note""#))),
            ),
            parse(r#"{"id":"M1","isRead":true}"#).unwrap(),
        ] {
            let f = MessageFields::of(&v);
            assert_eq!(f.meeting_reminder, f.protection, "{v}");
            assert_eq!(
                parse_meeting_reminder(&v).0,
                parse_message_protection(&v).0,
                "{v}"
            );
        }
    }

    #[test]
    fn meeting_reminder_leaves_the_flag_reminder_alone() {
        // An unflagged meeting request with the bit and a time: the flag
        // reminder reads as #2054 reads it (the parse has no flag term; the
        // store's merge ANDs the flag), and `MessageFields` writes the
        // reminder columns as before.
        let props = format!(
            "[{},{},{}]",
            class("IPM.Schedule.Meeting.Request"),
            prop(REMINDER_SET_PROP, r#""true""#),
            prop(REMINDER_TIME_PROP, r#""2026-11-08T10:00:00Z""#)
        );
        let v = delta_item(Some("#microsoft.graph.eventMessageRequest"), Some(&props));
        assert_eq!(
            parse_message_reminder(&v),
            (true, "2026-11-08T10:00:00Z".to_string(), String::new())
        );
        let m = Message::from_json(&v).unwrap();
        assert!(m.meeting_reminder_on && m.is_reminder_on && !m.is_flagged);
        assert!(MessageFields::of(&v).writes_reminder());
        // Without any time the flag reminder is off, the meeting one on.
        let props = format!(
            "[{},{}]",
            class("IPM.Schedule.Meeting.Request"),
            prop(REMINDER_SET_PROP, r#""true""#)
        );
        let m = Message::from_json(&delta_item(None, Some(&props))).unwrap();
        assert!(m.meeting_reminder_on && !m.is_reminder_on);
    }

    /// #2123: the message size is `Integer 0x0E08` in any case and tag
    /// form; the folder's `Long 0x0E08` is not it, and the reverse.
    #[test]
    fn message_size_prop_id_matches_its_forms_only() {
        for id in [
            MESSAGE_SIZE_PROP,
            "Integer 0xe08",
            "integer 0x0E08",
            "INTEGER 0X00000E08",
        ] {
            assert!(is_message_size_prop(id), "{id:?}");
            assert!(!is_folder_size_prop(id), "{id:?}");
        }
        for id in [FOLDER_SIZE_PROP, "long 0xe08", "LONG 0X0E08"] {
            assert!(is_folder_size_prop(id), "{id:?}");
            assert!(!is_message_size_prop(id), "{id:?}");
        }
        for id in [
            "Integer 0x0E09",
            "Integer 0E08",
            "Integer0x0E08",
            "String 0x0E08",
            "Integer {00062008-0000-0000-C000-000000000046} Id 0x0E08",
            "",
        ] {
            assert!(!is_message_size_prop(id), "{id:?}");
            assert!(!is_folder_size_prop(id), "{id:?}");
        }
    }

    /// #2123: a size reads from a string (as Graph sends extended values)
    /// or a JSON integer; anything else is `None`, never an error.
    #[test]
    fn message_reads_its_size() {
        let size = |json: &str| Message::from_json(&parse(json).unwrap()).unwrap().size;
        assert_eq!(
            size(
                r#"{"id":"A","singleValueExtendedProperties":[
                {"id":"String 0x001A","value":"IPM.Note"},
                {"id":"Integer 0x0E08","value":"18432"}]}"#
            ),
            Some(18432)
        );
        assert_eq!(
            size(
                r#"{"id":"A","singleValueExtendedProperties":[
                {"id":"Integer 0xe08","value":" 7 "}]}"#
            ),
            Some(7)
        );
        assert_eq!(
            size(
                r#"{"id":"A","singleValueExtendedProperties":[
                {"id":"Integer 0x0E08","value":0}]}"#
            ),
            Some(0)
        );
        for json in [
            r#"{"id":"A"}"#,
            r#"{"id":"A","singleValueExtendedProperties":[]}"#,
            r#"{"id":"A","singleValueExtendedProperties":{"id":"Integer 0x0E08","value":"5"}}"#,
            r#"{"id":"A","singleValueExtendedProperties":[{"id":"Integer 0x0E08","value":"x"}]}"#,
            r#"{"id":"A","singleValueExtendedProperties":[{"id":"Integer 0x0E08","value":"-5"}]}"#,
            r#"{"id":"A","singleValueExtendedProperties":[{"id":"Integer 0x0E08","value":-5}]}"#,
            r#"{"id":"A","singleValueExtendedProperties":[{"id":"Integer 0x0E08","value":1.5}]}"#,
            r#"{"id":"A","singleValueExtendedProperties":[{"id":"Integer 0x0E08","value":null}]}"#,
            r#"{"id":"A","singleValueExtendedProperties":[{"id":"Integer 0x0E08"}]}"#,
            r#"{"id":"A","singleValueExtendedProperties":[{"id":"Long 0x0E08","value":"5"}]}"#,
        ] {
            assert_eq!(size(json), None, "{json}");
        }
    }

    /// #2123: a folder's own size is its `Long 0x0E08`, string or number;
    /// a message's `Integer 0x0E08` is not it.
    #[test]
    fn folder_size_reads_long_0x0e08() {
        let size = |json: &str| parse_folder_size(&parse(json).unwrap());
        assert_eq!(
            size(
                r#"{"id":"F","singleValueExtendedProperties":[
                {"id":"Long 0xe08","value":"5368709120"}]}"#
            ),
            Some(5_368_709_120)
        );
        assert_eq!(
            size(
                r#"{"id":"F","singleValueExtendedProperties":[
                {"id":"Long 0x0E08","value":12}]}"#
            ),
            Some(12)
        );
        for json in [
            r#"{"id":"F"}"#,
            r#"{"id":"F","singleValueExtendedProperties":[{"id":"Integer 0x0E08","value":"5"}]}"#,
            r#"{"id":"F","singleValueExtendedProperties":[{"id":"Long 0x0E08","value":"-1"}]}"#,
            r#"{"id":"F","singleValueExtendedProperties":[{"id":"Long 0x0E08","value":true}]}"#,
        ] {
            assert_eq!(size(json), None, "{json}");
        }
    }

    #[test]
    fn in_reply_to_prop_id_matches_its_forms_only() {
        for id in [
            IN_REPLY_TO_PROP,
            "String 0x1042",
            "string 0x1042",
            "STRING 0X1042",
        ] {
            assert!(is_in_reply_to_prop(id), "{id:?}");
        }
        for id in [
            "Integer 0x1042",
            "String 0x1043",
            "String 0x10420",
            "String 0x001A",
            "String 1042",
            "String {00062008-0000-0000-C000-000000000046} Id 0x1042",
            "",
        ] {
            assert!(!is_in_reply_to_prop(id), "{id:?}");
        }
    }

    #[test]
    fn message_reads_internet_message_id_and_in_reply_to() {
        let v = parse(
            r#"{"id":"D1","internetMessageId":"<d1@x>","singleValueExtendedProperties":[
                {"id":"String 0x001A","value":"IPM.Note"},
                {"id":"String 0x1042","value":" <m1@x> "}]}"#,
        )
        .unwrap();
        let m = Message::from_json(&v).unwrap();
        assert_eq!(m.internet_message_id, "<d1@x>");
        // Kept as Graph sends it, not trimmed.
        assert_eq!(m.in_reply_to, " <m1@x> ");

        // Absent, or not a string: `""`, never an error.
        for json in [
            r#"{"id":"A"}"#,
            r#"{"id":"A","internetMessageId":null,"singleValueExtendedProperties":[]}"#,
            r#"{"id":"A","internetMessageId":7,"singleValueExtendedProperties":[
                {"id":"String 0x1042","value":7}]}"#,
            r#"{"id":"A","singleValueExtendedProperties":{"id":"String 0x1042","value":"<m@x>"}}"#,
            r#"{"id":"A","singleValueExtendedProperties":[{"id":"Integer 0x1042","value":"<m@x>"}]}"#,
        ] {
            let m = Message::from_json(&parse(json).unwrap()).unwrap();
            assert_eq!(
                (m.internet_message_id.as_str(), m.in_reply_to.as_str()),
                ("", ""),
                "{json}"
            );
        }
    }

    #[test]
    fn in_reply_to_is_carried_only_with_a_string_entry() {
        let carried = |json: &str| parse_in_reply_to(&parse(json).unwrap());
        assert_eq!(
            carried(r#"{"singleValueExtendedProperties":[{"id":"String 0x1042","value":""}]}"#),
            (true, String::new())
        );
        assert_eq!(
            carried(
                r#"{"singleValueExtendedProperties":[{"id":"String 0x1042","value":"<a@x>"}]}"#
            ),
            (true, "<a@x>".to_string())
        );
        for json in [
            r#"{}"#,
            r#"{"singleValueExtendedProperties":[]}"#,
            r#"{"singleValueExtendedProperties":[{"id":"String 0x001A","value":"IPM.Note"}]}"#,
            r#"{"singleValueExtendedProperties":[{"id":"String 0x1042"}]}"#,
            r#"{"singleValueExtendedProperties":[{"id":"String 0x1042","value":true}]}"#,
            r#"{"singleValueExtendedProperties":[{"value":"<a@x>"}]}"#,
        ] {
            assert_eq!(carried(json), (false, String::new()), "{json}");
            let f = MessageFields::of(&parse(json).unwrap());
            assert!(!f.writes_in_reply_to(), "{json}");
        }
        let f = MessageFields::of(
            &parse(r#"{"singleValueExtendedProperties":[{"id":"String 0x1042","value":"<a@x>"}]}"#)
                .unwrap(),
        );
        assert!(f.writes_in_reply_to());
        assert!(MessageFields::all().writes_in_reply_to());
    }

    #[test]
    fn fields_carry_internet_message_id_only_with_the_key() {
        let f = |json: &str| MessageFields::of(&parse(json).unwrap()).writes_internet_message_id();
        assert!(f(r#"{"internetMessageId":"<a@x>"}"#));
        assert!(f(r#"{"internetMessageId":""}"#));
        assert!(!f(r#"{}"#));
        assert!(!f(r#"{"internetMessageId":null}"#));
        assert!(!f(r#"{"internetMessageId":7}"#));
        // A complete item without the key says nothing either: no
        // "complete item" term.
        assert!(!f(
            r#"{"subject":"s","receivedDateTime":"t","from":{"emailAddress":{"address":"a@x"}}}"#
        ));
        assert!(MessageFields::all().writes_internet_message_id());
    }

    #[test]
    fn a_complete_item_writes_its_is_draft() {
        // #2096: `isDraft` is selected, so a complete item carries it; one
        // without the key (code-built) still writes its parsed value.
        let complete =
            r#""subject":"s","receivedDateTime":"t","from":{"emailAddress":{"address":"a@x"}}"#;
        for (json, draft) in [
            (format!(r#"{{{complete},"isDraft":true}}"#), true),
            (format!(r#"{{{complete},"isDraft":false}}"#), false),
            (format!(r#"{{{complete}}}"#), false),
        ] {
            let v = parse(&json).unwrap();
            assert!(MessageFields::of(&v).writes_is_draft(), "{json}");
            assert_eq!(Message::from_json(&v).unwrap().is_draft, draft, "{json}");
        }
        let partial = parse(r#"{"isRead":true}"#).unwrap();
        assert!(!MessageFields::of(&partial).writes_is_draft());
        let partial = parse(r#"{"isRead":true,"isDraft":true}"#).unwrap();
        assert!(MessageFields::of(&partial).writes_is_draft());
    }
}
