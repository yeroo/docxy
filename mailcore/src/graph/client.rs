//! Blocking Microsoft Graph REST client, over `ureq`.
//!
//! `base` is injectable so tests point it at the in-process fake server
//! (`crate::testserver`); production passes `https://graph.microsoft.com/v1.0`.
//! Every request sends `Authorization: Bearer {token}` and
//! `Accept: application/json`. Token refresh is not this client's job — on
//! `GraphError::Unauthorized` it just returns the error; the sync engine
//! (a later task) catches it, refreshes, and retries once. The bearer token
//! is never logged (it's not `Debug`-derived into any log line here).

use crate::graph::model::{
    AttachmentMeta, AutomaticReplies, Body, Calendar, ContactFields, DeltaPage, Event,
    EventDeltaItem, EventDeltaPage, EventRecurrence, FolderContact, FolderListing, MailFolder,
    MasterCategory, Me, MeetingMessage, Message, MessageFields, MessageHeader, OofStatus, Person,
    PhysicalAddress, REMINDER_SET_PROP, REMINDER_SIGNAL_TIME_PROP, Recipient, Recurrence,
    ScheduleEntry, SearchHit, plain_to_html, to_utc,
};
use crate::json::{self, Value};
use std::fmt;

/// A Microsoft Graph REST client bound to one base URL and access token.
pub struct GraphClient {
    base: String,
    token: String,
}

/// Errors from a Graph request: transport/HTTP failures mapped to the
/// shapes callers (the sync engine, triage commands) need to act on.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphError {
    /// 401: the access token is missing/expired/invalid. Not retried here.
    Unauthorized,
    /// 429 or 503: back off and retry after this many seconds (from the
    /// `Retry-After` header, defaulting to 30 when absent/unparseable).
    Throttled { retry_after_secs: u64 },
    /// 404: the resource (message/folder/attachment) doesn't exist.
    NotFound,
    /// Any other non-2xx response.
    Http { status: u16, body: String },
    /// A 2xx response whose body wasn't the JSON shape expected.
    Parse(String),
    /// Connection/TLS/IO failure — never reached the server, or never got a
    /// response back from it.
    Transport(String),
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphError::Unauthorized => write!(f, "unauthorized (401)"),
            GraphError::Throttled { retry_after_secs } => {
                write!(f, "throttled, retry after {retry_after_secs}s")
            }
            GraphError::NotFound => write!(f, "not found (404)"),
            GraphError::Http { status, body } => f.write_str(&http_error_text(*status, body)),
            GraphError::Parse(m) => write!(f, "failed to parse response: {m}"),
            GraphError::Transport(m) => write!(f, "transport error: {m}"),
        }
    }
}

/// The most characters `GraphError::Http`'s `Display` shows: a host status
/// line has room for one line, not a server's error page (#1954).
pub(crate) const HTTP_ERROR_TEXT_MAX: usize = 200;

/// `GraphError::Http`'s `Display` (#1954): `HTTP {status}: {message}` when
/// the body is Graph JSON with a non-empty `error.message`, else
/// `HTTP {status} {reason}` (the standard reason phrase, from
/// `reason_phrase`'s table), else `HTTP {status}`. Never the body itself (an
/// IIS 411 page is four lines of HTML); always one line, at most
/// `HTTP_ERROR_TEXT_MAX` characters. The body stays in the error for `Debug`.
fn http_error_text(status: u16, body: &str) -> String {
    let text = match (graph_error_message(body), reason_phrase(status)) {
        (Some(message), _) => format!("HTTP {status}: {message}"),
        (None, Some(reason)) => format!("HTTP {status} {reason}"),
        (None, None) => format!("HTTP {status}"),
    };
    one_line(&text, HTTP_ERROR_TEXT_MAX)
}

/// Graph's own `error.message` from a JSON error body, trimmed; `None` when
/// the body is not JSON, has none, or it is blank. Shared by `Display` and
/// the outbox's `send_error_text`, so the two never disagree on it.
pub(crate) fn graph_error_message(body: &str) -> Option<String> {
    let v = json::parse(body).ok()?;
    let message = v.get("error")?.get("message")?.as_str()?.trim();
    (!message.is_empty()).then(|| message.to_string())
}

/// Whether `e` says a delta link's sync state expired or became invalid
/// (#1871): a 410 Gone, or a 400 whose Graph `error.code` is
/// `SyncStateNotFound`, `SyncStateInvalid` or `resyncRequired` (compared
/// case-insensitively), as Exchange answers for an old or broken skip token.
/// The delta then has to start again from a fresh cursor.
pub(crate) fn is_expired_sync_state(e: &GraphError) -> bool {
    match e {
        GraphError::Http { status: 410, .. } => true,
        GraphError::Http { status: 400, body } => json::parse(body)
            .ok()
            .and_then(|v| {
                v.get("error")?
                    .get("code")?
                    .as_str()
                    .map(|c| c.trim().to_ascii_lowercase())
            })
            .is_some_and(|code| {
                matches!(
                    code.as_str(),
                    "syncstatenotfound" | "syncstateinvalid" | "resyncrequired"
                )
            }),
        _ => false,
    }
}

/// `s` as one line: every run of whitespace (newlines and tabs included)
/// becomes one space, other control characters are dropped, the ends are
/// trimmed, and past `max` characters it is cut (on a `char`) to end in `…`
/// within `max`.
pub(crate) fn one_line(s: &str, max: usize) -> String {
    let words: Vec<String> = s
        .split_whitespace()
        .map(|w| w.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|w| !w.is_empty())
        .collect();
    let joined = words.join(" ");
    if joined.chars().count() <= max {
        return joined;
    }
    let mut cut: String = joined.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The standard reason phrase for the error statuses Graph and its front
/// ends answer with; `None` for any other status.
fn reason_phrase(status: u16) -> Option<&'static str> {
    Some(match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        410 => "Gone",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => return None,
    })
}

impl std::error::Error for GraphError {}

/// Where a delta query starts from: a first call for a folder (its recent
/// mail, or all of it), or a `nextLink`/`deltaLink` URL carried over from a
/// previous page. Both first calls ask for the newest mail first
/// (`$orderby=receivedDateTime desc`, the one order Graph's message delta
/// supports, #1871).
#[derive(Debug, Clone, PartialEq)]
pub enum DeltaCursor {
    /// First call of a folder's *head* delta (#1871): only the mail received
    /// at or after `since` (an ISO-8601 UTC instant), with the server-side
    /// `$filter=receivedDateTime ge {since}` Graph supports on message delta.
    /// Its deltaLink keeps that filter.
    Head { folder_id: String, since: String },
    /// First call of a folder's *full* delta (#1871): every message, newest
    /// first, with no filter.
    Full(String),
    /// A `@odata.nextLink` (more pages) or `@odata.deltaLink` (resume point
    /// for the next sync) from a prior `DeltaPage`. Already a complete URL,
    /// so it's sent as-is rather than joined with `base`.
    Link(String),
}

/// The `Prefer` header of every calendarView delta request (#2013).
const CALENDAR_DELTA_PREFER: &str = "outlook.timezone=\"UTC\", odata.maxpagesize=50";

/// Where a calendarView delta round's page comes from (#2013): the first
/// page of an initial round over a calendar's window, or a `nextLink` or
/// `deltaLink` from an earlier page, sent as is (`DeltaCursor::Link`).
#[derive(Debug, Clone, PartialEq)]
pub enum CalendarDeltaCursor {
    Start {
        calendar_id: String,
        from_utc: String,
        to_utc: String,
    },
    Link(String),
}

/// One page of a folder list (`GraphClient::fetch_folder_page`).
struct FolderPage {
    /// Each listed folder with its `childFolderCount` (`None` when absent).
    folders: Vec<(MailFolder, Option<i64>)>,
    /// False when a listed item had no id (and was dropped).
    all_have_ids: bool,
    next_link: Option<String>,
}

/// Which RSVP action `respond_event` sends, mapped to Graph's
/// `/me/events/{id}/accept|decline|tentativelyAccept` endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsvpKind {
    Accept,
    Decline,
    Tentative,
}

impl RsvpKind {
    /// Maps the `response_status` vocabulary `Store::set_event_response`
    /// writes locally (and `SyncCommand::RespondEvent`'s and
    /// `OutboxOp::RespondEvent`'s `kind` carry) to the action
    /// `respond_event` sends: `"accepted"` → `Accept`, `"declined"` →
    /// `Decline`, `"tentativelyAccepted"` → `Tentative`. `None` for anything
    /// else: an unknown value must never fall back to accepting a meeting.
    pub fn from_status(status: &str) -> Option<RsvpKind> {
        match status {
            "accepted" => Some(RsvpKind::Accept),
            "declined" => Some(RsvpKind::Decline),
            "tentativelyAccepted" => Some(RsvpKind::Tentative),
            _ => None,
        }
    }
}

/// Checks an RSVP's proposed new time before anything is sent or written
/// (#2023), and returns the window to send, normalised to canonical UTC
/// (`graph::model::to_utc`), or `None` when the response proposes nothing.
/// `Some("")` counts as absent, so two empty fields are no proposal. A
/// proposal is refused (the message says why) when it comes with an accept
/// (Graph's `accept` takes none), with `send_response` false (Graph:
/// proposing requires `sendResponse: true`), with only one end of the
/// window, or with an end not after its start.
///
/// The sync engine runs this first for `SyncCommand::RespondEvent` and
/// `RespondMeeting`; a host that writes the RSVP status locally before it
/// sends the command should call it first too, so a refused response never
/// leaves a status Graph was never told. The stored event's
/// `allow_new_time_proposals` is the engine's check, not this one's.
pub fn rsvp_proposal(
    kind: RsvpKind,
    send_response: bool,
    start_utc: Option<&str>,
    end_utc: Option<&str>,
) -> Result<Option<(String, String)>, &'static str> {
    let start = start_utc.filter(|s| !s.is_empty());
    let end = end_utc.filter(|s| !s.is_empty());
    let (start, end) = match (start, end) {
        (None, None) => return Ok(None),
        (Some(s), Some(e)) => (to_utc(s, "UTC"), to_utc(e, "UTC")),
        _ => return Err("a proposed new time needs both a start and an end"),
    };
    if kind == RsvpKind::Accept {
        return Err("an accept cannot propose a new time");
    }
    if !send_response {
        return Err("proposing a new time requires sending a response");
    }
    if end <= start {
        return Err("a proposed new time must end after it starts");
    }
    Ok(Some((start, end)))
}

/// Why one end of an event cannot be sent in its named time zone (#2039),
/// or `None` when it can, or when `zone` is `""` (sent in UTC, nothing to
/// check). A named end needs `local`, its wall clock, as
/// `YYYY-MM-DDTHH:MM:SS` and a real date and time, `utc` as the store's
/// `YYYY-MM-DDTHH:MM:SSZ`, and `local − utc` within Graph's range of zones,
/// UTC−12 to UTC+14. That catches a wall clock built from the wrong day or
/// a swapped field, not a wrong DST offset: mailcore has no time zone
/// database, so the wall clock is the caller's.
///
/// `Store::create_local_event`, `Store::update_event_fields` and the
/// engine's `SyncCommand::UpdateEventSeries` refuse an end this refuses;
/// a host may call it first, as `rsvp_proposal`. `event_body_json` sends
/// such an end of a hand-built `EventInput` in UTC instead, never erring.
pub fn event_zone_error(zone: &str, local: &str, utc: &str) -> Option<&'static str> {
    if zone.is_empty() {
        return None;
    }
    // Digits only where digits go (the parser would take a `+1` month) and
    // no leap second, which no one types.
    let digits = local.len() == 19
        && local
            .bytes()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7 | 10 | 13 | 16) || b.is_ascii_digit());
    let local_secs = (digits && !local.ends_with(":60"))
        .then(|| crate::store::iso8601_to_unix_secs(&format!("{local}Z")))
        .flatten();
    let Some(local_secs) = local_secs else {
        return Some("a time in a named time zone needs its wall clock as YYYY-MM-DDTHH:MM:SS");
    };
    let Some(utc_secs) = crate::store::iso8601_to_unix_secs(utc) else {
        return Some("a time in a named time zone needs its UTC time as YYYY-MM-DDTHH:MM:SSZ");
    };
    if !(-12 * 3600..=14 * 3600).contains(&(local_secs - utc_secs)) {
        return Some("a wall clock must be within UTC-12 to UTC+14 of its UTC time");
    }
    None
}

/// The Graph `dateTimeTimeZone` one end is sent as in its named zone, or
/// `None` to send it in UTC: the zone is `""` or `event_zone_error`
/// refuses it. The one rule for `start`/`end` and `recurrenceTimeZone`.
fn zoned_end(zone: &str, local: &str, utc: &str) -> Option<Value> {
    if zone.is_empty() || event_zone_error(zone, local, utc).is_some() {
        return None;
    }
    Some(Value::Object(vec![
        ("dateTime".to_string(), Value::Str(local.to_string())),
        ("timeZone".to_string(), Value::Str(zone.to_string())),
    ]))
}

/// A recurrence `Value` with `zone` as its `range.recurrenceTimeZone`: the
/// create-side `Recurrence::to_json` sends none of its own.
fn with_range_time_zone(mut rec: Value, zone: &str) -> Value {
    if let Value::Object(o) = &mut rec {
        if let Some((_, Value::Object(range))) = o.iter_mut().find(|(k, _)| k == "range") {
            range.retain(|(k, _)| k != "recurrenceTimeZone");
            range.push((
                "recurrenceTimeZone".to_string(),
                Value::Str(zone.to_string()),
            ));
        }
    }
    rec
}

/// The groups of event properties a create or update sends (#1994). An
/// update sends only the groups an edit changed (`Store::update_event_fields`
/// returns them), so editing a title does not rewrite the attendees. Each
/// group has a JSON name, which a queued `OutboxOp::UpdateEvent` stores: the
/// names never change (see `docs/claude/mail.md`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EventFieldSet(u32);

impl EventFieldSet {
    pub const SUBJECT: EventFieldSet = EventFieldSet(1 << 0);
    /// `start`, `end` and `isAllDay`, always together: Graph validates them
    /// as one (an all-day toggle with only `isAllDay` is refused).
    pub const TIMES: EventFieldSet = EventFieldSet(1 << 1);
    pub const LOCATION: EventFieldSet = EventFieldSet(1 << 2);
    pub const ATTENDEES: EventFieldSet = EventFieldSet(1 << 3);
    pub const BODY: EventFieldSet = EventFieldSet(1 << 4);
    /// `recurrence`, sent only when the input has one.
    pub const RECURRENCE: EventFieldSet = EventFieldSet(1 << 5);
    pub const SHOW_AS: EventFieldSet = EventFieldSet(1 << 6);
    pub const SENSITIVITY: EventFieldSet = EventFieldSet(1 << 7);
    pub const IMPORTANCE: EventFieldSet = EventFieldSet(1 << 8);
    pub const CATEGORIES: EventFieldSet = EventFieldSet(1 << 9);
    /// `isReminderOn` and `reminderMinutesBeforeStart`.
    pub const REMINDER: EventFieldSet = EventFieldSet(1 << 10);
    /// `isOnlineMeeting`, and `onlineMeetingProvider` when one is set.
    pub const ONLINE_MEETING: EventFieldSet = EventFieldSet(1 << 11);
    pub const RESPONSE_REQUESTED: EventFieldSet = EventFieldSet(1 << 12);
    pub const ALLOW_NEW_TIME_PROPOSALS: EventFieldSet = EventFieldSet(1 << 13);

    /// Every group: what a create sends.
    pub const ALL: EventFieldSet = EventFieldSet((1 << 14) - 1);
    /// What an update sent before #1994, so an `UpdateEvent` queued then
    /// (with no `fields`) still sends what it did.
    pub const LEGACY_UPDATE: EventFieldSet = EventFieldSet(
        Self::SUBJECT.0
            | Self::TIMES.0
            | Self::LOCATION.0
            | Self::ATTENDEES.0
            | Self::BODY.0
            | Self::RECURRENCE.0,
    );

    /// Each group and its JSON name, in bit order. Never rename one: a
    /// queued op stores them.
    const NAMES: [(EventFieldSet, &'static str); 14] = [
        (Self::SUBJECT, "subject"),
        (Self::TIMES, "times"),
        (Self::LOCATION, "location"),
        (Self::ATTENDEES, "attendees"),
        (Self::BODY, "body"),
        (Self::RECURRENCE, "recurrence"),
        (Self::SHOW_AS, "showAs"),
        (Self::SENSITIVITY, "sensitivity"),
        (Self::IMPORTANCE, "importance"),
        (Self::CATEGORIES, "categories"),
        (Self::REMINDER, "reminder"),
        (Self::ONLINE_MEETING, "onlineMeeting"),
        (Self::RESPONSE_REQUESTED, "responseRequested"),
        (Self::ALLOW_NEW_TIME_PROPOSALS, "allowNewTimeProposals"),
    ];

    pub fn contains(self, other: EventFieldSet) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: EventFieldSet) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: EventFieldSet) {
        self.0 &= !other.0;
    }

    pub fn union(self, other: EventFieldSet) -> EventFieldSet {
        EventFieldSet(self.0 | other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The JSON names of the groups in the set, in bit order.
    pub fn to_names(self) -> Vec<&'static str> {
        Self::NAMES
            .iter()
            .filter(|(g, _)| self.contains(*g))
            .map(|(_, n)| *n)
            .collect()
    }

    /// The set the names spell; an unknown name is dropped.
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> EventFieldSet {
        let mut set = EventFieldSet::default();
        for name in names {
            if let Some((g, _)) = Self::NAMES.iter().find(|(_, n)| *n == name.as_ref()) {
                set.insert(*g);
            }
        }
        set
    }
}

/// The properties of a contact a create or update sends (#2063), one bit per
/// Graph property: an update sends only the ones an edit changed
/// (`Store::update_contact_fields` returns them). Each JSON name is the
/// Graph property's, which a queued `OutboxOp::UpdateContact` stores: the
/// names never change (see `docs/claude/mail.md`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContactFieldSet(u32);

impl ContactFieldSet {
    pub const GIVEN_NAME: ContactFieldSet = ContactFieldSet(1 << 0);
    pub const MIDDLE_NAME: ContactFieldSet = ContactFieldSet(1 << 1);
    pub const SURNAME: ContactFieldSet = ContactFieldSet(1 << 2);
    pub const DISPLAY_NAME: ContactFieldSet = ContactFieldSet(1 << 3);
    pub const FILE_AS: ContactFieldSet = ContactFieldSet(1 << 4);
    pub const EMAIL_ADDRESSES: ContactFieldSet = ContactFieldSet(1 << 5);
    pub const BUSINESS_PHONES: ContactFieldSet = ContactFieldSet(1 << 6);
    pub const HOME_PHONES: ContactFieldSet = ContactFieldSet(1 << 7);
    pub const MOBILE_PHONE: ContactFieldSet = ContactFieldSet(1 << 8);
    pub const COMPANY_NAME: ContactFieldSet = ContactFieldSet(1 << 9);
    pub const JOB_TITLE: ContactFieldSet = ContactFieldSet(1 << 10);
    pub const DEPARTMENT: ContactFieldSet = ContactFieldSet(1 << 11);
    pub const BUSINESS_ADDRESS: ContactFieldSet = ContactFieldSet(1 << 12);
    pub const HOME_ADDRESS: ContactFieldSet = ContactFieldSet(1 << 13);
    pub const BIRTHDAY: ContactFieldSet = ContactFieldSet(1 << 14);
    pub const PERSONAL_NOTES: ContactFieldSet = ContactFieldSet(1 << 15);
    pub const CATEGORIES: ContactFieldSet = ContactFieldSet(1 << 16);

    /// Every property.
    pub const ALL: ContactFieldSet = ContactFieldSet((1 << 17) - 1);

    /// Each property and its JSON (Graph) name, in bit order. Never rename
    /// one: a queued op stores them.
    const NAMES: [(ContactFieldSet, &'static str); 17] = [
        (Self::GIVEN_NAME, "givenName"),
        (Self::MIDDLE_NAME, "middleName"),
        (Self::SURNAME, "surname"),
        (Self::DISPLAY_NAME, "displayName"),
        (Self::FILE_AS, "fileAs"),
        (Self::EMAIL_ADDRESSES, "emailAddresses"),
        (Self::BUSINESS_PHONES, "businessPhones"),
        (Self::HOME_PHONES, "homePhones"),
        (Self::MOBILE_PHONE, "mobilePhone"),
        (Self::COMPANY_NAME, "companyName"),
        (Self::JOB_TITLE, "jobTitle"),
        (Self::DEPARTMENT, "department"),
        (Self::BUSINESS_ADDRESS, "businessAddress"),
        (Self::HOME_ADDRESS, "homeAddress"),
        (Self::BIRTHDAY, "birthday"),
        (Self::PERSONAL_NOTES, "personalNotes"),
        (Self::CATEGORIES, "categories"),
    ];

    pub fn contains(self, other: ContactFieldSet) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: ContactFieldSet) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: ContactFieldSet) {
        self.0 &= !other.0;
    }

    pub fn union(self, other: ContactFieldSet) -> ContactFieldSet {
        ContactFieldSet(self.0 | other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The JSON names of the properties in the set, in bit order.
    pub fn to_names(self) -> Vec<&'static str> {
        Self::NAMES
            .iter()
            .filter(|(g, _)| self.contains(*g))
            .map(|(_, n)| *n)
            .collect()
    }

    /// The set the names spell; an unknown name is dropped.
    pub fn from_names<S: AsRef<str>>(names: &[S]) -> ContactFieldSet {
        let mut set = ContactFieldSet::default();
        for name in names {
            if let Some((g, _)) = Self::NAMES.iter().find(|(_, n)| *n == name.as_ref()) {
                set.insert(*g);
            }
        }
        set
    }

    /// The properties of `f` that hold something: what a create sends.
    pub fn non_empty(f: &ContactFields) -> ContactFieldSet {
        let mut set = ContactFieldSet::default();
        let mut put = |g: ContactFieldSet, has: bool| {
            if has {
                set.insert(g);
            }
        };
        put(Self::GIVEN_NAME, !f.given_name.is_empty());
        put(Self::MIDDLE_NAME, !f.middle_name.is_empty());
        put(Self::SURNAME, !f.surname.is_empty());
        put(Self::DISPLAY_NAME, !f.display_name.is_empty());
        put(Self::FILE_AS, !f.file_as.is_empty());
        put(
            Self::EMAIL_ADDRESSES,
            f.email_addresses
                .iter()
                .any(|e| !e.address.trim().is_empty()),
        );
        put(Self::BUSINESS_PHONES, !f.business_phones.is_empty());
        put(Self::HOME_PHONES, !f.home_phones.is_empty());
        put(Self::MOBILE_PHONE, !f.mobile_phone.is_empty());
        put(Self::COMPANY_NAME, !f.company_name.is_empty());
        put(Self::JOB_TITLE, !f.job_title.is_empty());
        put(Self::DEPARTMENT, !f.department.is_empty());
        put(Self::BUSINESS_ADDRESS, !f.business_address.is_empty());
        put(Self::HOME_ADDRESS, !f.home_address.is_empty());
        put(Self::BIRTHDAY, !f.birthday.is_empty());
        put(Self::PERSONAL_NOTES, !f.personal_notes.is_empty());
        put(Self::CATEGORIES, !f.categories.is_empty());
        set
    }

    /// The properties whose values differ between `a` and `b`: what an
    /// update sends.
    pub fn changed(a: &ContactFields, b: &ContactFields) -> ContactFieldSet {
        let mut set = ContactFieldSet::default();
        let mut put = |g: ContactFieldSet, differs: bool| {
            if differs {
                set.insert(g);
            }
        };
        put(Self::GIVEN_NAME, a.given_name != b.given_name);
        put(Self::MIDDLE_NAME, a.middle_name != b.middle_name);
        put(Self::SURNAME, a.surname != b.surname);
        put(Self::DISPLAY_NAME, a.display_name != b.display_name);
        put(Self::FILE_AS, a.file_as != b.file_as);
        put(
            Self::EMAIL_ADDRESSES,
            a.email_addresses != b.email_addresses,
        );
        put(
            Self::BUSINESS_PHONES,
            a.business_phones != b.business_phones,
        );
        put(Self::HOME_PHONES, a.home_phones != b.home_phones);
        put(Self::MOBILE_PHONE, a.mobile_phone != b.mobile_phone);
        put(Self::COMPANY_NAME, a.company_name != b.company_name);
        put(Self::JOB_TITLE, a.job_title != b.job_title);
        put(Self::DEPARTMENT, a.department != b.department);
        put(
            Self::BUSINESS_ADDRESS,
            a.business_address != b.business_address,
        );
        put(Self::HOME_ADDRESS, a.home_address != b.home_address);
        put(Self::BIRTHDAY, a.birthday != b.birthday);
        put(Self::PERSONAL_NOTES, a.personal_notes != b.personal_notes);
        put(Self::CATEGORIES, a.categories != b.categories);
        set
    }
}

/// One attendee an event create or update sends. `r#type` is Graph's
/// `required`, `optional` or `resource`; `""` sends `required` (and, on a
/// `Store::update_event_fields`, keeps the stored attendee's type).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttendeeInput {
    pub name: String,
    pub addr: String,
    pub r#type: String,
}

/// The fields of an event, as the create/update calls serialize them. Only
/// the groups in `fields` are sent (`event_body_json`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EventInput {
    pub subject: String,
    pub start_utc: String, // YYYY-MM-DDTHH:MM:SSZ
    pub end_utc: String,
    pub is_all_day: bool,
    pub location: String,
    pub attendees: Vec<AttendeeInput>,
    pub body_html: String,
    /// The recurrence pattern for a repeating event, or `None` for a single
    /// event. Serialized into the create/update body by `event_body_json`.
    pub recurrence: Option<Recurrence>,
    /// Graph's own lossless recurrence: a series edit's (#2022), or the one a
    /// local create was given (`LocalEventFields::series_recurrence`, #2034,
    /// through `Store::event_for_send`). When `Some`, the `recurrence` group
    /// sends it (`to_send_json`) instead of the create-side `recurrence`, so
    /// a `relativeMonthly` or `numbered` series is not rewritten into one of
    /// the create side's three kinds. Its `range.recurrence_time_zone` is
    /// sent as given unless the start is sent in its named zone
    /// (`start_time_zone`, #2039), whose zone then replaces it.
    pub series_recurrence: Option<EventRecurrence>,
    /// Graph `showAs`, `sensitivity`, `importance`; `""` is not sent (Graph
    /// refuses an empty enum).
    pub show_as: String,
    pub sensitivity: String,
    pub importance: String,
    pub categories: Vec<String>,
    pub is_reminder_on: bool,
    pub reminder_minutes: i64,
    pub is_online_meeting: bool,
    /// Graph `onlineMeetingProvider`; `""` is not sent, so Graph uses the
    /// calendar's default provider (`teamsForBusiness` on Microsoft 365).
    pub online_meeting_provider: String,
    pub response_requested: bool,
    pub allow_new_time_proposals: bool,
    /// The time zone the start is sent in (#2039): a Windows time zone id
    /// (`W. Europe Standard Time`, the safe choice) or one of the IANA
    /// names Graph lists for `dateTimeTimeZone.timeZone` (`Europe/Berlin`).
    /// `""` sends the UTC wall clock with `timeZone: "UTC"`, as before. A
    /// series whose start is sent in a zone sends it as its
    /// `recurrence.range.recurrenceTimeZone` too (see `event_body_json`).
    pub start_time_zone: String,
    /// The start's wall clock in `start_time_zone`, `YYYY-MM-DDTHH:MM:SS`
    /// (no `Z`, no offset, no fraction): what the person typed. Sent only
    /// when `event_zone_error` accepts it with `start_utc`; ignored when the
    /// zone is `""`. An all-day end's wall clock is `T00:00:00` of its
    /// local date (Graph requires midnight in the event's zone).
    pub start_local: String,
    /// `start_time_zone` and `start_local` for the end, which may be in
    /// another zone (a flight).
    pub end_time_zone: String,
    pub end_local: String,
    /// The groups this call sends.
    pub fields: EventFieldSet,
}

/// The `$select` fields covering every header field `Message::from_json`
/// reads, so delta pages carry everything the local store needs without
/// an extra per-message fetch.
const MESSAGE_SELECT: &str = "id,conversationId,subject,from,toRecipients,ccRecipients,\
receivedDateTime,sentDateTime,isRead,flag,hasAttachments,importance,bodyPreview,categories";

/// The `$expand` that brings a message's Outlook reminder (#2054), its
/// message class (#2081) and its protection (#2088): the three
/// PSETID_Common named properties `Message::from_json` reads
/// (`PidLidReminderSet`, `PidLidReminderTime`, `PidLidReminderSignalTime`,
/// the `REMINDER_*_PROP` ids), `PidTagMessageClass` (`MESSAGE_CLASS_PROP`),
/// from which a meeting message's type is read, and `PidTagSensitivity`,
/// the `msip_labels` header and `PidNameContentClass`
/// (`SENSITIVITY_PROP`, `MSIP_LABELS_PROP`, `CONTENT_CLASS_PROP`), filtered
/// so nothing else comes. Quotes and braces are percent-encoded, so the URL
/// goes out as written here. One expand: a 400 on any term sends the call
/// again without all of them.
const MESSAGE_EXPAND: &str = "singleValueExtendedProperties($filter=\
id%20eq%20%27Boolean%20%7B00062008-0000-0000-C000-000000000046%7D%20Id%200x8503%27%20or%20\
id%20eq%20%27SystemTime%20%7B00062008-0000-0000-C000-000000000046%7D%20Id%200x8502%27%20or%20\
id%20eq%20%27SystemTime%20%7B00062008-0000-0000-C000-000000000046%7D%20Id%200x8560%27%20or%20\
id%20eq%20%27String%200x001A%27%20or%20\
id%20eq%20%27Integer%200x0036%27%20or%20\
id%20eq%20%27String%20%7B00020386-0000-0000-C000-000000000046%7D%20Name%20msip_labels%27%20or%20\
id%20eq%20%27String%20%7B00020386-0000-0000-C000-000000000046%7D%20Name%20Content-Class%27)";

/// `target` with the reminder, class and protection `$expand` (#2054,
/// #2081, #2088) appended.
fn with_expand(target: &str) -> String {
    format!("{target}&$expand={MESSAGE_EXPAND}")
}

enum Method {
    Get,
    Patch,
    Post,
    Delete,
}

/// Graph's inline-attachment ceiling: files this size or smaller go inline as a
/// fileAttachment; larger ones use an upload session.
const INLINE_MAX: usize = 3 * 1024 * 1024;
/// Upload-session chunk size — a multiple of 320 KiB per Graph's requirement.
const CHUNK_SIZE: usize = 12 * 320 * 1024; // 3,932,160 bytes

/// What an attachment upload reports while it runs (#1941), to
/// `GraphClient::upload_attachment`'s caller. Both methods default to
/// nothing; `()` is the observer that wants neither.
pub(crate) trait UploadObserver {
    /// An upload session was opened (`Some(url)`; the url resumes it), or
    /// the one the upload was given or opened is no good any more (`None`):
    /// forget it.
    fn session(&mut self, _upload_url: Option<&str>) {}
    /// Graph has `sent` of the attachment's `total` bytes.
    fn progress(&mut self, _sent: u64, _total: u64) {}
}

impl UploadObserver for () {}

/// A session PUT Graph refused for good (#1941): a 4xx other than a 429
/// throttle (`put_bytes` maps a 401 and a 404 to `Http` too). Retrying the
/// same session cannot help, unlike a 5xx, a throttle or a lost connection.
fn session_refused(e: &GraphError) -> bool {
    matches!(e, GraphError::Http { status, .. } if *status < 500)
}

impl GraphClient {
    /// `base` has no trailing slash requirement — one is stripped if
    /// present, since every path built below starts with `/`.
    pub fn new(base: &str, access_token: &str) -> Self {
        GraphClient {
            base: base.trim_end_matches('/').to_string(),
            token: access_token.to_string(),
        }
    }

    /// GET `/me/mailFolders?$top=100` — the top-level mail folders — then
    /// recursively GET each folder's `/childFolders?$top=100`, flattening
    /// the whole tree into one `Vec` (each folder's `parent_id`, parsed from
    /// Graph's `parentFolderId`, is enough for callers to reconstruct the
    /// hierarchy later). Every list follows `@odata.nextLink` until Graph
    /// stops sending one. A `visited` guard skips any folder id already
    /// collected, so a server that (erroneously, or via test-double route
    /// reuse) reports a folder as its own descendant can't recurse forever.
    ///
    /// A folder whose `childFolderCount` is 0 has no `childFolders` call
    /// (#1886: the sync engine lists the tree on every tick); one without
    /// the property is asked.
    ///
    /// An error on any top-level page always propagates (can't list folders
    /// at all is a real failure). Each folder's `childFolders` sub-call has a
    /// looser policy — see `collect_folder_and_children` — that leaves the
    /// listing incomplete (`FolderListing::complete`), as does a listed
    /// folder without an id, which is dropped.
    pub fn list_folders(&self) -> Result<FolderListing, GraphError> {
        let mut top = Vec::new();
        let mut complete = true;
        let mut target = "/me/mailFolders?$top=100".to_string();
        loop {
            let page = self.fetch_folder_page(&target)?;
            complete &= page.all_have_ids;
            top.extend(page.folders);
            match page.next_link {
                Some(next) => target = next,
                None => break,
            }
        }

        let mut listing = FolderListing {
            folders: Vec::new(),
            complete,
        };
        let mut visited = std::collections::HashSet::new();
        for (folder, child_count) in top {
            self.collect_folder_and_children(folder, child_count, &mut listing, &mut visited)?;
        }
        Ok(listing)
    }

    /// Pushes `folder` onto `out`, then fetches (every page of) and recurses
    /// into its `/childFolders`, unless `child_count` says it has none. See
    /// `list_folders` for the `visited` cycle guard.
    ///
    /// Error policy for the `childFolders` sub-call: `Unauthorized` and
    /// `Throttled` are global conditions the sync engine must handle
    /// centrally (refresh the token / back off), so those propagate out of
    /// `list_folders` same as before. Anything else (`NotFound`, `Http`,
    /// `Transport`, malformed-JSON `Parse`) keeps the children listed so far
    /// and marks the listing incomplete — a transient failure or a 404
    /// on one deep, obscure subfolder must not discard the folders already
    /// collected (Inbox, Sent, etc.), which is what propagating via `?`
    /// unconditionally would do, and must not let the engine remove the
    /// folders it could not see.
    fn collect_folder_and_children(
        &self,
        folder: MailFolder,
        child_count: Option<i64>,
        out: &mut FolderListing,
        visited: &mut std::collections::HashSet<String>,
    ) -> Result<(), GraphError> {
        if !visited.insert(folder.id.clone()) {
            return Ok(());
        }
        let mut target = format!(
            "/me/mailFolders/{}/childFolders?$top=100",
            encode_path_segment(&folder.id)
        );
        out.folders.push(folder);
        if child_count == Some(0) {
            return Ok(());
        }

        let mut children = Vec::new();
        loop {
            match self.fetch_folder_page(&target) {
                Ok(page) => {
                    out.complete &= page.all_have_ids;
                    children.extend(page.folders);
                    match page.next_link {
                        Some(next) => target = next,
                        None => break,
                    }
                }
                Err(e @ (GraphError::Unauthorized | GraphError::Throttled { .. })) => {
                    return Err(e);
                }
                Err(_) => {
                    out.complete = false;
                    break;
                }
            }
        }
        for (child, count) in children {
            self.collect_folder_and_children(child, count, out, visited)?;
        }
        Ok(())
    }

    /// GET one page of a folder list (`/me/mailFolders` or a
    /// `childFolders`, or a `nextLink` of either): each folder with its
    /// `childFolderCount`, whether every listed item had an id (one
    /// without is dropped), and the page's `@odata.nextLink`.
    fn fetch_folder_page(&self, target: &str) -> Result<FolderPage, GraphError> {
        let resp = self.send(Method::Get, target, None, &[])?;
        let v = parse_body(resp)?;
        let items = value_array(&v, "value")?;
        let mut folders = Vec::with_capacity(items.len());
        let mut all_have_ids = true;
        for item in items {
            match MailFolder::from_json(item) {
                Some(f) if !f.id.is_empty() => {
                    let child_count = item.get("childFolderCount").and_then(Value::as_i64);
                    folders.push((f, child_count));
                }
                _ => all_have_ids = false,
            }
        }
        let next_link = v
            .get("@odata.nextLink")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(FolderPage {
            folders,
            all_have_ids,
            next_link,
        })
    }

    /// GET `/me/mailFolders/{id}?$select=id`: whether the folder still
    /// exists. The sync engine asks it for a folder a complete listing no
    /// longer has before removing it (#1886): the tree walk is many
    /// requests, not a snapshot, so a folder moved during it can be missed.
    /// `Ok(false)` only on a 404; any other error is returned.
    pub fn folder_exists(&self, folder_id: &str) -> Result<bool, GraphError> {
        let path = format!(
            "/me/mailFolders/{}?$select=id",
            encode_path_segment(folder_id)
        );
        match self.send(Method::Get, &path, None, &[]) {
            Ok(_) => Ok(true),
            Err(GraphError::NotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// GET `/me/messages/{id}?$select=isDraft`: whether the message is still
    /// an unsent draft (#1874). A 404 is `Ok(false)`: a draft Graph sent
    /// moves to Sent Items under a new id, so its draft id is gone. A body
    /// without `isDraft` is a `Parse` error.
    pub fn is_draft(&self, id: &str) -> Result<bool, GraphError> {
        let path = format!("/me/messages/{}?$select=isDraft", encode_path_segment(id));
        match self.send(Method::Get, &path, None, &[]) {
            Ok(resp) => parse_body(resp)?
                .get("isDraft")
                .and_then(Value::as_bool)
                .ok_or_else(|| GraphError::Parse(format!("no isDraft for message {id}"))),
            Err(GraphError::NotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// GET `/me/mailFolders/{id}?$select=totalItemCount,unreadItemCount`:
    /// one folder's `(total, unread)` counts, which the sync engine re-reads
    /// after syncing just that folder (#1885). A missing count is 0, as in
    /// `MailFolder::from_json`.
    pub fn get_folder_counts(&self, folder_id: &str) -> Result<(i64, i64), GraphError> {
        let folder_id = encode_path_segment(folder_id);
        let path = format!("/me/mailFolders/{folder_id}?$select=totalItemCount,unreadItemCount");
        let resp = self.send(Method::Get, &path, None, &[])?;
        let v = parse_body(resp)?;
        let count = |name: &str| v.get(name).and_then(Value::as_i64).unwrap_or(0);
        Ok((count("totalItemCount"), count("unreadItemCount")))
    }

    /// GET a folder's `/messages/delta` (a first call, `DeltaCursor::Head`
    /// or `DeltaCursor::Full`) or a stored `nextLink`/`deltaLink`
    /// (`DeltaCursor::Link`), requesting pages of 50 via
    /// `Prefer: odata.maxpagesize=50`.
    ///
    /// A first call asks for the reminder too (`MESSAGE_EXPAND`, #2054),
    /// and Graph keeps that in every link it mints from it. Should Graph
    /// refuse the expand there (a 400), the same call is sent once more
    /// without it, so the folder still syncs, and the page says so
    /// (`DeltaPage::expand_refused`, its upserts `reminder_unasked`): the
    /// engine then reads no reminders for that folder until its links
    /// reset, and keeps the reminders already stored. A 400 with another
    /// cause costs that one extra request, and the second answer is
    /// returned as it is.
    pub fn delta(&self, cursor: DeltaCursor) -> Result<DeltaPage, GraphError> {
        let (target, first) = match cursor {
            DeltaCursor::Head { folder_id, since } => {
                let folder_id = encode_path_segment(&folder_id);
                let target = format!(
                    "/me/mailFolders/{folder_id}/messages/delta\
                     ?$filter=receivedDateTime%20ge%20{since}\
                     &$orderby=receivedDateTime%20desc&$select={MESSAGE_SELECT}"
                );
                (target, true)
            }
            DeltaCursor::Full(folder_id) => {
                let folder_id = encode_path_segment(&folder_id);
                let target = format!(
                    "/me/mailFolders/{folder_id}/messages/delta\
                     ?$orderby=receivedDateTime%20desc&$select={MESSAGE_SELECT}"
                );
                (target, true)
            }
            DeltaCursor::Link(url) => (url, false),
        };
        let prefer = [("Prefer", "odata.maxpagesize=50")];
        let (resp, expanded) = if first {
            self.get_with_expand(&target, &prefer)?
        } else {
            (self.send(Method::Get, &target, None, &prefer)?, true)
        };
        let v = parse_body(resp)?;
        let mut page = DeltaPage::from_json(&v)
            .ok_or_else(|| GraphError::Parse("malformed delta page".to_string()))?;
        if !expanded {
            page.expand_refused = true;
            page.mark_reminder_unasked();
        }
        Ok(page)
    }

    /// GET `/me/messages/{id}?$select=MESSAGE_SELECT&$expand=MESSAGE_EXPAND`:
    /// one message's stored properties and reminder (#2054; without the
    /// expand should Graph refuse it, as `delta`), with which of them the
    /// response carried. The
    /// sync engine completes a header-less row with it (#1899).
    pub fn get_message(&self, message_id: &str) -> Result<(Message, MessageFields), GraphError> {
        let message_id = encode_path_segment(message_id);
        let path = format!("/me/messages/{message_id}?$select={MESSAGE_SELECT}");
        let (resp, expanded) = self.get_with_expand(&path, &[])?;
        let v = parse_body(resp)?;
        let m = Message::from_json(&v)
            .ok_or_else(|| GraphError::Parse("malformed message".to_string()))?;
        let mut fields = MessageFields::of(&v);
        fields.reminder_unasked = !expanded;
        Ok((m, fields))
    }

    /// Searches messages on the server (#2006): GET
    /// `/me/messages?$search=…&$select=MESSAGE_SELECT,parentFolderId&$top=…
    /// &$expand=MESSAGE_EXPAND`, or `/me/mailFolders/{id}/messages?…` with a
    /// folder. The expand brings each hit's reminder and meeting message
    /// type (#2081); should Graph refuse it (a 400), the first request is
    /// sent once more without it, as `delta`'s is, and the hits are marked
    /// `MessageFields::reminder_unasked`. A 400 with another cause (bad KQL)
    /// costs that one extra request, and the second answer is returned as
    /// it is. A `nextLink` is sent as is (it keeps what the first request
    /// asked). `query` is KQL as
    /// the client wrote it; it goes in one quoted `$search` value
    /// (`search_value`), so a `"` or `\` in it cannot end the clause.
    /// Graph allows neither `$orderby` nor `$filter` with `$search`: the hits
    /// come in Graph's own order. `top` is clamped to
    /// `1..=`[`SEARCH_TOP_MAX`]; `@odata.nextLink` is followed (sent as is)
    /// until `top` hits are in (the rest of a page dropped), a page has no
    /// `nextLink`, or a page brings no hit (a link that loops). An item
    /// without an id is dropped.
    pub fn search_messages(
        &self,
        folder_id: Option<&str>,
        query: &str,
        top: usize,
    ) -> Result<Vec<SearchHit>, GraphError> {
        let top = top.clamp(1, SEARCH_TOP_MAX);
        let collection = match folder_id {
            Some(f) => format!("/me/mailFolders/{}/messages", encode_path_segment(f)),
            None => "/me/messages".to_string(),
        };
        let mut target = format!(
            "{collection}?$search={}&$select={MESSAGE_SELECT},parentFolderId&$top={top}",
            search_value(query)
        );
        let mut hits = Vec::new();
        let mut first = true;
        let mut expanded = true;
        loop {
            let resp = if first {
                first = false;
                let (resp, asked) = self.get_with_expand(&target, &[])?;
                expanded = asked;
                resp
            } else {
                self.send(Method::Get, &target, None, &[])?
            };
            let v = parse_body(resp)?;
            let before = hits.len();
            hits.extend(
                value_array(&v, "value")?
                    .iter()
                    .filter_map(SearchHit::from_json)
                    .map(|mut hit| {
                        hit.fields.reminder_unasked = !expanded;
                        hit
                    }),
            );
            if hits.len() >= top {
                hits.truncate(top);
                break;
            }
            match v.get("@odata.nextLink").and_then(Value::as_str) {
                Some(next) if hits.len() > before => target = next.to_string(),
                _ => break,
            }
        }
        Ok(hits)
    }

    /// GET `/me/messages/{id}?$select=MESSAGE_SELECT,parentFolderId&$expand=
    /// MESSAGE_EXPAND` (without the expand should Graph refuse it, as
    /// `delta`): one message as a [`SearchHit`], with the folder it is filed in, for the
    /// engine to store a search hit the store does not have (#2006). A body
    /// without an id is a `Parse` error.
    pub fn get_message_hit(&self, message_id: &str) -> Result<SearchHit, GraphError> {
        let message_id = encode_path_segment(message_id);
        let path = format!("/me/messages/{message_id}?$select={MESSAGE_SELECT},parentFolderId");
        let (resp, expanded) = self.get_with_expand(&path, &[])?;
        let v = parse_body(resp)?;
        let mut hit = SearchHit::from_json(&v)
            .ok_or_else(|| GraphError::Parse("malformed message".to_string()))?;
        hit.fields.reminder_unasked = !expanded;
        Ok(hit)
    }

    /// [`GraphClient::get_body_with_headers`] with the headers dropped: the
    /// body alone (a fresh draft's, whose headers mean nothing).
    pub fn get_body(&self, message_id: &str, prefer_text: bool) -> Result<Body, GraphError> {
        self.get_body_with_headers(message_id, prefer_text)
            .map(|(body, _)| body)
    }

    /// GET `/me/messages/{id}?$select=body,internetMessageHeaders`
    /// ([`BODY_SELECT`]): the body, and in the same request the message's
    /// [`STORED_HEADERS`](crate::graph::model::STORED_HEADERS) (#1933, see [`MessageHeader::list_from_json`]:
    /// none when Graph sent none). When `prefer_text` is set, asks Graph to
    /// convert the body to plain text via
    /// `Prefer: outlook.body-content-type="text"`; otherwise Graph returns
    /// its native HTML. A response over [`BODY_MAX`] is refused as a `Parse`
    /// error (#1978): the client will not hold it, which no retry changes.
    pub fn get_body_with_headers(
        &self,
        message_id: &str,
        prefer_text: bool,
    ) -> Result<(Body, Vec<MessageHeader>), GraphError> {
        let message_id = encode_path_segment(message_id);
        let path = format!("/me/messages/{message_id}?$select={BODY_SELECT}");
        let headers: &[(&str, &str)] = if prefer_text {
            &[("Prefer", "outlook.body-content-type=\"text\"")]
        } else {
            &[]
        };
        let resp = self.send(Method::Get, &path, None, headers)?;
        let v = parse_body_capped(resp, BODY_MAX)?;
        let body = v
            .get("body")
            .ok_or_else(|| GraphError::Parse("response has no body field".to_string()))?;
        let body =
            Body::from_json(body).ok_or_else(|| GraphError::Parse("malformed body".to_string()))?;
        Ok((body, MessageHeader::list_from_json(&v)))
    }

    /// GET `/me/messages/{id}/attachments` — metadata only, no bytes.
    pub fn list_attachments(&self, message_id: &str) -> Result<Vec<AttachmentMeta>, GraphError> {
        let message_id = encode_path_segment(message_id);
        let path = format!("/me/messages/{message_id}/attachments");
        let resp = self.send(Method::Get, &path, None, &[])?;
        let v = parse_body(resp)?;
        let items = value_array(&v, "value")?;
        Ok(items.iter().filter_map(AttachmentMeta::from_json).collect())
    }

    /// GET `/me/messages/{id}/attachments/{attachment_id}` and decode its
    /// `contentBytes` (standard base64, as Graph's `fileAttachment` sends
    /// it) into raw bytes.
    pub fn get_attachment_bytes(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, GraphError> {
        let message_id = encode_path_segment(message_id);
        let attachment_id = encode_path_segment(attachment_id);
        let path = format!("/me/messages/{message_id}/attachments/{attachment_id}");
        let resp = self.send(Method::Get, &path, None, &[])?;
        let v = parse_body(resp)?;
        let content_bytes = v
            .get("contentBytes")
            .and_then(Value::as_str)
            .ok_or_else(|| GraphError::Parse("response has no contentBytes field".to_string()))?;
        base64_decode(content_bytes)
            .ok_or_else(|| GraphError::Parse("contentBytes is not valid base64".to_string()))
    }

    /// GET `/me/messages/{id}/attachments/{aid}/$value` — the attachment's
    /// raw body bytes (an `itemAttachment`'s MIME), NOT JSON. Used to download
    /// a nested item as `.eml`/`.ics`; `get_attachment_bytes` handles a
    /// `fileAttachment`'s base64 `contentBytes` instead.
    pub fn get_attachment_raw_value(
        &self,
        message_id: &str,
        attachment_id: &str,
    ) -> Result<Vec<u8>, GraphError> {
        use std::io::Read;
        let message_id = encode_path_segment(message_id);
        let attachment_id = encode_path_segment(attachment_id);
        let path = format!("/me/messages/{message_id}/attachments/{attachment_id}/$value");
        let resp = self.send(Method::Get, &path, None, &[])?;
        let mut buf = Vec::new();
        resp.into_reader()
            .read_to_end(&mut buf)
            .map_err(|e| GraphError::Transport(e.to_string()))?;
        Ok(buf)
    }

    /// PATCH `/me/messages/{id}` with `{"isRead":true|false}`.
    pub fn mark_read(&self, id: &str, read: bool) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/messages/{id}");
        let body = Value::Object(vec![("isRead".to_string(), Value::Bool(read))]).to_string();
        self.send(Method::Patch, &path, Some(body), &[])?;
        Ok(())
    }

    /// PATCH `/me/messages/{id}` with `{"flag":{"flagStatus":"flagged"|"notFlagged"}}`.
    pub fn set_flag(&self, id: &str, flagged: bool) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/messages/{id}");
        let status = if flagged { "flagged" } else { "notFlagged" };
        let body = Value::Object(vec![(
            "flag".to_string(),
            Value::Object(vec![(
                "flagStatus".to_string(),
                Value::Str(status.to_string()),
            )]),
        )])
        .to_string();
        self.send(Method::Patch, &path, Some(body), &[])?;
        Ok(())
    }

    /// GETs `target` with the reminder `$expand` (#2054) appended; on a 400,
    /// sends it once more as it is (`delta`'s doc says why). The `bool` is
    /// whether the answer is to the call with the expand: when it is not,
    /// the caller marks what it parses `MessageFields::reminder_unasked`,
    /// so its lack of the reminder keeps a stored one.
    fn get_with_expand(
        &self,
        target: &str,
        headers: &[(&str, &str)],
    ) -> Result<(ureq::Response, bool), GraphError> {
        match self.send(Method::Get, &with_expand(target), None, headers) {
            Err(GraphError::Http { status: 400, .. }) => self
                .send(Method::Get, target, None, headers)
                .map(|r| (r, false)),
            other => other.map(|r| (r, true)),
        }
    }

    /// PATCH `/me/messages/{id}` with `PidLidReminderSet` false (#2054):
    /// dismisses a flagged message's reminder on the server, so the user's
    /// other devices stop showing it.
    pub fn dismiss_message_reminder(&self, id: &str) -> Result<(), GraphError> {
        self.patch_message_property(id, REMINDER_SET_PROP, "false")
    }

    /// PATCH `/me/messages/{id}` with `PidLidReminderSignalTime` set to
    /// `new_reminder_utc`, a store-shaped UTC time sent as is (#2054): the
    /// reminder fires again then. `PidLidReminderTime` is left alone, as
    /// Outlook's Snooze does.
    pub fn snooze_message_reminder(
        &self,
        id: &str,
        new_reminder_utc: &str,
    ) -> Result<(), GraphError> {
        self.patch_message_property(id, REMINDER_SIGNAL_TIME_PROP, new_reminder_utc)
    }

    /// PATCH `/me/messages/{id}` with one `singleValueExtendedProperties`
    /// entry.
    fn patch_message_property(&self, id: &str, prop: &str, value: &str) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/messages/{id}");
        let body = Value::Object(vec![(
            "singleValueExtendedProperties".to_string(),
            Value::Array(vec![Value::Object(vec![
                ("id".to_string(), Value::Str(prop.to_string())),
                ("value".to_string(), Value::Str(value.to_string())),
            ])]),
        )])
        .to_string();
        self.send(Method::Patch, &path, Some(body), &[])?;
        Ok(())
    }

    /// POST `/me/messages/{id}/move` with `{"destinationId": dest}`, returning
    /// the id of the moved message (Graph mints a new one on move).
    pub fn move_message(&self, id: &str, dest_folder: &str) -> Result<String, GraphError> {
        // Only `id` is part of the URL path and needs escaping; `dest_folder`
        // goes into the JSON body as `destinationId`, not the path.
        let encoded_id = encode_path_segment(id);
        let path = format!("/me/messages/{encoded_id}/move");
        let body = Value::Object(vec![(
            "destinationId".to_string(),
            Value::Str(dest_folder.to_string()),
        )])
        .to_string();
        let resp = self.send(Method::Post, &path, Some(body), &[])?;
        let v = parse_body(resp)?;
        v.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| GraphError::Parse("move response has no id".to_string()))
    }

    /// DELETE `/me/messages/{id}`. Graph soft-deletes the message to the
    /// mailbox's Recoverable Items, not to Deleted Items: "delete to Deleted
    /// Items" is `move_message` to that folder (`deleteditems`).
    pub fn delete_message(&self, id: &str) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/messages/{id}");
        self.send(Method::Delete, &path, None, &[])?;
        Ok(())
    }

    /// POST `/me/messages/{id}/createReply` (or `createReplyAll` when `all`
    /// is set) — Graph creates a new draft pre-populated with quoted body,
    /// subject prefix, and recipients, and returns it; parsed the same way
    /// as any other `Message`.
    pub fn create_reply(&self, id: &str, all: bool) -> Result<Message, GraphError> {
        let id = encode_path_segment(id);
        let action = if all { "createReplyAll" } else { "createReply" };
        let path = format!("/me/messages/{id}/{action}");
        let resp = self.send(Method::Post, &path, None, &[])?;
        let v = parse_body(resp)?;
        Message::from_json(&v).ok_or_else(|| GraphError::Parse("malformed reply draft".to_string()))
    }

    /// POST `/me/messages/{id}/createForward` — same shape as
    /// `create_reply`, but for forwarding.
    pub fn create_forward(&self, id: &str) -> Result<Message, GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/messages/{id}/createForward");
        let resp = self.send(Method::Post, &path, None, &[])?;
        let v = parse_body(resp)?;
        Message::from_json(&v)
            .ok_or_else(|| GraphError::Parse("malformed forward draft".to_string()))
    }

    /// POST `/me/messages` with a new draft's subject/body/recipients;
    /// Graph creates it (implicitly as a draft — no `isDraft` field needed,
    /// that's the default for a message created this way) and returns it
    /// with its minted id, which callers need for later `update_draft` /
    /// `send_draft` calls.
    pub fn create_draft(
        &self,
        body_html: &str,
        subject: &str,
        to: &[Recipient],
        cc: &[Recipient],
        bcc: &[Recipient],
    ) -> Result<Message, GraphError> {
        let body = draft_body_json(body_html, subject, to, cc, bcc);
        let resp = self.send(Method::Post, "/me/messages", Some(body), &[])?;
        let v = parse_body(resp)?;
        Message::from_json(&v).ok_or_else(|| GraphError::Parse("malformed draft".to_string()))
    }

    /// PATCH `/me/messages/{id}` with the same subject/body/recipients
    /// shape as `create_draft`, overwriting an existing draft in place.
    pub fn update_draft(
        &self,
        id: &str,
        body_html: &str,
        subject: &str,
        to: &[Recipient],
        cc: &[Recipient],
        bcc: &[Recipient],
    ) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/messages/{id}");
        let body = draft_body_json(body_html, subject, to, cc, bcc);
        self.send(Method::Patch, &path, Some(body), &[])?;
        Ok(())
    }

    /// POST `/me/messages/{id}/send` — hands the draft to Graph for
    /// delivery. Graph replies 202 (queued for sending) or occasionally 200;
    /// both are 2xx, so `send`'s existing `Ok(resp)` path covers them with
    /// no extra mapping here.
    pub fn send_draft(&self, id: &str) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/messages/{id}/send");
        self.send(Method::Post, &path, None, &[])?;
        Ok(())
    }

    /// GET `/me/calendarView?startDateTime=&endDateTime=&$top=50`: the
    /// default calendar's occurrences in the range (`calendar_view_paged`).
    pub fn calendar_view(&self, from_utc: &str, to_utc: &str) -> Result<Vec<Event>, GraphError> {
        self.calendar_view_paged(format!(
            "/me/calendarView?startDateTime={from_utc}&endDateTime={to_utc}&$top=50"
        ))
    }

    /// GET `/me/calendars/{id}/calendarView?startDateTime=&endDateTime=&$top=50`:
    /// one calendar's occurrences in the range (#1984), the id
    /// percent-encoded as a path segment. Each returned event's
    /// `calendar_id` is `calendar_id`: the URL decides which calendar an
    /// event is in, not the JSON's `calendar.id`.
    pub fn calendar_view_in(
        &self,
        calendar_id: &str,
        from_utc: &str,
        to_utc: &str,
    ) -> Result<Vec<Event>, GraphError> {
        let id = encode_path_segment(calendar_id);
        let mut events = self.calendar_view_paged(format!(
            "/me/calendars/{id}/calendarView?startDateTime={from_utc}&endDateTime={to_utc}&$top=50"
        ))?;
        for e in &mut events {
            e.calendar_id = calendar_id.to_string();
        }
        Ok(events)
    }

    /// GET one page of a calendar's `calendarView/delta` (#2013): for
    /// `Start`, `/me/calendars/{encoded id}/calendarView/delta?startDateTime=
    /// &endDateTime=` (no `$top` or `$select`: Graph's calendarView delta
    /// refuses both), and each upserted event's `calendar_id` is that
    /// calendar (the URL decides, as in `calendar_view_in`); for `Link`, the
    /// URL as is, and `calendar_id` is left for the caller to set. Every
    /// request, a link's too, sends `Prefer: outlook.timezone="UTC",
    /// odata.maxpagesize=50`: a deltaLink does not keep the time zone. A
    /// page without a `value` array, or with neither a `nextLink` nor a
    /// `deltaLink` (a round that could never finish), is a `Parse` error.
    pub fn calendar_delta(
        &self,
        cursor: CalendarDeltaCursor,
    ) -> Result<EventDeltaPage, GraphError> {
        let (target, calendar_id) = match cursor {
            CalendarDeltaCursor::Start {
                calendar_id,
                from_utc,
                to_utc,
            } => {
                let id = encode_path_segment(&calendar_id);
                (
                    format!(
                        "/me/calendars/{id}/calendarView/delta\
                         ?startDateTime={from_utc}&endDateTime={to_utc}"
                    ),
                    Some(calendar_id),
                )
            }
            CalendarDeltaCursor::Link(url) => (url, None),
        };
        let resp = self.send(
            Method::Get,
            &target,
            None,
            &[("Prefer", CALENDAR_DELTA_PREFER)],
        )?;
        let v = parse_body(resp)?;
        let mut page = EventDeltaPage::from_json(&v)
            .ok_or_else(|| GraphError::Parse("malformed calendar delta page".to_string()))?;
        if page.next_link.is_none() && page.delta_link.is_none() {
            return Err(GraphError::Parse(
                "calendar delta page without a nextLink or deltaLink".to_string(),
            ));
        }
        if let Some(calendar_id) = calendar_id {
            for item in &mut page.items {
                if let EventDeltaItem::Upsert(e) = item {
                    e.calendar_id = calendar_id.clone();
                }
            }
        }
        Ok(page)
    }

    /// GET `/me/events/{id}/instances?startDateTime=&endDateTime=&$top=50`
    /// (#2022): a series' occurrences in the range, exceptions included, the
    /// master's id percent-encoded as a path segment, with `calendar_view`'s
    /// UTC `Prefer` and paging (`calendar_view_paged`). `calendar_id` is what
    /// the JSON gave (Graph sends no `calendar` unless expanded, so `""`).
    pub fn event_instances(
        &self,
        master_id: &str,
        from_utc: &str,
        to_utc: &str,
    ) -> Result<Vec<Event>, GraphError> {
        let id = encode_path_segment(master_id);
        self.calendar_view_paged(format!(
            "/me/events/{id}/instances?startDateTime={from_utc}&endDateTime={to_utc}&$top=50"
        ))
    }

    /// GET `/me/events/{id}` with the UTC `Prefer` (#2022): one event as
    /// Graph holds it, a series master with its `recurrence`
    /// (`calendarView` only lists occurrences). An answer without a
    /// non-empty `id` is a `Parse` error.
    pub fn get_event(&self, id: &str) -> Result<Event, GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/events/{id}");
        let resp = self.send(
            Method::Get,
            &path,
            None,
            &[("Prefer", "outlook.timezone=\"UTC\"")],
        )?;
        let v = parse_body(resp)?;
        Event::from_json(&v)
            .filter(|e| !e.id.is_empty())
            .ok_or_else(|| GraphError::Parse("malformed event".to_string()))
    }

    /// Sends `Prefer: outlook.timezone="UTC"` so every returned event's
    /// `start`/`end` is already a UTC wall-clock time (see `Event::from_json`
    /// / `model::to_utc` for the fixed-width normalization this enables),
    /// and follows `@odata.nextLink` (a relative path or a full URL, either
    /// way `full_url` resolves it) until Graph stops sending one.
    fn calendar_view_paged(&self, first: String) -> Result<Vec<Event>, GraphError> {
        let mut target = first;
        let mut events = Vec::new();
        loop {
            let resp = self.send(
                Method::Get,
                &target,
                None,
                &[("Prefer", "outlook.timezone=\"UTC\"")],
            )?;
            let v = parse_body(resp)?;
            let items = value_array(&v, "value")?;
            events.extend(items.iter().filter_map(Event::from_json));
            match v.get("@odata.nextLink").and_then(Value::as_str) {
                Some(next) => target = next.to_string(),
                None => break,
            }
        }
        Ok(events)
    }

    /// GET `/me/calendars` (#1984): the user's calendars in Graph's order,
    /// 100 a page, following `@odata.nextLink`. An item without an `id` is
    /// skipped. Any page's failure is an `Err`, never a partial list, so
    /// `Store::replace_calendars` never drops a calendar it did not see.
    pub fn list_calendars(&self) -> Result<Vec<Calendar>, GraphError> {
        let mut target =
            "/me/calendars?$select=id,name,color,hexColor,isDefaultCalendar,canEdit,owner&$top=100"
                .to_string();
        let mut out = Vec::new();
        loop {
            let resp = self.send(Method::Get, &target, None, &[])?;
            let v = parse_body(resp)?;
            out.extend(
                value_array(&v, "value")?
                    .iter()
                    .filter_map(Calendar::from_json),
            );
            match v.get("@odata.nextLink").and_then(Value::as_str) {
                Some(next) => target = next.to_string(),
                None => break,
            }
        }
        Ok(out)
    }

    /// POST `/me/calendar/getSchedule` — the availability of each address in
    /// `schedules` over `[start_utc, end_utc)` at `interval_minutes` slots.
    /// Returns each schedule's `availabilityView` digit string.
    pub fn get_schedule(
        &self,
        schedules: &[String],
        start_utc: &str,
        end_utc: &str,
        interval_minutes: i64,
    ) -> Result<Vec<ScheduleEntry>, GraphError> {
        let body = Value::Object(vec![
            (
                "schedules".to_string(),
                Value::Array(schedules.iter().map(|s| Value::Str(s.clone())).collect()),
            ),
            ("startTime".to_string(), graph_utc_wall_clock(start_utc)),
            ("endTime".to_string(), graph_utc_wall_clock(end_utc)),
            (
                "availabilityViewInterval".to_string(),
                Value::Num(interval_minutes as f64),
            ),
        ])
        .to_string();
        let resp = self.send(Method::Post, "/me/calendar/getSchedule", Some(body), &[])?;
        let v = parse_body(resp)?;
        let items = value_array(&v, "value")?;
        Ok(items.iter().filter_map(ScheduleEntry::from_json).collect())
    }

    /// POST `/me/events/{id}/accept|decline|tentativelyAccept` with
    /// `{"comment":…, "sendResponse":…}` — `comment` defaults to `""` when
    /// `None` rather than omitting the field, so the body shape is the same
    /// for every call. With `send_response` false Graph tells the organizer
    /// nothing and ignores the comment. `proposed` (UTC start and end) goes
    /// out as `proposedNewTime`, its `dateTime`s without the `Z` next to
    /// `timeZone:"UTC"` as `event_body_json` sends them, on decline and
    /// tentative only: Graph's `accept` takes no proposal, so an accept drops
    /// it. Callers check a proposal first with [`rsvp_proposal`].
    pub fn respond_event(
        &self,
        id: &str,
        kind: RsvpKind,
        comment: Option<&str>,
        send_response: bool,
        proposed: Option<(String, String)>,
    ) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let action = match kind {
            RsvpKind::Accept => "accept",
            RsvpKind::Decline => "decline",
            RsvpKind::Tentative => "tentativelyAccept",
        };
        let path = format!("/me/events/{id}/{action}");
        let mut obj = vec![
            (
                "comment".to_string(),
                Value::Str(comment.unwrap_or("").to_string()),
            ),
            ("sendResponse".to_string(), Value::Bool(send_response)),
        ];
        if let Some((start, end)) = proposed.as_ref().filter(|_| kind != RsvpKind::Accept) {
            obj.push((
                "proposedNewTime".to_string(),
                Value::Object(vec![
                    ("start".to_string(), graph_utc_wall_clock(start)),
                    ("end".to_string(), graph_utc_wall_clock(end)),
                ]),
            ));
        }
        let body = Value::Object(obj).to_string();
        self.send(Method::Post, &path, Some(body), &[])?;
        Ok(())
    }

    /// GET `/me/messages/{id}?$expand=microsoft.graph.eventMessageRequest/event($select=id)`
    /// — an `eventMessageRequest` message exposes the calendar event it invites
    /// to via its `event` navigation property; expanding it (selecting only
    /// `id`) is enough to then `respond_event` on that event. Returns the
    /// expanded `event.id`, or `None` when the message carries no `event`
    /// (not an invite, or an odd server state) so the caller can surface a
    /// "not a meeting invite" notice rather than a hard error.
    pub fn meeting_event_id(&self, message_id: &str) -> Result<Option<String>, GraphError> {
        let id = encode_path_segment(message_id);
        let path = format!(
            "/me/messages/{id}?$expand=microsoft.graph.eventMessageRequest/event($select=id)"
        );
        let resp = self.send(Method::Get, &path, None, &[])?;
        let v = parse_body(resp)?;
        Ok(v.get("event")
            .and_then(|e| e.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    /// GET `/me/messages/{id}?$expand=microsoft.graph.eventMessage/event($select=id,isOrganizer)`
    /// (#2024): a meeting message's `meetingMessageType` and the calendar
    /// event it refers to. The `eventMessage` cast covers requests,
    /// cancellations and responses (`eventMessageRequest`,
    /// `eventMessageResponse` derive from it); a message of another type
    /// carries neither, and reads as all `None`. No top-level `$select`, as
    /// in `meeting_event_id`: `meetingMessageType` is a derived-type
    /// property, which a plain GET returns. `RespondMeeting` keeps
    /// `meeting_event_id`'s narrower `eventMessageRequest` cast.
    pub fn meeting_message(&self, message_id: &str) -> Result<MeetingMessage, GraphError> {
        let id = encode_path_segment(message_id);
        let path = format!(
            "/me/messages/{id}?$expand=microsoft.graph.eventMessage/event($select=id,isOrganizer)"
        );
        let resp = self.send(Method::Get, &path, None, &[])?;
        let v = parse_body(resp)?;
        Ok(MeetingMessage::from_json(&v))
    }

    /// GET `/me/outlook/masterCategories` — the mailbox's master category list.
    pub fn get_master_categories(&self) -> Result<Vec<MasterCategory>, GraphError> {
        let resp = self.send(Method::Get, "/me/outlook/masterCategories", None, &[])?;
        let v = parse_body(resp)?;
        let items = value_array(&v, "value")?;
        Ok(items.iter().filter_map(MasterCategory::from_json).collect())
    }

    /// PATCH `/me/messages/{id}` with `{"categories": [ …names… ]}` — sets the
    /// message's assigned category names (an empty slice clears them).
    pub fn set_message_categories(
        &self,
        id: &str,
        categories: &[String],
    ) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/messages/{id}");
        let arr = Value::Array(categories.iter().map(|c| Value::Str(c.clone())).collect());
        let body = Value::Object(vec![("categories".to_string(), arr)]).to_string();
        self.send(Method::Patch, &path, Some(body), &[])?;
        Ok(())
    }

    /// GET `/me?$select=displayName,mail,userPrincipalName`: who is signed in.
    /// Needs only `User.Read`.
    pub fn me(&self) -> Result<Me, GraphError> {
        let resp = self.send(
            Method::Get,
            "/me?$select=displayName,mail,userPrincipalName",
            None,
            &[],
        )?;
        Ok(Me::from_json(&parse_body(resp)?))
    }

    /// The signed-in user's photo as `(bytes, content type)`: GET
    /// `/me/photo/$value`, and on a 404 `/me/photos/48x48/$value`. `Ok(None)`
    /// when both are 404 (the user has no photo: the client draws initials).
    /// Any other error, on either request, is returned as it is, with no
    /// fallback. A photo over [`PHOTO_MAX`] bytes is refused as a `Parse`
    /// error.
    pub fn my_photo(&self) -> Result<Option<(Vec<u8>, String)>, GraphError> {
        match self.get_photo("/me/photo/$value") {
            Err(GraphError::NotFound) => {}
            other => return other.map(Some),
        }
        match self.get_photo("/me/photos/48x48/$value") {
            Err(GraphError::NotFound) => Ok(None),
            other => other.map(Some),
        }
    }

    /// One photo `$value` request: binary, so it asks for `image/*` (the
    /// header replaces `send`'s JSON `Accept`), refuses a `Content-Length`
    /// over [`PHOTO_MAX`] before reading, and reads at most `PHOTO_MAX` bytes
    /// whatever the header said. The content type is the `Content-Type`
    /// header without its parameters, `application/octet-stream` when absent.
    fn get_photo(&self, path: &str) -> Result<(Vec<u8>, String), GraphError> {
        let resp = self.send(Method::Get, path, None, &[("Accept", "image/*")])?;
        let content_type = resp
            .header("Content-Type")
            .map(|v| v.split(';').next().unwrap_or("").trim())
            .filter(|v| !v.is_empty())
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = read_response_capped(resp, PHOTO_MAX, photo_too_large)?;
        Ok((bytes, content_type))
    }

    /// GET `/me/mailboxSettings` and parse its `automaticRepliesSetting` into
    /// an [`AutomaticReplies`].
    pub fn get_automatic_replies(&self) -> Result<AutomaticReplies, GraphError> {
        let resp = self.send(Method::Get, "/me/mailboxSettings", None, &[])?;
        let v = parse_body(resp)?;
        AutomaticReplies::from_json(&v)
            .ok_or_else(|| GraphError::Parse("no automaticRepliesSetting".to_string()))
    }

    /// PATCH `/me/mailboxSettings` with the automatic-replies configuration.
    /// The reply messages are `plain_to_html`-encoded; the scheduled datetimes
    /// are sent ONLY when `status == Scheduled` (a non-scheduled write that
    /// carried them would make Graph reject the PATCH or flip to scheduled).
    pub fn set_automatic_replies(&self, r: &AutomaticReplies) -> Result<(), GraphError> {
        self.send(
            Method::Patch,
            "/me/mailboxSettings",
            Some(automatic_replies_body(r)),
            &[],
        )?;
        Ok(())
    }

    /// POST `/me/events` with the event body; returns the created `Event`
    /// (with its Graph-minted id). Graph sends invites to any attendees.
    pub fn create_event(&self, input: &EventInput) -> Result<Event, GraphError> {
        self.create_event_at("/me/events", input)
    }

    /// `create_event` in the given calendar (#2048): POST
    /// `/me/calendars/{encoded id}/events` with the same body. The returned
    /// event's `calendar_id` is `calendar_id`: the URL decides, as in
    /// `calendar_view_in`.
    pub fn create_event_in(
        &self,
        calendar_id: &str,
        input: &EventInput,
    ) -> Result<Event, GraphError> {
        let path = format!("/me/calendars/{}/events", encode_path_segment(calendar_id));
        let mut e = self.create_event_at(&path, input)?;
        e.calendar_id = calendar_id.to_string();
        Ok(e)
    }

    /// The POST `create_event` and `create_event_in` share.
    fn create_event_at(&self, path: &str, input: &EventInput) -> Result<Event, GraphError> {
        let resp = self.send(Method::Post, path, Some(event_body_json(input)), &[])?;
        let v = parse_body(resp)?;
        Event::from_json(&v).ok_or_else(|| GraphError::Parse("malformed created event".to_string()))
    }

    /// PATCH `/me/events/{id}` with the groups in `input.fields` (the same
    /// body shape as `create_event`, #1994). Graph answers with 200 and the
    /// updated event; an answer without one (an empty 204, or a body with no
    /// `id`) is `Ok(None)`, never an error: the PATCH was applied.
    pub fn update_event(&self, id: &str, input: &EventInput) -> Result<Option<Event>, GraphError> {
        self.update_event_body(id, event_body_json(input))
    }

    /// `update_event` with the PATCH body already serialized
    /// (`event_body_json`): what a queued series edit stores and sends
    /// (#2022, `OutboxOp::UpdateEventSeries`), as the master it edits is not
    /// stored. The same answer rule as `update_event`.
    pub fn update_event_body(&self, id: &str, body: String) -> Result<Option<Event>, GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/events/{id}");
        let resp = self.send(Method::Patch, &path, Some(body), &[])?;
        Ok(parse_body(resp)
            .ok()
            .and_then(|v| Event::from_json(&v))
            .filter(|e| !e.id.is_empty()))
    }

    /// DELETE `/me/events/{id}` (cancels an organized event / removes an
    /// invited one from the user's calendar).
    pub fn delete_event(&self, id: &str) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/events/{id}");
        self.send(Method::Delete, &path, None, &[])?;
        Ok(())
    }

    /// POST `/me/events/{id}/cancel` with `{"comment":…}` (#1985): the
    /// organizer cancels a meeting, and Graph sends every attendee a
    /// cancellation carrying `comment` and deletes the organizer's copy.
    /// Only the organizer may call it. `comment` is `""` for `None`, as in
    /// `respond_event`. Graph answers `202` with no body.
    pub fn cancel_event(&self, id: &str, comment: Option<&str>) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/events/{id}/cancel");
        let body = Value::Object(vec![(
            "comment".to_string(),
            Value::Str(comment.unwrap_or("").to_string()),
        )])
        .to_string();
        self.send(Method::Post, &path, Some(body), &[])?;
        Ok(())
    }

    /// POST `/me/events/{id}/forward` with `{"toRecipients":[…],
    /// "comment":…}` (#1985): sends the meeting to `to`, organizer or
    /// attendee alike. `comment` is `""` for `None`. Graph answers `202` with
    /// no body.
    pub fn forward_event(
        &self,
        id: &str,
        to: &[Recipient],
        comment: Option<&str>,
    ) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/events/{id}/forward");
        let body = Value::Object(vec![
            ("toRecipients".to_string(), recipients_json(to)),
            (
                "comment".to_string(),
                Value::Str(comment.unwrap_or("").to_string()),
            ),
        ])
        .to_string();
        self.send(Method::Post, &path, Some(body), &[])?;
        Ok(())
    }

    /// POST `/me/events/{id}/dismissReminder` (#2025): dismisses the event's
    /// reminder on the server, so the user's other devices stop showing it.
    /// Graph takes no request body; `send` frames the empty one as
    /// `Content-Length: 0` (#1954). Graph answers `200` with no body.
    pub fn dismiss_reminder(&self, id: &str) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/events/{id}/dismissReminder");
        self.send(Method::Post, &path, None, &[])?;
        Ok(())
    }

    /// POST `/me/events/{id}/snoozeReminder` with `{"newReminderTime":…}`
    /// (#2025): the reminder fires again at `new_reminder_utc`, a store-shaped
    /// UTC time (`YYYY-MM-DDTHH:MM:SSZ`) sent as its wall clock next to
    /// `timeZone: "UTC"` (`graph_utc_wall_clock`). Graph answers `200` with no
    /// body.
    pub fn snooze_reminder(&self, id: &str, new_reminder_utc: &str) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/events/{id}/snoozeReminder");
        let body = Value::Object(vec![(
            "newReminderTime".to_string(),
            graph_utc_wall_clock(new_reminder_utc),
        )])
        .to_string();
        self.send(Method::Post, &path, Some(body), &[])?;
        Ok(())
    }

    /// GET `/me/people` (top 200, relevance-ordered). Each returned person's
    /// primary address is its first `scoredEmailAddresses` entry; people with
    /// no email address are skipped. `rank` is the entry's position in the
    /// original response order, preserved across the skips. Requires the
    /// `People.Read` scope — a token without it yields a 403, which surfaces
    /// as an `Err` for the caller to degrade on.
    pub fn people(&self) -> Result<Vec<Person>, GraphError> {
        let resp = self.send(
            Method::Get,
            "/me/people?$top=200&$select=displayName,scoredEmailAddresses",
            None,
            &[],
        )?;
        let v = parse_body(resp)?;
        let items = value_array(&v, "value")?;
        let people = items
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let addr = p
                    .get("scoredEmailAddresses")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(|e| e.get("address"))
                    .and_then(Value::as_str)?;
                let name = p.get("displayName").and_then(Value::as_str).unwrap_or("");
                Some(Person {
                    name: name.to_string(),
                    address: addr.to_string(),
                    rank: i as i64,
                })
            })
            .collect();
        Ok(people)
    }

    /// GET `/me/contacts` (the default Contacts folder, #1934, #2063), 200 a
    /// page, following `@odata.nextLink` (a relative path or a full URL, as
    /// `calendar_view` does) until Graph stops sending one. Returns every
    /// contact as a record with the fields a contact form edits
    /// (`CONTACT_SELECT`); an item without an `id` is skipped. Any page's
    /// failure is an `Err`, never a partial list, so a caller that replaces
    /// its stored set never drops contacts it did not get to see. Requires
    /// `Contacts.Read`, which `AuthConfig::default()` does not ask for: a
    /// host that wants `Store::is_contact` or the records adds it to its
    /// scope, and a token without it gets a 403 here.
    pub fn contacts(&self) -> Result<Vec<FolderContact>, GraphError> {
        let mut target = format!("/me/contacts?$top=200&$select={CONTACT_SELECT}");
        let mut out = Vec::new();
        loop {
            let resp = self.send(Method::Get, &target, None, &[])?;
            let v = parse_body(resp)?;
            out.extend(
                value_array(&v, "value")?
                    .iter()
                    .filter_map(FolderContact::from_json),
            );
            match v.get("@odata.nextLink").and_then(Value::as_str) {
                Some(next) => target = next.to_string(),
                None => break,
            }
        }
        Ok(out)
    }

    /// GET `/me/contacts/{id}` with the same `$select` as `contacts` (#2063):
    /// one record, to bring a contact back to the server's version after a
    /// refused write. A 404 is `NotFound`.
    pub fn contact(&self, id: &str) -> Result<FolderContact, GraphError> {
        let path = format!(
            "/me/contacts/{}?$select={CONTACT_SELECT}",
            encode_path_segment(id)
        );
        let resp = self.send(Method::Get, &path, None, &[])?;
        let v = parse_body(resp)?;
        FolderContact::from_json(&v).ok_or_else(|| GraphError::Parse("malformed contact".into()))
    }

    /// POST `/me/contacts` with the properties of `fields` that hold
    /// something (#2063); returns the created contact as Graph answers it
    /// (its id, `changeKey`, and the `displayName` / `fileAs` Graph
    /// computes). Requires `Contacts.ReadWrite`: a token with only
    /// `Contacts.Read` gets `Http { status: 403, .. }`.
    pub fn create_contact(&self, fields: &ContactFields) -> Result<FolderContact, GraphError> {
        let body = contact_body_json(fields, ContactFieldSet::non_empty(fields));
        let resp = self.send(Method::Post, "/me/contacts", Some(body), &[])?;
        let v = parse_body(resp)?;
        FolderContact::from_json(&v)
            .ok_or_else(|| GraphError::Parse("malformed created contact".into()))
    }

    /// PATCH `/me/contacts/{id}` with only the properties in `set`, each with
    /// its value in `fields` (#2063). An empty set sends nothing. Graph's
    /// answer body is not read: the PATCH was applied. Requires
    /// `Contacts.ReadWrite`.
    pub fn update_contact(
        &self,
        id: &str,
        fields: &ContactFields,
        set: ContactFieldSet,
    ) -> Result<(), GraphError> {
        if set.is_empty() {
            return Ok(());
        }
        let path = format!("/me/contacts/{}", encode_path_segment(id));
        self.send(
            Method::Patch,
            &path,
            Some(contact_body_json(fields, set)),
            &[],
        )?;
        Ok(())
    }

    /// DELETE `/me/contacts/{id}` (#2063). A 404 is `NotFound`, as every
    /// call returns it: the outbox treats it as done. Requires
    /// `Contacts.ReadWrite`.
    pub fn delete_contact(&self, id: &str) -> Result<(), GraphError> {
        let path = format!("/me/contacts/{}", encode_path_segment(id));
        self.send(Method::Delete, &path, None, &[])?;
        Ok(())
    }

    /// POST `/me/messages/{id}/attachments` with an inline
    /// `#microsoft.graph.fileAttachment` (base64 `contentBytes`). For files
    /// ≤ 3MB; larger files go through `upload_large_attachment` (see
    /// `add_attachment`).
    pub fn add_file_attachment(
        &self,
        message_id: &str,
        name: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<(), GraphError> {
        self.post_file_attachment(message_id, name, content_type, bytes, None)
    }

    /// `add_file_attachment`, with `content_id` `Some` making it an inline
    /// attachment (#1949): `isInline: true` and `contentId` follow the other
    /// keys. With `None` the body is exactly `add_file_attachment`'s.
    fn post_file_attachment(
        &self,
        message_id: &str,
        name: &str,
        content_type: &str,
        bytes: &[u8],
        content_id: Option<&str>,
    ) -> Result<(), GraphError> {
        let id = encode_path_segment(message_id);
        let path = format!("/me/messages/{id}/attachments");
        let mut fields = vec![
            (
                "@odata.type".to_string(),
                Value::Str("#microsoft.graph.fileAttachment".to_string()),
            ),
            ("name".to_string(), Value::Str(name.to_string())),
            (
                "contentType".to_string(),
                Value::Str(content_type.to_string()),
            ),
            ("contentBytes".to_string(), Value::Str(base64_encode(bytes))),
        ];
        push_inline_fields(&mut fields, content_id);
        let body = Value::Object(fields).to_string();
        self.send(Method::Post, &path, Some(body), &[])?;
        Ok(())
    }

    /// Attaches `bytes` to the draft `message_id`, routing by size: inline
    /// `fileAttachment` for ≤ `INLINE_MAX`, an upload session (chunked PUT) for
    /// larger files.
    pub fn add_attachment(
        &self,
        message_id: &str,
        name: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<(), GraphError> {
        self.add_attachment_with(message_id, name, content_type, bytes, None)
    }

    /// `add_attachment`, with `content_id` `Some` making the attachment
    /// inline (#1949): both paths, the small POST and the upload session's
    /// `AttachmentItem`, send `isInline: true` and `contentId`. With `None`
    /// it is `add_attachment`, request bodies unchanged.
    pub fn add_attachment_with(
        &self,
        message_id: &str,
        name: &str,
        content_type: &str,
        bytes: &[u8],
        content_id: Option<&str>,
    ) -> Result<(), GraphError> {
        self.upload_attachment(
            message_id,
            name,
            content_type,
            bytes,
            content_id,
            None,
            &mut (),
        )
    }

    /// `add_attachment_with`, reporting to `obs` as it runs (#1941): one
    /// `progress(total, total)` once a small (≤ `INLINE_MAX`) attachment's
    /// POST succeeded, and for a larger one `session` when an upload session
    /// opens or the stored one is no good any more, and `progress` after
    /// each chunk Graph accepted. `resume_url` (only a larger attachment's)
    /// is an upload session of these same bytes, name, type and content id
    /// opened by an earlier attempt: the caller checks that, so the upload
    /// asks it where to go on from rather than starting a new session (see
    /// `upload_large_attachment`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upload_attachment(
        &self,
        message_id: &str,
        name: &str,
        content_type: &str,
        bytes: &[u8],
        content_id: Option<&str>,
        resume_url: Option<&str>,
        obs: &mut dyn UploadObserver,
    ) -> Result<(), GraphError> {
        if bytes.len() <= INLINE_MAX {
            self.post_file_attachment(message_id, name, content_type, bytes, content_id)?;
            let total = bytes.len() as u64;
            obs.progress(total, total);
            Ok(())
        } else {
            self.upload_large_attachment(
                message_id,
                name,
                content_type,
                bytes,
                content_id,
                resume_url,
                obs,
            )
        }
    }

    /// Uploads `bytes` through an upload session: POST
    /// `.../attachments/createUploadSession`, then PUT the bytes to the
    /// returned pre-authenticated `uploadUrl` in `CHUNK_SIZE` chunks, each
    /// with a `Content-Range` header, until the last chunk completes.
    ///
    /// With a `resume_url` (#1941) it first GETs that session
    /// (`upload_session_next`): a live one says where Graph is, which is
    /// reported as `progress(start, total)`, and the PUTs go on from there
    /// with no new session. A GET that fails in any way opens a new session
    /// from byte 0. So does a refusal (a 4xx other than 429) of a resumed
    /// PUT, in the same attempt; the same refusal of a new session's PUT is
    /// returned. Either refusal also tells `obs` to forget the session
    /// (`session(None)`), so no later attempt resumes it. A 5xx, a 429/503
    /// throttle or a transport failure is returned with the session kept:
    /// the next attempt resumes it. A new session's url goes to
    /// `obs.session(Some(url))` before its first PUT.
    #[allow(clippy::too_many_arguments)]
    fn upload_large_attachment(
        &self,
        message_id: &str,
        name: &str,
        content_type: &str,
        bytes: &[u8],
        content_id: Option<&str>,
        resume_url: Option<&str>,
        obs: &mut dyn UploadObserver,
    ) -> Result<(), GraphError> {
        if let Some(url) = resume_url
            && let Some(start) = self.upload_session_next(url, bytes.len())
        {
            obs.progress(start as u64, bytes.len() as u64);
            match self.put_chunks(url, bytes, start, obs) {
                Err(e) if session_refused(&e) => obs.session(None),
                done => return done,
            }
        }
        let id = encode_path_segment(message_id);
        let path = format!("/me/messages/{id}/attachments/createUploadSession");
        let mut item = vec![
            ("attachmentType".to_string(), Value::Str("file".to_string())),
            ("name".to_string(), Value::Str(name.to_string())),
            ("size".to_string(), Value::Num(bytes.len() as f64)),
            (
                "contentType".to_string(),
                Value::Str(content_type.to_string()),
            ),
        ];
        push_inline_fields(&mut item, content_id);
        let body =
            Value::Object(vec![("AttachmentItem".to_string(), Value::Object(item))]).to_string();
        let resp = self.send(Method::Post, &path, Some(body), &[])?;
        let v = parse_body(resp)?;
        let upload_url = v
            .get("uploadUrl")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                GraphError::Parse("createUploadSession response has no uploadUrl".to_string())
            })?
            .to_string();
        obs.session(Some(&upload_url));
        match self.put_chunks(&upload_url, bytes, 0, obs) {
            Err(e) if session_refused(&e) => {
                obs.session(None);
                Err(e)
            }
            done => done,
        }
    }

    /// PUTs `bytes[start..]` to the session `upload_url` in `CHUNK_SIZE`
    /// chunks, reporting `progress(end, total)` after each one Graph
    /// accepted.
    fn put_chunks(
        &self,
        upload_url: &str,
        bytes: &[u8],
        start: usize,
        obs: &mut dyn UploadObserver,
    ) -> Result<(), GraphError> {
        let total = bytes.len();
        for (start, end) in chunk_ranges(start, total, CHUNK_SIZE) {
            let range = format!("bytes {}-{}/{}", start, end - 1, total);
            self.put_bytes(upload_url, &range, &bytes[start..end])?;
            obs.progress(end as u64, total as u64);
        }
        Ok(())
    }

    /// Where the upload session `upload_url` of a `total`-byte attachment
    /// goes on from (#1941): a raw `GET` with NO bearer, as `put_bytes`, and
    /// the lowest start in its `nextExpectedRanges` (`"start-"` or
    /// `"start-end"`). `None` for any failure, any status, a body that does
    /// not parse, no ranges, or a start that is not below `total`: the
    /// caller then opens a new session.
    fn upload_session_next(&self, upload_url: &str, total: usize) -> Option<usize> {
        let resp = ureq::get(upload_url)
            .set("Accept", "application/json")
            .call()
            .ok()?;
        let v = parse_body(resp).ok()?;
        let start = v
            .get("nextExpectedRanges")?
            .as_array()?
            .iter()
            .map(|r| {
                let r = r.as_str()?;
                r.split('-').next()?.trim().parse::<usize>().ok()
            })
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .min()?;
        (start < total).then_some(start)
    }

    /// Raw `PUT` of one `chunk` to a Graph upload-session `upload_url` with a
    /// `Content-Range` header and NO bearer (the session URL is already
    /// authorized). A 2xx (202 for an accepted intermediate chunk, 200/201 for
    /// the final one) is Ok; anything else maps to a `GraphError`, a 401 or
    /// a 404 as `Http` (#1941): they are the session's (its own
    /// authorization lapsed, or it is gone), not the signed-in user's, so
    /// the engine neither refreshes the token nor asks for a sign-in, and
    /// `session_refused` sees them.
    fn put_bytes(
        &self,
        upload_url: &str,
        content_range: &str,
        chunk: &[u8],
    ) -> Result<(), GraphError> {
        match ureq::put(upload_url)
            .set("Content-Range", content_range)
            .send_bytes(chunk)
        {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(status @ (401 | 404), resp)) => Err(GraphError::Http {
                status,
                body: resp.into_string().unwrap_or_default(),
            }),
            Err(ureq::Error::Status(status, resp)) => Err(classify_status(status, resp)),
            Err(ureq::Error::Transport(t)) => Err(GraphError::Transport(t.to_string())),
        }
    }

    /// Builds and sends one request, applying the standard auth/accept
    /// headers plus any `extra_headers`, and maps a non-2xx result to a
    /// `GraphError`. `path` is either a path rooted at `base` (most calls)
    /// or an already-complete URL (a delta `nextLink`/`deltaLink`).
    fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<String>,
        extra_headers: &[(&str, &str)],
    ) -> Result<ureq::Response, GraphError> {
        let url = self.full_url(path);
        let mut req = match method {
            Method::Get => ureq::get(&url),
            Method::Patch => ureq::patch(&url),
            Method::Post => ureq::post(&url),
            Method::Delete => ureq::delete(&url),
        };
        req = req
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/json");
        for (name, value) in extra_headers {
            req = req.set(name, value);
        }

        // A POST (`createReply`, `createForward`, `send`) or PATCH with no
        // body still sends an empty one: ureq's `call()` writes no
        // `Content-Length` at all, and Exchange Online front ends answer such
        // a POST with `411 Length Required` (#1954). `send_bytes(&[])` is
        // framed by ureq as `Content-Length: 0`. GET and DELETE stay bodiless.
        let result = match (&body, method) {
            (Some(b), _) => req.set("Content-Type", "application/json").send_string(b),
            (None, Method::Post | Method::Patch) => req.send_bytes(&[]),
            (None, Method::Get | Method::Delete) => req.call(),
        };

        match result {
            Ok(resp) => Ok(resp),
            Err(ureq::Error::Status(status, resp)) => Err(classify_status(status, resp)),
            Err(ureq::Error::Transport(t)) => Err(GraphError::Transport(t.to_string())),
        }
    }

    /// `path` is used verbatim when it's already a complete URL (a delta
    /// link); otherwise it's joined onto `base`.
    fn full_url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{}", self.base, path)
        }
    }
}

/// Maps a non-2xx status to a `GraphError`, reading `Retry-After` off the
/// response before it (and its body) are dropped for the throttled/success
/// cases, and consuming the body for the catch-all `Http` case so callers
/// can see what the server said.
fn classify_status(status: u16, resp: ureq::Response) -> GraphError {
    match status {
        401 => GraphError::Unauthorized,
        404 => GraphError::NotFound,
        429 | 503 => {
            let retry_after_secs = resp
                .header("Retry-After")
                .and_then(|v| v.parse().ok())
                .unwrap_or(30);
            GraphError::Throttled { retry_after_secs }
        }
        _ => {
            let body = resp.into_string().unwrap_or_default();
            GraphError::Http { status, body }
        }
    }
}

/// The largest JSON response [`parse_body`] reads: 10 MiB, the limit ureq's
/// `into_string` kept before (#1978).
const JSON_MAX: usize = 10 * 1024 * 1024;

/// The largest response `GraphClient::get_body` reads (32 MiB, #1978): an
/// html body with `data:` pictures can be far bigger than any listing.
pub const BODY_MAX: usize = 32 * 1024 * 1024;

/// The `$select` of a body fetch ([`GraphClient::get_body_with_headers`]):
/// the body and, in the same request, the internet headers the store keeps
/// a few of (#1933).
pub const BODY_SELECT: &str = "body,internetMessageHeaders";

/// Reads a successful response's body and parses it as JSON, at most
/// [`JSON_MAX`] bytes ([`parse_body_capped`]).
fn parse_body(resp: ureq::Response) -> Result<Value, GraphError> {
    parse_body_capped(resp, JSON_MAX)
}

/// Reads a successful response's body, at most `max` bytes, and parses it
/// as JSON (bytes that are not UTF-8 replaced, as ureq's `into_string`
/// did). A response over `max`, by its `Content-Length` (refused before
/// reading) or by what it yields, is a `Parse` error (#1978): the client
/// refused it, the connection is fine, so it is not taken for a transport
/// failure (offline, back-off) and retried for ever. A read that fails
/// mid-body is still `Transport`.
fn parse_body_capped(resp: ureq::Response, max: usize) -> Result<Value, GraphError> {
    let bytes = read_response_capped(resp, max, response_too_large)?;
    json::parse(&String::from_utf8_lossy(&bytes)).map_err(|e| GraphError::Parse(e.to_string()))
}

/// Reads a response's body, at most `max` bytes: a `Content-Length` over
/// `max` is refused before reading (`too_large(max)`), and `read_capped`
/// stops a body that yields more than it declared, or declares nothing.
fn read_response_capped(
    resp: ureq::Response,
    max: usize,
    too_large: fn(usize) -> GraphError,
) -> Result<Vec<u8>, GraphError> {
    let declared = resp
        .header("Content-Length")
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|n| n > max as u64) {
        return Err(too_large(max));
    }
    read_capped(resp.into_reader(), max, too_large)
}

fn response_too_large(max: usize) -> GraphError {
    GraphError::Parse(format!("response is larger than {max} bytes"))
}

/// The largest profile photo `GraphClient::my_photo` accepts (4 MiB). Graph's
/// photos are a few kilobytes; anything near this is not a photo.
pub const PHOTO_MAX: usize = 4 * 1024 * 1024;

fn photo_too_large(max: usize) -> GraphError {
    GraphError::Parse(format!("profile photo is larger than {max} bytes"))
}

/// Reads `r` to its end, refusing (`too_large(max)`) once it yields more
/// than `max` bytes. At most `max + 1` bytes are ever read, so a chunked or
/// lying response cannot make it buffer more. A read error is `Transport`.
fn read_capped(
    r: impl std::io::Read,
    max: usize,
    too_large: fn(usize) -> GraphError,
) -> Result<Vec<u8>, GraphError> {
    use std::io::Read;
    let mut buf = Vec::new();
    r.take(max as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| GraphError::Transport(e.to_string()))?;
    if buf.len() > max {
        return Err(too_large(max));
    }
    Ok(buf)
}

/// Reads `key` off `v` as a JSON array, erroring with a `Parse` when it's
/// missing or the wrong shape.
fn value_array<'a>(v: &'a Value, key: &str) -> Result<&'a [Value], GraphError> {
    v.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| GraphError::Parse(format!("response has no '{key}' array")))
}

/// Builds the PATCH body for `set_automatic_replies`:
/// `{"automaticRepliesSetting": {...}}`. Scheduled start/end are included only
/// for `OofStatus::Scheduled`.
fn automatic_replies_body(r: &AutomaticReplies) -> String {
    let mut setting = vec![
        (
            "status".to_string(),
            Value::Str(r.status.as_wire().to_string()),
        ),
        (
            "externalAudience".to_string(),
            Value::Str(r.external_audience.as_wire().to_string()),
        ),
        (
            "internalReplyMessage".to_string(),
            Value::Str(plain_to_html(&r.internal_message)),
        ),
        (
            "externalReplyMessage".to_string(),
            Value::Str(plain_to_html(&r.external_message)),
        ),
    ];
    if r.status == OofStatus::Scheduled {
        setting.push((
            "scheduledStartDateTime".to_string(),
            utc_datetime_obj(&r.scheduled_start_utc),
        ));
        setting.push((
            "scheduledEndDateTime".to_string(),
            utc_datetime_obj(&r.scheduled_end_utc),
        ));
    }
    Value::Object(vec![(
        "automaticRepliesSetting".to_string(),
        Value::Object(setting),
    )])
    .to_string()
}

/// A Graph `dateTimeTimeZone` for a UTC instant as its wall clock: the
/// `dateTime` without the `Z`, next to `timeZone: "UTC"`, matching how
/// `calendar_view` reads times back under `Prefer: outlook.timezone="UTC"`.
/// What `get_schedule`, `respond_event`'s `proposedNewTime` and
/// `event_body_json` send.
fn graph_utc_wall_clock(utc: &str) -> Value {
    Value::Object(vec![
        (
            "dateTime".to_string(),
            Value::Str(utc.trim_end_matches('Z').to_string()),
        ),
        ("timeZone".to_string(), Value::Str("UTC".to_string())),
    ])
}

/// A Graph `{"dateTime": <utc>, "timeZone": "UTC"}` object for a canonical UTC
/// timestamp.
fn utc_datetime_obj(utc: &str) -> Value {
    Value::Object(vec![
        ("dateTime".to_string(), Value::Str(utc.to_string())),
        ("timeZone".to_string(), Value::Str("UTC".to_string())),
    ])
}

/// Builds the JSON body shared by `create_draft` and `update_draft`:
/// `{"subject":…, "body":{"contentType":"HTML","content":…},
/// "toRecipients":[…], "ccRecipients":[…], "bccRecipients":[…]}`.
fn draft_body_json(
    body_html: &str,
    subject: &str,
    to: &[Recipient],
    cc: &[Recipient],
    bcc: &[Recipient],
) -> String {
    Value::Object(vec![
        ("subject".to_string(), Value::Str(subject.to_string())),
        (
            "body".to_string(),
            Value::Object(vec![
                ("contentType".to_string(), Value::Str("HTML".to_string())),
                ("content".to_string(), Value::Str(body_html.to_string())),
            ]),
        ),
        ("toRecipients".to_string(), recipients_json(to)),
        ("ccRecipients".to_string(), recipients_json(cc)),
        ("bccRecipients".to_string(), recipients_json(bcc)),
    ])
    .to_string()
}

/// The `$select` of `contacts` and `contact` (#2063): the id, the change
/// key and every property `ContactFieldSet` names.
const CONTACT_SELECT: &str = "id,changeKey,givenName,middleName,surname,displayName,fileAs,\
emailAddresses,businessPhones,homePhones,mobilePhone,companyName,jobTitle,department,\
businessAddress,homeAddress,birthday,personalNotes,categories";

/// The JSON body of a contact create or update (#2063): the properties in
/// `set`, each with its value in `f`. A string goes as given (`""` clears
/// it), a list whole, an address as its full five-key object, the
/// `emailAddresses` as `[{name, address}]` without blank addresses, and the
/// birthday as noon UTC of its day (so no client's zone shows the day
/// before), `null` when `""`.
pub(crate) fn contact_body_json(f: &ContactFields, set: ContactFieldSet) -> String {
    let has = |g: ContactFieldSet| set.contains(g);
    let mut obj: Vec<(String, Value)> = Vec::new();
    let mut put = |k: &str, v: Value| obj.push((k.to_string(), v));
    let text = |s: &str| Value::Str(s.to_string());
    let list = |l: &[String]| Value::Array(l.iter().map(|s| Value::Str(s.clone())).collect());
    let address = |a: &PhysicalAddress| {
        Value::Object(vec![
            ("street".to_string(), text(&a.street)),
            ("city".to_string(), text(&a.city)),
            ("state".to_string(), text(&a.state)),
            ("postalCode".to_string(), text(&a.postal_code)),
            ("countryOrRegion".to_string(), text(&a.country_or_region)),
        ])
    };
    let strings = [
        (ContactFieldSet::GIVEN_NAME, "givenName", &f.given_name),
        (ContactFieldSet::MIDDLE_NAME, "middleName", &f.middle_name),
        (ContactFieldSet::SURNAME, "surname", &f.surname),
        (
            ContactFieldSet::DISPLAY_NAME,
            "displayName",
            &f.display_name,
        ),
        (ContactFieldSet::FILE_AS, "fileAs", &f.file_as),
    ];
    for (g, k, v) in strings {
        if has(g) {
            put(k, text(v));
        }
    }
    if has(ContactFieldSet::EMAIL_ADDRESSES) {
        let emails = f
            .email_addresses
            .iter()
            .filter(|e| !e.address.trim().is_empty())
            .map(|e| {
                Value::Object(vec![
                    ("name".to_string(), text(&e.name)),
                    ("address".to_string(), text(e.address.trim())),
                ])
            })
            .collect();
        put("emailAddresses", Value::Array(emails));
    }
    if has(ContactFieldSet::BUSINESS_PHONES) {
        put("businessPhones", list(&f.business_phones));
    }
    if has(ContactFieldSet::HOME_PHONES) {
        put("homePhones", list(&f.home_phones));
    }
    let more = [
        (
            ContactFieldSet::MOBILE_PHONE,
            "mobilePhone",
            &f.mobile_phone,
        ),
        (
            ContactFieldSet::COMPANY_NAME,
            "companyName",
            &f.company_name,
        ),
        (ContactFieldSet::JOB_TITLE, "jobTitle", &f.job_title),
        (ContactFieldSet::DEPARTMENT, "department", &f.department),
    ];
    for (g, k, v) in more {
        if has(g) {
            put(k, text(v));
        }
    }
    if has(ContactFieldSet::BUSINESS_ADDRESS) {
        put("businessAddress", address(&f.business_address));
    }
    if has(ContactFieldSet::HOME_ADDRESS) {
        put("homeAddress", address(&f.home_address));
    }
    if has(ContactFieldSet::BIRTHDAY) {
        let b = if f.birthday.is_empty() {
            Value::Null
        } else {
            text(&format!("{}T12:00:00Z", f.birthday))
        };
        put("birthday", b);
    }
    if has(ContactFieldSet::PERSONAL_NOTES) {
        put("personalNotes", text(&f.personal_notes));
    }
    if has(ContactFieldSet::CATEGORIES) {
        put("categories", list(&f.categories));
    }
    Value::Object(obj).to_string()
}

/// Serializes an `EventInput` to the JSON body shared by `create_event` and
/// `update_event`: only the groups in `input.fields` (#1994), and never an
/// empty enum string (`showAs`, `sensitivity`, `importance`,
/// `onlineMeetingProvider`: Graph refuses `""`). Each of `start`/`end` is
/// its wall clock in its named zone (`start_time_zone`/`start_local`, #2039)
/// when `event_zone_error` accepts it, else the UTC wall clock (the `Z`
/// suffix stripped) with `timeZone:"UTC"`, matching how `calendar_view`
/// reads events back under the `Prefer: outlook.timezone="UTC"` header.
/// When the start is sent in its zone, the recurrence (either shape, with
/// or without `TIMES` in the set) sends that zone as
/// `range.recurrenceTimeZone`, over a `series_recurrence`'s own: Graph
/// takes `startDate`/`endDate` as dates in it, and `startDate` must match
/// the start. With no zone named, the body is what it was before #2039.
pub(crate) fn event_body_json(input: &EventInput) -> String {
    let has = |g: EventFieldSet| input.fields.contains(g);
    let mut obj: Vec<(String, Value)> = Vec::new();
    let mut put = |k: &str, v: Value| obj.push((k.to_string(), v));
    let enum_str = |s: &str| (!s.is_empty()).then(|| Value::Str(s.to_string()));
    if has(EventFieldSet::SUBJECT) {
        put("subject", Value::Str(input.subject.clone()));
    }
    let start_zoned = zoned_end(&input.start_time_zone, &input.start_local, &input.start_utc);
    if has(EventFieldSet::TIMES) {
        let end_zoned = zoned_end(&input.end_time_zone, &input.end_local, &input.end_utc);
        put(
            "start",
            start_zoned
                .clone()
                .unwrap_or_else(|| graph_utc_wall_clock(&input.start_utc)),
        );
        put(
            "end",
            end_zoned.unwrap_or_else(|| graph_utc_wall_clock(&input.end_utc)),
        );
        put("isAllDay", Value::Bool(input.is_all_day));
    }
    if has(EventFieldSet::LOCATION) {
        put(
            "location",
            Value::Object(vec![(
                "displayName".to_string(),
                Value::Str(input.location.clone()),
            )]),
        );
    }
    if has(EventFieldSet::ATTENDEES) {
        let attendees = input
            .attendees
            .iter()
            .map(|a| {
                let kind = if a.r#type.is_empty() {
                    "required"
                } else {
                    &a.r#type
                };
                Value::Object(vec![
                    (
                        "emailAddress".to_string(),
                        Value::Object(vec![
                            ("address".to_string(), Value::Str(a.addr.clone())),
                            ("name".to_string(), Value::Str(a.name.clone())),
                        ]),
                    ),
                    ("type".to_string(), Value::Str(kind.to_string())),
                ])
            })
            .collect();
        put("attendees", Value::Array(attendees));
    }
    if has(EventFieldSet::BODY) {
        put(
            "body",
            Value::Object(vec![
                ("contentType".to_string(), Value::Str("HTML".to_string())),
                ("content".to_string(), Value::Str(input.body_html.clone())),
            ]),
        );
    }
    if has(EventFieldSet::RECURRENCE) {
        let zone = start_zoned
            .is_some()
            .then_some(input.start_time_zone.as_str());
        if let Some(rec) = &input.series_recurrence {
            match zone {
                Some(zone) => {
                    let mut rec = rec.clone();
                    rec.range.recurrence_time_zone = zone.to_string();
                    put("recurrence", rec.to_send_json());
                }
                None => put("recurrence", rec.to_send_json()),
            }
        } else if let Some(rec) = &input.recurrence {
            match zone {
                Some(zone) => put("recurrence", with_range_time_zone(rec.to_json(), zone)),
                None => put("recurrence", rec.to_json()),
            }
        }
    }
    if has(EventFieldSet::SHOW_AS) {
        if let Some(v) = enum_str(&input.show_as) {
            put("showAs", v);
        }
    }
    if has(EventFieldSet::SENSITIVITY) {
        if let Some(v) = enum_str(&input.sensitivity) {
            put("sensitivity", v);
        }
    }
    if has(EventFieldSet::IMPORTANCE) {
        if let Some(v) = enum_str(&input.importance) {
            put("importance", v);
        }
    }
    if has(EventFieldSet::CATEGORIES) {
        put(
            "categories",
            Value::Array(
                input
                    .categories
                    .iter()
                    .map(|c| Value::Str(c.clone()))
                    .collect(),
            ),
        );
    }
    if has(EventFieldSet::REMINDER) {
        put("isReminderOn", Value::Bool(input.is_reminder_on));
        put(
            "reminderMinutesBeforeStart",
            Value::Num(input.reminder_minutes as f64),
        );
    }
    if has(EventFieldSet::ONLINE_MEETING) {
        put("isOnlineMeeting", Value::Bool(input.is_online_meeting));
        if let Some(v) = enum_str(&input.online_meeting_provider) {
            put("onlineMeetingProvider", v);
        }
    }
    if has(EventFieldSet::RESPONSE_REQUESTED) {
        put("responseRequested", Value::Bool(input.response_requested));
    }
    if has(EventFieldSet::ALLOW_NEW_TIME_PROPOSALS) {
        put(
            "allowNewTimeProposals",
            Value::Bool(input.allow_new_time_proposals),
        );
    }
    Value::Object(obj).to_string()
}

/// Serializes recipients as Graph's `emailAddress` array shape:
/// `[{"emailAddress":{"address":…,"name":…}}, …]`.
fn recipients_json(recipients: &[Recipient]) -> Value {
    Value::Array(
        recipients
            .iter()
            .map(|r| {
                Value::Object(vec![(
                    "emailAddress".to_string(),
                    Value::Object(vec![
                        ("address".to_string(), Value::Str(r.address.clone())),
                        ("name".to_string(), Value::Str(r.name.clone())),
                    ]),
                )])
            })
            .collect(),
    )
}

/// Splits the bytes `start..total` into consecutive `(start, end)` half-open
/// ranges of at most `chunk` bytes each — `end` exclusive, so
/// `bytes[start..end]` is the slice to send and the last range's `end` always
/// equals `total`. `start` is 0 for a new upload session, or where a resumed
/// one goes on from (#1941). Empty (`start >= total`) yields no ranges.
fn chunk_ranges(start: usize, total: usize, chunk: usize) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut start = start;
    while start < total {
        let end = (start + chunk).min(total);
        v.push((start, end));
        start = end;
    }
    v
}

/// The most hits [`GraphClient::search_messages`] returns: Graph's own cap
/// on a message `$search` (#2006).
pub const SEARCH_TOP_MAX: usize = 1000;

/// A message `$search` value (#2006): `query` in double quotes, its `\`
/// and `"` escaped with a backslash so it cannot end the quoted clause, the
/// whole value percent-encoded ([`encode_path_segment`]).
fn search_value(query: &str) -> String {
    let mut quoted = String::with_capacity(query.len() + 2);
    quoted.push('"');
    for c in query.chars() {
        if c == '\\' || c == '"' {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    encode_path_segment(&quoted)
}

/// Percent-encodes every byte except RFC 3986 unreserved characters
/// (`A-Za-z0-9-._~`). Graph's REST-format message/folder/attachment ids
/// commonly contain `/`, `+`, and trailing `=` (they're base64-ish), which
/// would otherwise be misread as path separators or otherwise corrupt the
/// URL when interpolated straight into a path segment — Microsoft's Graph
/// docs call out URL-encoding ids used in paths for exactly this reason.
fn encode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Decodes standard base64 (RFC 4648 section 4: `+`/`/` alphabet, `=`
/// padding) — the encoding Graph uses for `fileAttachment.contentBytes`.
/// This is deliberately a separate small decoder from
/// `crate::pkce::base64url_decode`: that one speaks base64*url*
/// (`-`/`_`, no padding), a different alphabet, so it can't be reused
/// as-is here. Structured the same way (chunks of 4 input chars -> up to 3
/// output bytes) so the two are easy to compare.
pub(crate) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    fn digit(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }

    let s = s.trim_end_matches('=');
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    if bytes.len() % 4 == 1 {
        return None; // one leftover char can't hold a full byte
    }
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    for chunk in bytes.chunks(4) {
        let mut n = [0u32; 4];
        for (i, &c) in chunk.iter().enumerate() {
            n[i] = digit(c)?;
        }
        let combined = (n[0] << 18) | (n[1] << 12) | (n[2] << 6) | n[3];
        out.push((combined >> 16) as u8);
        if chunk.len() > 2 {
            out.push((combined >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(combined as u8);
        }
    }
    Some(out)
}

/// Appends `isInline: true` and `contentId` to an attachment's JSON fields
/// when `content_id` is `Some` (#1949); with `None` adds nothing, so a plain
/// attachment's body is unchanged.
fn push_inline_fields(fields: &mut Vec<(String, Value)>, content_id: Option<&str>) {
    if let Some(cid) = content_id {
        fields.push(("isInline".to_string(), Value::Bool(true)));
        fields.push(("contentId".to_string(), Value::Str(cid.to_string())));
    }
}

/// Standard base64 (RFC 4648 §4: `+`/`/` alphabet, `=` padding) — the encoding
/// Graph expects for `fileAttachment.contentBytes`. The `pkce` module's
/// base64*url* encoder can't be reused (different alphabet, no padding).
/// Used by `add_file_attachment` to encode inline attachment bytes.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::model::MeetingMessageType;
    use crate::testserver::{FakeServer, Route};

    #[test]
    fn list_folders_parses() {
        let srv = FakeServer::start(vec![Route{
            method:"GET".into(), path_prefix:"/me/mailFolders".into(), status:200, headers:vec![],
            body: r#"{"value":[{"id":"F1","displayName":"Inbox","parentFolderId":null,"totalItemCount":2,"unreadItemCount":1,"wellKnownName":"inbox"}]}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let listing = c.list_folders().unwrap();
        assert_eq!(listing.folders[0].display_name, "Inbox");
        assert!(listing.complete);
    }

    #[test]
    fn throttle_maps_to_retry_after() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/mailFolders".into(),
            status: 429,
            headers: vec![("Retry-After".into(), "7".into())],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        match c.list_folders() {
            Err(GraphError::Throttled { retry_after_secs }) => assert_eq!(retry_after_secs, 7),
            other => panic!("expected throttled, got {other:?}"),
        }
    }

    #[test]
    fn unauthorized_maps() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/mailFolders".into(),
            status: 401,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(c.list_folders(), Err(GraphError::Unauthorized)));
    }

    #[test]
    fn sends_bearer_header() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/mailFolders".into(),
            status: 200,
            headers: vec![],
            body: r#"{"value":[]}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "SECRET");
        c.list_folders().unwrap();
        let reqs = srv.requests();
        assert!(
            reqs[0]
                .headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("authorization") && v == "Bearer SECRET")
        );
    }

    #[test]
    fn get_message_selects_header_fields() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"M1","subject":"Hi","from":{"emailAddress":{"name":"A","address":"a@x"}},"receivedDateTime":"2026-01-01T00:00:00Z","isRead":true}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let (m, f) = c.get_message("M1").unwrap();
        assert_eq!(m.subject, "Hi");
        assert_eq!(m.from.address, "a@x");
        assert_eq!(m.received, "2026-01-01T00:00:00Z");
        assert!(!f.is_partial());
        assert!(!f.preview, "bodyPreview was not in the response");
        let path = &srv.requests()[0].path;
        assert!(path.starts_with("/me/messages/M1?$select="), "{path}");
        for field in [
            "receivedDateTime",
            "subject",
            "from",
            "isRead",
            "bodyPreview",
        ] {
            assert!(path.contains(field), "{field} selected: {path}");
        }
    }

    /// A `$search` page route: `prefix`, with these items and maybe a
    /// `nextLink`.
    fn search_page(prefix: &str, items: &str, next: Option<&str>) -> Route {
        let next = next
            .map(|n| format!(r#","@odata.nextLink":"{n}""#))
            .unwrap_or_default();
        Route {
            method: "GET".into(),
            path_prefix: prefix.into(),
            status: 200,
            headers: vec![],
            body: format!(r#"{{"value":[{items}]{next}}}"#),
        }
    }

    #[test]
    fn search_value_quotes_escapes_and_encodes() {
        assert_eq!(search_value("budget"), "%22budget%22");
        assert_eq!(
            search_value(r#"budget "q3" a\b"#),
            "%22budget%20%5C%22q3%5C%22%20a%5C%5Cb%22"
        );
        // A query that tries to close the clause stays inside it.
        assert_eq!(search_value(r#"x" OR "y"#), "%22x%5C%22%20OR%20%5C%22y%22");
        assert_eq!(search_value(""), "%22%22");
    }

    #[test]
    fn search_messages_mailbox_path_selects_parent_folder_and_top() {
        let srv = FakeServer::start(vec![search_page("/me/messages?", "", None)]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert_eq!(c.search_messages(None, "budget", 25).unwrap(), vec![]);
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "GET");
        assert_eq!(
            reqs[0].path,
            format!(
                "/me/messages?$search=%22budget%22&$select={MESSAGE_SELECT},parentFolderId&$top=25\
                 &$expand={MESSAGE_EXPAND}"
            )
        );
    }

    #[test]
    fn search_messages_folder_path_is_percent_encoded() {
        let srv = FakeServer::start(vec![search_page("/me/mailFolders/", "", None)]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.search_messages(Some("A/B+C="), "budget", 10).unwrap();
        let path = &srv.requests()[0].path;
        assert_eq!(
            path,
            &format!(
                "/me/mailFolders/A%2FB%2BC%3D/messages?$search=%22budget%22\
                 &$select={MESSAGE_SELECT},parentFolderId&$top=10&$expand={MESSAGE_EXPAND}"
            )
        );
    }

    #[test]
    fn search_messages_escapes_quotes_and_backslashes() {
        let srv = FakeServer::start(vec![search_page("/me/messages?", "", None)]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.search_messages(None, r#"budget "q3" a\b"#, 5).unwrap();
        let path = &srv.requests()[0].path;
        assert!(
            path.starts_with(
                "/me/messages?$search=%22budget%20%5C%22q3%5C%22%20a%5C%5Cb%22&$select="
            ),
            "{path}"
        );
        // Graph allows neither with `$search` on messages (as query
        // options: the expand's own `$filter` is inside its parentheses).
        assert!(!path.contains("$orderby"), "{path}");
        assert!(!path.contains("&$filter"), "{path}");
        assert!(!path.contains("?$filter"), "{path}");
    }

    #[test]
    fn search_messages_clamps_top() {
        let srv = FakeServer::start(vec![search_page("/me/messages?", "", None)]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.search_messages(None, "a", 0).unwrap();
        c.search_messages(None, "a", 5000).unwrap();
        c.search_messages(None, "a", SEARCH_TOP_MAX).unwrap();
        let tops: Vec<String> = srv
            .requests()
            .iter()
            .map(|r| {
                let top = r.path.rsplit("$top=").next().unwrap();
                top.split('&').next().unwrap().to_string()
            })
            .collect();
        assert_eq!(tops, ["1", "1000", "1000"]);
    }

    #[test]
    fn search_messages_follows_next_link_until_top() {
        let srv = FakeServer::start(vec![
            search_page(
                "/me/messages?$skiptoken=2",
                r#"{"id":"M3","parentFolderId":"F1"},{"id":"M4","parentFolderId":"F1"}"#,
                Some("/me/messages?$skiptoken=3"),
            ),
            search_page(
                "/me/messages?$skiptoken=3",
                r#"{"id":"M5","parentFolderId":"F1"}"#,
                None,
            ),
            search_page(
                "/me/messages?$search",
                r#"{"id":"M1","parentFolderId":"F1"},{"id":"M2","parentFolderId":"F2"}"#,
                Some("/me/messages?$skiptoken=2"),
            ),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let hits = c.search_messages(None, "budget", 3).unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.message.id.as_str()).collect();
        assert_eq!(ids, ["M1", "M2", "M3"], "truncated to top");
        let paths: Vec<String> = srv.requests().into_iter().map(|r| r.path).collect();
        assert_eq!(paths.len(), 2, "top reached on page 2: {paths:?}");
        assert_eq!(paths[1], "/me/messages?$skiptoken=2", "the link as is");
    }

    #[test]
    fn search_messages_follows_next_link_to_the_end() {
        let srv = FakeServer::start(vec![
            search_page(
                "/me/messages?$skiptoken=2",
                r#"{"id":"M2"},{"subject":"no id"}"#,
                Some("/me/messages?$skiptoken=3"),
            ),
            search_page("/me/messages?$skiptoken=3", r#"{"id":"M3"}"#, None),
            search_page(
                "/me/messages?$search",
                r#"{"id":"M1"}"#,
                Some("/me/messages?$skiptoken=2"),
            ),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let hits = c.search_messages(None, "budget", 100).unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.message.id.as_str()).collect();
        assert_eq!(ids, ["M1", "M2", "M3"], "the item without an id dropped");
        assert_eq!(srv.requests().len(), 3);
    }

    #[test]
    fn search_messages_stops_at_a_page_without_items() {
        // A link that loops (or a server that keeps sending one) ends at
        // the first page that brings nothing.
        let srv = FakeServer::start(vec![
            search_page(
                "/me/messages?$skiptoken=",
                "",
                Some("/me/messages?$skiptoken=1"),
            ),
            search_page(
                "/me/messages?$search",
                r#"{"id":"M1"}"#,
                Some("/me/messages?$skiptoken=1"),
            ),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert_eq!(c.search_messages(None, "budget", 50).unwrap().len(), 1);
        assert_eq!(srv.requests().len(), 2);
    }

    #[test]
    fn search_messages_stops_at_a_page_without_hits() {
        // Items without an id are no hits: a looping link over such a
        // page ends there too.
        let srv = FakeServer::start(vec![search_page(
            "/me/messages?",
            r#"{"subject":"no id"}"#,
            Some("/me/messages?$skiptoken=1"),
        )]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert_eq!(c.search_messages(None, "budget", 50).unwrap(), vec![]);
        assert_eq!(srv.requests().len(), 1);
    }

    #[test]
    fn search_messages_maps_errors() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages?".into(),
            status: 403,
            headers: vec![],
            body: r#"{"error":{"code":"ErrorAccessDenied"}}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        match c.search_messages(None, "budget", 5) {
            Err(GraphError::Http { status: 403, .. }) => {}
            other => panic!("expected a 403, got {other:?}"),
        }
    }

    #[test]
    fn search_hit_carries_folder_and_delta_fields() {
        let srv = FakeServer::start(vec![search_page(
            "/me/messages?",
            r#"{"id":"M1","parentFolderId":"F2","subject":"Budget","from":{"emailAddress":{"name":"A","address":"a@x"}},"receivedDateTime":"2026-01-01T00:00:00Z","isRead":true}"#,
            None,
        )]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let hits = c.search_messages(None, "budget", 5).unwrap();
        assert_eq!(hits.len(), 1);
        let h = &hits[0];
        assert_eq!(h.folder_id, "F2");
        assert_eq!(h.message.subject, "Budget");
        assert_eq!(h.message.from.address, "a@x");
        assert!(h.message.is_read);
        assert!(h.fields.subject && h.fields.from && h.fields.is_read);
        assert!(!h.fields.preview, "bodyPreview was not in the response");
    }

    #[test]
    fn get_message_hit_selects_parent_folder() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"A/B+C=","parentFolderId":"F2","subject":"Hi"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let hit = c.get_message_hit("A/B+C=").unwrap();
        assert_eq!(hit.folder_id, "F2");
        assert_eq!(hit.message.subject, "Hi");
        assert_eq!(
            srv.requests()[0].path,
            format!(
                "/me/messages/A%2FB%2BC%3D?$select={MESSAGE_SELECT},parentFolderId\
                 &$expand={MESSAGE_EXPAND}"
            )
        );
    }

    #[test]
    fn get_message_hit_without_an_id_is_a_parse_error() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/".into(),
            status: 200,
            headers: vec![],
            body: r#"{"subject":"Hi"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(c.get_message_hit("M1"), Err(GraphError::Parse(_))));
    }

    #[test]
    fn get_folder_counts_reads_totals() {
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/A%2FB%3D?".into(),
                status: 200,
                headers: vec![],
                body: r#"{"id":"A/B=","totalItemCount":12,"unreadItemCount":3}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F2".into(),
                status: 200,
                headers: vec![],
                body: r#"{"id":"F2","totalItemCount":4}"#.into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert_eq!(c.get_folder_counts("A/B=").unwrap(), (12, 3));
        // A missing count is 0, as in `MailFolder::from_json`.
        assert_eq!(c.get_folder_counts("F2").unwrap(), (4, 0));
        let path = &srv.requests()[0].path;
        assert_eq!(
            path,
            "/me/mailFolders/A%2FB%3D?$select=totalItemCount,unreadItemCount"
        );
    }

    #[test]
    fn get_message_escapes_id() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"A/B+C="}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.get_message("A/B+C=").unwrap();
        let path = &srv.requests()[0].path;
        assert!(path.starts_with("/me/messages/A%2FB%2BC%3D?"), "{path}");
    }

    #[test]
    fn not_found_maps() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/".into(),
            status: 404,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(c.get_body("X", false), Err(GraphError::NotFound)));
    }

    #[test]
    fn other_status_maps_to_http_with_body() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/mailFolders".into(),
            status: 500,
            headers: vec![],
            body: "boom".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        match c.list_folders() {
            Err(GraphError::Http { status, body }) => {
                assert_eq!(status, 500);
                assert_eq!(body, "boom");
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[test]
    fn get_body_sends_prefer_text_header_and_parses() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: r#"{"body":{"contentType":"text","content":"hi"}}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let body = c.get_body("M1", true).unwrap();
        assert_eq!(body.content, "hi");
        let reqs = srv.requests();
        assert!(
            reqs[0]
                .headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("prefer")
                    && v == "outlook.body-content-type=\"text\"")
        );
    }

    #[test]
    fn get_body_selects_internet_message_headers() {
        // #1933: the body GET asks for the headers in the same request and
        // keeps only `STORED_HEADERS`, the first of a name (ignoring case),
        // values trimmed (an empty one stays, as Graph sent it).
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: r#"{"body":{"contentType":"text","content":"hi"},"internetMessageHeaders":[
                {"name":"Received","value":"from x"},
                {"name":"X-MS-Exchange-Organization-AuthAs","value":"  Internal\r\n"},
                {"name":"x-ms-exchange-organization-authas","value":"Anonymous"},
                {"name":"X-MS-Exchange-Organization-MessageDirectionality","value":" "}
            ]}"#
            .into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let (body, headers) = c.get_body_with_headers("M1", false).unwrap();
        assert_eq!(body.content, "hi");
        assert_eq!(
            headers,
            [
                MessageHeader {
                    name: "X-MS-Exchange-Organization-AuthAs".into(),
                    value: "Internal".into(),
                },
                MessageHeader {
                    name: "X-MS-Exchange-Organization-MessageDirectionality".into(),
                    value: String::new(),
                },
            ]
        );
        assert_eq!(c.get_body("M1", false).unwrap().content, "hi");
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 2);
        for r in &reqs {
            assert_eq!(
                r.path,
                "/me/messages/M1?$select=body,internetMessageHeaders"
            );
        }
    }

    #[test]
    fn get_body_keeps_the_msip_labels_header() {
        // #2015: the sensitivity label comes with the body as Exchange names
        // it (lowercase), trimmed and otherwise byte for byte as sent.
        let label = "MSIP_Label_6f0b3a2e-1c4d-4e5f-8a9b-0c1d2e3f4a5b_Enabled=true; \
            MSIP_Label_6f0b3a2e-1c4d-4e5f-8a9b-0c1d2e3f4a5b_SetDate=2026-11-02T09:15:00Z; \
            MSIP_Label_6f0b3a2e-1c4d-4e5f-8a9b-0c1d2e3f4a5b_Method=Standard; \
            MSIP_Label_6f0b3a2e-1c4d-4e5f-8a9b-0c1d2e3f4a5b_Name=Confidential; \
            MSIP_Label_6f0b3a2e-1c4d-4e5f-8a9b-0c1d2e3f4a5b_SiteId=72f988bf-86f1-41af-91ab-2d7cd011db47; \
            MSIP_Label_6f0b3a2e-1c4d-4e5f-8a9b-0c1d2e3f4a5b_ActionId=0a1b2c3d-4e5f-6071-8293-a4b5c6d7e8f9; \
            MSIP_Label_6f0b3a2e-1c4d-4e5f-8a9b-0c1d2e3f4a5b_ContentBits=0";
        let body = format!(
            r#"{{"body":{{"contentType":"text","content":"hi"}},"internetMessageHeaders":[
                {{"name":"Received","value":"from x"}},
                {{"name":"msip_labels","value":"  {label}\r\n"}},
                {{"name":"X-MS-Exchange-Organization-AuthAs","value":"Internal"}}
            ]}}"#
        );
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body,
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let (_, headers) = c.get_body_with_headers("M1", false).unwrap();
        assert_eq!(
            headers,
            [
                MessageHeader {
                    name: "msip_labels".into(),
                    value: label.into(),
                },
                MessageHeader {
                    name: "X-MS-Exchange-Organization-AuthAs".into(),
                    value: "Internal".into(),
                },
            ]
        );
    }

    #[test]
    fn get_body_without_usable_headers_yields_none() {
        // No field, a null one, or malformed entries: the body still parses
        // and no header comes back (#1933).
        let cases = [
            r#"{"body":{"contentType":"text","content":"hi"}}"#,
            r#"{"body":{"contentType":"text","content":"hi"},"internetMessageHeaders":null}"#,
            r#"{"body":{"contentType":"text","content":"hi"},"internetMessageHeaders":{"name":"X-MS-Exchange-Organization-AuthAs","value":"Internal"}}"#,
            r#"{"body":{"contentType":"text","content":"hi"},"internetMessageHeaders":[
                {"name":"X-MS-Exchange-Organization-AuthAs"},
                {"name":"X-MS-Exchange-Organization-AuthAs","value":7},
                {"value":"Internal"},
                "X-MS-Exchange-Organization-AuthAs: Internal",
                null
            ]}"#,
        ];
        for json in cases {
            let srv = FakeServer::start(vec![Route {
                method: "GET".into(),
                path_prefix: "/me/messages/M1".into(),
                status: 200,
                headers: vec![],
                body: json.into(),
            }]);
            let c = GraphClient::new(&srv.base_url, "AT");
            let (body, headers) = c.get_body_with_headers("M1", true).unwrap();
            assert_eq!(body.content, "hi", "{json}");
            assert!(headers.is_empty(), "{json}: {headers:?}");
        }
    }

    #[test]
    fn list_attachments_parses() {
        let srv = FakeServer::start(vec![Route{
            method:"GET".into(), path_prefix:"/me/messages/M1/attachments".into(), status:200, headers:vec![],
            body: r#"{"value":[{"id":"A1","name":"f.txt","contentType":"text/plain","size":3,"isInline":false}]}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let atts = c.list_attachments("M1").unwrap();
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].name, "f.txt");
    }

    #[test]
    fn get_attachment_bytes_decodes_base64() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M1/attachments/A1".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"A1","contentBytes":"aGVsbG8="}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let bytes = c.get_attachment_bytes("M1", "A1").unwrap();
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn get_attachment_raw_value_returns_body_bytes() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M1/attachments/A1/$value".into(),
            status: 200,
            headers: vec![],
            body: "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let bytes = c.get_attachment_raw_value("M1", "A1").unwrap();
        assert_eq!(bytes, b"BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n");
    }

    #[test]
    fn mark_read_sends_patch_body() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.mark_read("M1", true).unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].body, r#"{"isRead":true}"#);
    }

    #[test]
    fn set_flag_sends_patch_body() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.set_flag("M1", true).unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].body, r#"{"flag":{"flagStatus":"flagged"}}"#);
    }

    #[test]
    fn move_message_posts_and_returns_new_id() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/move".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"M1-NEW"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let new_id = c.move_message("M1", "DEST").unwrap();
        assert_eq!(new_id, "M1-NEW");
        let reqs = srv.requests();
        assert_eq!(reqs[0].body, r#"{"destinationId":"DEST"}"#);
    }

    #[test]
    fn delete_message_sends_delete() {
        let srv = FakeServer::start(vec![Route {
            method: "DELETE".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 204,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.delete_message("M1").unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "DELETE");
    }

    fn delta_route() -> Route {
        Route {
            method: "GET".into(),
            path_prefix: "/me/mailFolders/F1/messages/delta".into(),
            status: 200,
            headers: vec![],
            body: r#"{"value":[],"@odata.deltaLink":"http://x/delta?token=abc"}"#.into(),
        }
    }

    fn sends_maxpagesize(r: &crate::testserver::RecordedRequest) -> bool {
        r.headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("prefer") && v == "odata.maxpagesize=50")
    }

    #[test]
    fn delta_head_first_call_filters_and_orders_newest_first() {
        let srv = FakeServer::start(vec![delta_route()]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let page = c
            .delta(DeltaCursor::Head {
                folder_id: "F1".to_string(),
                since: "2026-10-23T00:00:00Z".to_string(),
            })
            .unwrap();
        assert_eq!(page.delta_link.as_deref(), Some("http://x/delta?token=abc"));
        let reqs = srv.requests();
        assert_eq!(
            reqs[0].path,
            format!(
                "/me/mailFolders/F1/messages/delta\
                 ?$filter=receivedDateTime%20ge%202026-10-23T00:00:00Z\
                 &$orderby=receivedDateTime%20desc&$select={MESSAGE_SELECT}\
                 &$expand={MESSAGE_EXPAND}"
            )
        );
        assert!(sends_maxpagesize(&reqs[0]));
    }

    #[test]
    fn delta_full_first_call_orders_newest_first_without_filter() {
        let srv = FakeServer::start(vec![delta_route()]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let page = c.delta(DeltaCursor::Full("F1".to_string())).unwrap();
        assert_eq!(page.delta_link.as_deref(), Some("http://x/delta?token=abc"));
        let reqs = srv.requests();
        assert_eq!(
            reqs[0].path,
            format!(
                "/me/mailFolders/F1/messages/delta\
                 ?$orderby=receivedDateTime%20desc&$select={MESSAGE_SELECT}\
                 &$expand={MESSAGE_EXPAND}"
            )
        );
        // No `$filter` of its own: the only one is the expand's.
        assert!(!reqs[0].path.contains("&$filter") && !reqs[0].path.contains("?$filter"));
        assert!(sends_maxpagesize(&reqs[0]));
    }

    #[test]
    fn is_expired_sync_state_classifies_410_and_400_codes() {
        let http = |status: u16, body: &str| GraphError::Http {
            status,
            body: body.to_string(),
        };
        assert!(is_expired_sync_state(&http(410, "")));
        assert!(is_expired_sync_state(&http(410, "<html>gone</html>")));
        for code in ["SyncStateNotFound", "syncStateInvalid", "resyncRequired"] {
            let body = format!(r#"{{"error":{{"code":"{code}","message":"m"}}}}"#);
            assert!(is_expired_sync_state(&http(400, &body)), "{code}");
        }
        assert!(!is_expired_sync_state(&http(
            400,
            r#"{"error":{"code":"ErrorInvalidRequest","message":"m"}}"#
        )));
        assert!(!is_expired_sync_state(&http(400, "not json")));
        // Only a 400 carries the codes: a 403 with one is not an expiry.
        assert!(!is_expired_sync_state(&http(
            403,
            r#"{"error":{"code":"SyncStateNotFound"}}"#
        )));
        assert!(!is_expired_sync_state(&GraphError::NotFound));
        assert!(!is_expired_sync_state(&GraphError::Throttled {
            retry_after_secs: 1
        }));
    }

    #[test]
    fn delta_link_cursor_uses_url_as_is() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/mailFolders/F1/messages/delta".into(),
            status: 200,
            headers: vec![],
            body: r#"{"value":[],"@odata.nextLink":"IGNORED"}"#.into(),
        }]);
        let link = format!(
            "{}/me/mailFolders/F1/messages/delta?$skiptoken=xyz",
            srv.base_url
        );
        let c = GraphClient::new(&srv.base_url, "AT");
        let page = c.delta(DeltaCursor::Link(link.clone())).unwrap();
        assert!(page.delta_link.is_none());
        let reqs = srv.requests();
        assert!(reqs[0].path.ends_with("$skiptoken=xyz"));
    }

    #[test]
    fn base64_decode_matches_known_vector() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8h").unwrap(), b"hello!");
        assert_eq!(base64_decode("").unwrap(), Vec::<u8>::new());
        assert!(base64_decode("not*base64!").is_none());
    }

    #[test]
    fn base64_decode_handles_two_char_final_group() {
        // "YQ==" is a 2-char final group (1 padding-trimmed data char pair),
        // the mod-4-remainder-2 case: 1 output byte, distinct from
        // `base64_decode_matches_known_vector`'s remainder-3 case ("aGk=").
        assert_eq!(base64_decode("YQ==").unwrap(), b"a");
    }

    #[test]
    fn base64_encode_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
        // round-trips with the existing decoder
        let raw: &[u8] = &[0, 1, 2, 250, 251, 252, 253, 255];
        assert_eq!(base64_decode(&base64_encode(raw)).unwrap(), raw);
    }

    #[test]
    fn ids_are_percent_encoded_in_path_segments() {
        // Real Graph REST-format ids commonly contain '/', '+', and a
        // trailing '=' — all of which must be percent-encoded when placed
        // in a URL path, or an id containing '/' would silently change the
        // path structure.
        let raw_id = "AAMk/id+with=chars";
        let encoded_id = "AAMk%2Fid%2Bwith%3Dchars";
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: format!("/me/messages/{encoded_id}"),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.mark_read(raw_id, true).unwrap();
        let reqs = srv.requests();
        assert!(reqs[0].path.contains(encoded_id));
        assert!(!reqs[0].path.contains(raw_id));
        assert!(!reqs[0].path.contains("AAMk/id"));
    }

    #[test]
    fn list_folders_recurses_into_child_folders() {
        // Order matters: the fake server matches the *first* route whose
        // path_prefix is a prefix of the request path, so the more specific
        // child-folder routes must come before the generic top-level route
        // (which would otherwise also match "/me/mailFolders/F1/..." paths).
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F1/childFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"F2","displayName":"Sub","parentFolderId":"F1","totalItemCount":0,"unreadItemCount":0,"wellKnownName":null}]}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F2/childFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[]}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"F1","displayName":"Inbox","parentFolderId":null,"totalItemCount":1,"unreadItemCount":0,"wellKnownName":"inbox"}]}"#.into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let listing = c.list_folders().unwrap();
        assert!(listing.complete);
        let folders = listing.folders;
        assert_eq!(folders.len(), 2);
        assert!(
            folders
                .iter()
                .any(|f| f.id == "F1" && f.display_name == "Inbox")
        );
        assert!(
            folders
                .iter()
                .any(|f| f.id == "F2" && f.display_name == "Sub")
        );
    }

    #[test]
    fn child_folders_404_is_tolerated_top_level_folders_still_returned() {
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F1/childFolders".into(),
                status: 404,
                headers: vec![],
                body: "{}".into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"F1","displayName":"Inbox","parentFolderId":null,"totalItemCount":1,"unreadItemCount":0,"wellKnownName":"inbox"}]}"#.into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let listing = c.list_folders().unwrap();
        assert_eq!(listing.folders.len(), 1);
        assert_eq!(listing.folders[0].id, "F1");
        // F1's children are unknown, so the listing is incomplete (#1886).
        assert!(!listing.complete);
    }

    /// A folder JSON object with a `childFolderCount` (`None`: absent).
    fn folder_json(id: &str, parent: Option<&str>, children: Option<i64>) -> String {
        let parent = parent.map_or("null".to_string(), |p| format!("\"{p}\""));
        let children = children.map_or(String::new(), |n| format!(r#","childFolderCount":{n}"#));
        format!(
            r#"{{"id":"{id}","displayName":"{id}","parentFolderId":{parent},"totalItemCount":0,"unreadItemCount":0{children}}}"#
        )
    }

    fn get(path_prefix: &str, status: u16, body: String) -> Route {
        Route {
            method: "GET".into(),
            path_prefix: path_prefix.into(),
            status,
            headers: vec![],
            body,
        }
    }

    fn ids(listing: &FolderListing) -> Vec<&str> {
        listing.folders.iter().map(|f| f.id.as_str()).collect()
    }

    #[test]
    fn list_folders_follows_next_links_at_top_and_in_children() {
        // Second pages first: the fake server matches the first route whose
        // prefix fits, and `/me/mailFolders?` / `/me/mailFolders/F1/...`
        // would also match them.
        let srv = FakeServer::start(vec![
            get(
                "/me/mailFolders/F1/childFolders?$skiptoken=C2",
                200,
                format!(
                    r#"{{"value":[{}]}}"#,
                    folder_json("C2", Some("F1"), Some(0))
                ),
            ),
            get(
                "/me/mailFolders/F1/childFolders",
                200,
                format!(
                    r#"{{"value":[{}],"@odata.nextLink":"/me/mailFolders/F1/childFolders?$skiptoken=C2"}}"#,
                    folder_json("C1", Some("F1"), Some(0))
                ),
            ),
            get(
                "/me/mailFolders?$top=100&$skiptoken=P2",
                200,
                format!(r#"{{"value":[{}]}}"#, folder_json("F2", None, Some(0))),
            ),
            get(
                "/me/mailFolders?",
                200,
                format!(
                    r#"{{"value":[{}],"@odata.nextLink":"/me/mailFolders?$top=100&$skiptoken=P2"}}"#,
                    folder_json("F1", None, Some(2))
                ),
            ),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let listing = c.list_folders().unwrap();
        assert!(listing.complete);
        assert_eq!(ids(&listing), vec!["F1", "C1", "C2", "F2"]);
    }

    #[test]
    fn list_folders_skips_children_of_a_leaf() {
        // F1 says it has no children: no `childFolders` call. F2 does not
        // say: asked, as before.
        let srv = FakeServer::start(vec![
            get(
                "/me/mailFolders/F2/childFolders",
                200,
                r#"{"value":[]}"#.into(),
            ),
            get(
                "/me/mailFolders?",
                200,
                format!(
                    r#"{{"value":[{},{}]}}"#,
                    folder_json("F1", None, Some(0)),
                    folder_json("F2", None, None)
                ),
            ),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let listing = c.list_folders().unwrap();
        assert!(listing.complete);
        assert_eq!(ids(&listing), vec!["F1", "F2"]);
        let paths: Vec<String> = srv.requests().into_iter().map(|r| r.path).collect();
        assert!(
            !paths.iter().any(|p| p.contains("/F1/childFolders")),
            "{paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.contains("/F2/childFolders")),
            "{paths:?}"
        );
    }

    #[test]
    fn list_folders_is_incomplete_after_a_child_error() {
        // F1's children answer 500: F1, F2 and nothing under F1 are listed,
        // and the listing says it is incomplete.
        let srv = FakeServer::start(vec![
            get("/me/mailFolders/F1/childFolders", 500, "boom".into()),
            get(
                "/me/mailFolders?",
                200,
                format!(
                    r#"{{"value":[{},{}]}}"#,
                    folder_json("F1", None, Some(1)),
                    folder_json("F2", None, Some(0))
                ),
            ),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let listing = c.list_folders().unwrap();
        assert_eq!(ids(&listing), vec!["F1", "F2"]);
        assert!(!listing.complete);
    }

    #[test]
    fn list_folders_is_incomplete_after_a_later_child_page_error() {
        // The first page of F1's children came; the second answers 500. C1
        // is kept, and the listing is incomplete.
        let srv = FakeServer::start(vec![
            get(
                "/me/mailFolders/F1/childFolders?$skiptoken=C2",
                500,
                "boom".into(),
            ),
            get(
                "/me/mailFolders/F1/childFolders",
                200,
                format!(
                    r#"{{"value":[{}],"@odata.nextLink":"/me/mailFolders/F1/childFolders?$skiptoken=C2"}}"#,
                    folder_json("C1", Some("F1"), Some(0))
                ),
            ),
            get(
                "/me/mailFolders?",
                200,
                format!(r#"{{"value":[{}]}}"#, folder_json("F1", None, Some(2))),
            ),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let listing = c.list_folders().unwrap();
        assert_eq!(ids(&listing), vec!["F1", "C1"]);
        assert!(!listing.complete);
    }

    #[test]
    fn list_folders_top_level_later_page_error_propagates() {
        let srv = FakeServer::start(vec![
            get("/me/mailFolders?$top=100&$skiptoken=P2", 500, "boom".into()),
            get(
                "/me/mailFolders?",
                200,
                format!(
                    r#"{{"value":[{}],"@odata.nextLink":"/me/mailFolders?$top=100&$skiptoken=P2"}}"#,
                    folder_json("F1", None, Some(0))
                ),
            ),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(
            c.list_folders(),
            Err(GraphError::Http { status: 500, .. })
        ));
    }

    #[test]
    fn is_draft_reads_is_draft_and_a_404_is_no_draft() {
        let srv = FakeServer::start(vec![
            get(
                "/me/messages/D?",
                200,
                r#"{"id":"D","isDraft":true}"#.into(),
            ),
            get(
                "/me/messages/S?",
                200,
                r#"{"id":"S","isDraft":false}"#.into(),
            ),
            get("/me/messages/GONE?", 404, "{}".into()),
            get("/me/messages/ODD?", 200, r#"{"id":"ODD"}"#.into()),
            get("/me/messages/BAD?", 500, "boom".into()),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert_eq!(c.is_draft("D"), Ok(true));
        assert_eq!(c.is_draft("S"), Ok(false));
        assert_eq!(c.is_draft("GONE"), Ok(false));
        assert!(matches!(c.is_draft("ODD"), Err(GraphError::Parse(_))));
        assert!(matches!(
            c.is_draft("BAD"),
            Err(GraphError::Http { status: 500, .. })
        ));
        assert!(
            srv.requests()
                .iter()
                .all(|r| r.path.ends_with("?$select=isDraft"))
        );
    }

    #[test]
    fn folder_exists_is_true_on_200_false_on_404_and_errs_otherwise() {
        let srv = FakeServer::start(vec![
            get("/me/mailFolders/HERE?", 200, r#"{"id":"HERE"}"#.into()),
            get("/me/mailFolders/GONE?", 404, "{}".into()),
            get("/me/mailFolders/BAD?", 500, "boom".into()),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert_eq!(c.folder_exists("HERE"), Ok(true));
        assert_eq!(c.folder_exists("GONE"), Ok(false));
        assert!(matches!(
            c.folder_exists("BAD"),
            Err(GraphError::Http { status: 500, .. })
        ));
        assert!(
            srv.requests()
                .iter()
                .all(|r| r.path.ends_with("?$select=id"))
        );
    }

    #[test]
    fn list_folders_drops_a_folder_without_an_id_and_is_incomplete() {
        let srv = FakeServer::start(vec![get(
            "/me/mailFolders?",
            200,
            format!(
                r#"{{"value":[{},{{"displayName":"no id","childFolderCount":0}}]}}"#,
                folder_json("F1", None, Some(0))
            ),
        )]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let listing = c.list_folders().unwrap();
        assert_eq!(ids(&listing), vec!["F1"]);
        assert!(!listing.complete);
    }

    #[test]
    fn child_folders_401_propagates_unauthorized() {
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders/F1/childFolders".into(),
                status: 401,
                headers: vec![],
                body: "{}".into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/mailFolders".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"F1","displayName":"Inbox","parentFolderId":null,"totalItemCount":1,"unreadItemCount":0,"wellKnownName":"inbox"}]}"#.into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(c.list_folders(), Err(GraphError::Unauthorized)));
    }

    fn sample_message_json(id: &str) -> String {
        format!(
            r#"{{"id":"{id}","conversationId":"C1","subject":"Re: Hi",
            "from":{{"emailAddress":{{"name":"A","address":"a@x"}}}},
            "toRecipients":[],"ccRecipients":[],
            "receivedDateTime":"","sentDateTime":"","isRead":false,
            "hasAttachments":false,"importance":"normal","bodyPreview":""}}"#
        )
    }

    #[test]
    fn create_reply_posts_and_parses_draft() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/createReply".into(),
            status: 200,
            headers: vec![],
            body: sample_message_json("DRAFT1"),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let draft = c.create_reply("M1", false).unwrap();
        assert_eq!(draft.id, "DRAFT1");
        assert_eq!(draft.subject, "Re: Hi");
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "POST");
        assert!(reqs[0].path.ends_with("/createReply"));
    }

    #[test]
    fn create_reply_all_posts_to_create_reply_all() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/createReplyAll".into(),
            status: 200,
            headers: vec![],
            body: sample_message_json("DRAFT2"),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let draft = c.create_reply("M1", true).unwrap();
        assert_eq!(draft.id, "DRAFT2");
        let reqs = srv.requests();
        assert!(reqs[0].path.ends_with("/createReplyAll"));
    }

    #[test]
    fn create_forward_posts_and_parses_draft() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/createForward".into(),
            status: 200,
            headers: vec![],
            body: sample_message_json("DRAFT3"),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let draft = c.create_forward("M1").unwrap();
        assert_eq!(draft.id, "DRAFT3");
        let reqs = srv.requests();
        assert!(reqs[0].path.ends_with("/createForward"));
    }

    /// A recorded request's `Content-Length`, the name matched in any case.
    fn content_length(req: &crate::testserver::RecordedRequest) -> Option<&str> {
        req.headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.as_str())
    }

    /// The bodiless POSTs frame an empty body, `Content-Length: 0`: without
    /// it an Exchange Online front end answers `411 Length Required` (#1954).
    #[test]
    fn bodiless_posts_send_content_length_zero() {
        let post = |path: &str, status: u16, body: String| Route {
            method: "POST".into(),
            path_prefix: path.into(),
            status,
            headers: vec![],
            body,
        };
        // `path_prefix` matches by prefix: `createReplyAll` before `createReply`.
        let srv = FakeServer::start(vec![
            post(
                "/me/messages/M1/createReplyAll",
                200,
                sample_message_json("D2"),
            ),
            post(
                "/me/messages/M1/createReply",
                200,
                sample_message_json("D1"),
            ),
            post(
                "/me/messages/M1/createForward",
                200,
                sample_message_json("D3"),
            ),
            post("/me/messages/D1/send", 202, String::new()),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.create_reply("M1", false).unwrap();
        c.create_reply("M1", true).unwrap();
        c.create_forward("M1").unwrap();
        c.send_draft("D1").unwrap();
        let reqs = srv.requests();
        let paths: Vec<&str> = reqs.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "/me/messages/M1/createReply",
                "/me/messages/M1/createReplyAll",
                "/me/messages/M1/createForward",
                "/me/messages/D1/send",
            ]
        );
        for req in &reqs {
            assert_eq!(req.method, "POST");
            assert_eq!(content_length(req), Some("0"), "{}", req.path);
            assert_eq!(req.body, "", "{}", req.path);
        }
    }

    /// GET and DELETE stay bodiless: no `Content-Length` header (#1954).
    #[test]
    fn get_and_delete_send_no_content_length() {
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/messages/M1".into(),
                status: 200,
                headers: vec![],
                body: r#"{"isDraft":true}"#.into(),
            },
            Route {
                method: "DELETE".into(),
                path_prefix: "/me/messages/M1".into(),
                status: 204,
                headers: vec![],
                body: String::new(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(c.is_draft("M1").unwrap());
        c.delete_message("M1").unwrap();
        let reqs = srv.requests();
        let methods: Vec<&str> = reqs.iter().map(|r| r.method.as_str()).collect();
        assert_eq!(methods, ["GET", "DELETE"]);
        for req in &reqs {
            assert_eq!(content_length(req), None, "{} {}", req.method, req.path);
        }
    }

    /// The IIS page an Exchange Online front end answers a bodiless POST
    /// with (#1954), as the issue quotes it.
    const IIS_411_HTML: &str = "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01//EN\" \"http://www.w3.org/TR/html4/strict.dtd\">\r\n<HTML><HEAD><TITLE>Length Required</TITLE>\r\n<META HTTP-EQUIV=\"Content-Type\" Content=\"text/html; charset=us-ascii\"></HEAD>\r\n<BODY><h2>Length Required</h2>\r\n<hr><p>HTTP Error 411. The request must be chunked or have a content length.</p>\r\n</BODY></HTML>\r\n";

    /// A 411 HTML page keeps its body in the error but displays as one line
    /// without markup (#1954).
    #[test]
    fn http_411_html_displays_one_line() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/createReplyAll".into(),
            status: 411,
            headers: vec![("Content-Type".into(), "text/html".into())],
            body: IIS_411_HTML.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let err = c.create_reply("M1", true).unwrap_err();
        assert_eq!(
            err,
            GraphError::Http {
                status: 411,
                body: IIS_411_HTML.into(),
            }
        );
        let text = err.to_string();
        assert_eq!(text, "HTTP 411 Length Required");
        assert!(!text.contains('<') && !text.contains('\n'), "{text}");
    }

    fn http(status: u16, body: &str) -> GraphError {
        GraphError::Http {
            status,
            body: body.into(),
        }
    }

    #[test]
    fn http_error_display_uses_graphs_own_message() {
        assert_eq!(
            http(
                403,
                r#"{"error":{"code":"ErrorAccessDenied","message":"  Access is denied.  "}}"#
            )
            .to_string(),
            "HTTP 403: Access is denied."
        );
        // A multi-line message becomes one line; tabs and control characters
        // go too.
        assert_eq!(
            http(
                400,
                r#"{"error":{"code":"X","message":"Bad id.\r\n\tTry\u0007 again."}}"#
            )
            .to_string(),
            "HTTP 400: Bad id. Try again."
        );
    }

    #[test]
    fn http_error_display_without_a_message_uses_the_reason_phrase() {
        // Blank or missing `error.message`: no code fallback.
        assert_eq!(
            http(400, r#"{"error":{"code":"X","message":"  "}}"#).to_string(),
            "HTTP 400 Bad Request"
        );
        assert_eq!(
            http(400, r#"{"error":{"code":"X"}}"#).to_string(),
            "HTTP 400 Bad Request"
        );
        assert_eq!(
            http(500, "boom").to_string(),
            "HTTP 500 Internal Server Error"
        );
        assert_eq!(http(502, "").to_string(), "HTTP 502 Bad Gateway");
    }

    #[test]
    fn http_error_display_of_an_unlisted_status_is_the_bare_status() {
        let text = http(
            599,
            "<html><head><title>Odd\r\nThing</title></head>\n<body>x</body></html>",
        )
        .to_string();
        assert_eq!(text, "HTTP 599");
        assert!(!text.contains('<') && !text.contains('\n'), "{text}");
    }

    #[test]
    fn http_error_display_caps_a_long_message() {
        let long = "é".repeat(500);
        let text = http(400, &format!(r#"{{"error":{{"message":"{long}"}}}}"#)).to_string();
        assert_eq!(text.chars().count(), HTTP_ERROR_TEXT_MAX);
        assert!(text.starts_with("HTTP 400: éé"), "{text}");
        assert!(text.ends_with('…'), "{text}");
        // At the cap exactly: nothing is cut.
        let fits = "a".repeat(HTTP_ERROR_TEXT_MAX - "HTTP 400: ".len());
        let text = http(400, &format!(r#"{{"error":{{"message":"{fits}"}}}}"#)).to_string();
        assert_eq!(text, format!("HTTP 400: {fits}"));
    }

    #[test]
    fn one_line_collapses_whitespace_and_cuts_on_a_char() {
        assert_eq!(one_line("  a \r\n\t b  ", 10), "a b");
        assert_eq!(one_line("a\u{0}b c", 10), "ab c");
        assert_eq!(one_line("abcdef", 4), "abc…");
        assert_eq!(one_line("ééééé", 3), "éé…");
        assert_eq!(one_line("", 3), "");
    }

    #[test]
    fn create_draft_posts_body_and_parses_returned_draft() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages".into(),
            status: 201,
            headers: vec![],
            body: sample_message_json("NEWDRAFT"),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let to = vec![Recipient {
            name: "Bob".to_string(),
            address: "bob@x".to_string(),
        }];
        let cc = vec![Recipient {
            name: "Cara".to_string(),
            address: "cara@x".to_string(),
        }];
        let draft = c.create_draft("<p>hi</p>", "Hello", &to, &cc, &[]).unwrap();
        assert_eq!(draft.id, "NEWDRAFT");
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/messages");
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(sent.get("subject").and_then(Value::as_str), Some("Hello"));
        assert_eq!(
            sent.get("body")
                .and_then(|b| b.get("contentType"))
                .and_then(Value::as_str),
            Some("HTML")
        );
        assert_eq!(
            sent.get("body")
                .and_then(|b| b.get("content"))
                .and_then(Value::as_str),
            Some("<p>hi</p>")
        );
        let to_out = sent.get("toRecipients").and_then(Value::as_array).unwrap();
        assert_eq!(
            to_out[0]
                .get("emailAddress")
                .and_then(|e| e.get("address"))
                .and_then(Value::as_str),
            Some("bob@x")
        );
        let cc_out = sent.get("ccRecipients").and_then(Value::as_array).unwrap();
        assert_eq!(
            cc_out[0]
                .get("emailAddress")
                .and_then(|e| e.get("address"))
                .and_then(Value::as_str),
            Some("cara@x")
        );
    }

    #[test]
    fn create_draft_includes_bcc_recipients() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages".into(),
            status: 201,
            headers: vec![],
            body: sample_message_json("NEWDRAFT"),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let to = [Recipient {
            name: "B".into(),
            address: "b@x".into(),
        }];
        let bcc = [Recipient {
            name: "S".into(),
            address: "secret@x".into(),
        }];
        let _ = c.create_draft("<p>hi</p>", "Sub", &to, &[], &bcc);
        let reqs = srv.requests();
        let sent = json::parse(&reqs[0].body).unwrap();
        let bccs = sent.get("bccRecipients").and_then(Value::as_array).unwrap();
        assert_eq!(bccs.len(), 1);
        assert_eq!(
            bccs[0]
                .get("emailAddress")
                .and_then(|e| e.get("address"))
                .and_then(Value::as_str),
            Some("secret@x")
        );
    }

    #[test]
    fn update_draft_patches_body_and_returns_unit() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.update_draft("M1", "<p>edit</p>", "Subj", &[], &[], &[])
            .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH");
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(sent.get("subject").and_then(Value::as_str), Some("Subj"));
        assert_eq!(
            sent.get("body")
                .and_then(|b| b.get("contentType"))
                .and_then(Value::as_str),
            Some("HTML")
        );
    }

    #[test]
    fn send_draft_posts_to_send_and_maps_202_to_ok() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/send".into(),
            status: 202,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.send_draft("M1").unwrap();
        let reqs = srv.requests();
        assert!(reqs[0].path.ends_with("/send"));
    }

    #[test]
    fn send_draft_maps_200_to_ok() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/send".into(),
            status: 200,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.send_draft("M1").unwrap();
    }

    #[test]
    fn send_draft_401_maps_to_unauthorized() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/send".into(),
            status: 401,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(c.send_draft("M1"), Err(GraphError::Unauthorized)));
    }

    #[test]
    fn create_draft_401_maps_to_unauthorized() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages".into(),
            status: 401,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(
            c.create_draft("x", "y", &[], &[], &[]),
            Err(GraphError::Unauthorized)
        ));
    }

    fn sample_event_json(id: &str, start: &str) -> String {
        format!(
            r#"{{"id":"{id}","subject":"s{id}",
            "start":{{"dateTime":"{start}.0000000","timeZone":"UTC"}},
            "end":{{"dateTime":"{start}.0000000","timeZone":"UTC"}},
            "isAllDay":false,
            "location":{{"displayName":"Room"}},
            "organizer":{{"emailAddress":{{"name":"Org","address":"org@x"}}}},
            "responseStatus":{{"response":"accepted"}},
            "seriesMasterId":null,
            "bodyPreview":"p","webLink":"https://x/{id}",
            "lastModifiedDateTime":"2026-07-17T00:00:00Z",
            "body":{{"contentType":"html","content":"b"}},
            "attendees":[{{"type":"required","status":{{"response":"none"}},"emailAddress":{{"name":"A","address":"a@x"}}}}]
            }}"#
        )
    }

    #[test]
    fn calendar_view_parses_events_with_utc_times_and_prefer_header() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/calendarView".into(),
            status: 200,
            headers: vec![],
            body: format!(
                r#"{{"value":[{},{}]}}"#,
                sample_event_json("E1", "2026-07-18T09:00:00"),
                sample_event_json("E2", "2026-07-18T11:00:00")
            ),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let events = c
            .calendar_view("2026-07-18T00:00:00Z", "2026-07-19T00:00:00Z")
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(events[0].start_utc.ends_with('Z'));
        assert_eq!(events[0].start_utc, "2026-07-18T09:00:00Z");
        assert_eq!(events[0].response_status, "accepted");
        assert_eq!(events[0].attendees.len(), 1);
        assert_eq!(events[0].organizer_addr, "org@x");

        let reqs = srv.requests();
        assert!(
            reqs[0]
                .headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("prefer") && v == "outlook.timezone=\"UTC\"")
        );
        assert!(reqs[0].path.contains("startDateTime="));
        assert!(reqs[0].path.contains("endDateTime="));
    }

    #[test]
    fn calendar_view_follows_next_link_pagination() {
        // Order matters, same as `list_folders_recurses_into_child_folders`:
        // the fake server matches the *first* route whose path_prefix is a
        // prefix of the request path, so the more specific second-page route
        // must come before the generic first-page route. The nextLink is
        // given here as a path (not a full URL) — `GraphClient::full_url`
        // joins any target that doesn't already start with `http(s)://`
        // onto `base`, so a bare path exercises the same "follow the link
        // as-is" logic a real absolute nextLink would.
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/calendarView?$skiptoken=P2".into(),
                status: 200,
                headers: vec![],
                body: format!(
                    r#"{{"value":[{}]}}"#,
                    sample_event_json("E2", "2026-07-18T11:00:00")
                ),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/calendarView".into(),
                status: 200,
                headers: vec![],
                body: format!(
                    r#"{{"value":[{}],"@odata.nextLink":"/me/calendarView?$skiptoken=P2"}}"#,
                    sample_event_json("E1", "2026-07-18T09:00:00"),
                ),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let events = c
            .calendar_view("2026-07-18T00:00:00Z", "2026-07-19T00:00:00Z")
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].id, "E1");
        assert_eq!(events[1].id, "E2");
    }

    #[test]
    fn calendar_view_in_encodes_the_id_and_pages() {
        // The calendar id holds `/`, `+` and `=`; the second page is the
        // more specific prefix, so it goes first (first prefix match wins).
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/calendars/A%2Fb%2B%3D/calendarView?$skiptoken=P2".into(),
                status: 200,
                headers: vec![],
                body: format!(
                    r#"{{"value":[{}]}}"#,
                    sample_event_json("E2", "2026-07-18T11:00:00")
                ),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/calendars/A%2Fb%2B%3D/calendarView?startDateTime=".into(),
                status: 200,
                headers: vec![],
                body: format!(
                    r#"{{"value":[{}],"@odata.nextLink":"/me/calendars/A%2Fb%2B%3D/calendarView?$skiptoken=P2"}}"#,
                    sample_event_json("E1", "2026-07-18T09:00:00"),
                ),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let events = c
            .calendar_view_in("A/b+=", "2026-07-18T00:00:00Z", "2026-07-19T00:00:00Z")
            .unwrap();
        let ids: Vec<&str> = events.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["E1", "E2"]);
        assert!(events.iter().all(|e| e.calendar_id == "A/b+="));
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 2);
        assert!(
            reqs[0]
                .path
                .contains("endDateTime=2026-07-19T00:00:00Z&$top=50")
        );
        for r in &reqs {
            assert!(
                r.headers
                    .iter()
                    .any(|(k, v)| k.eq_ignore_ascii_case("prefer")
                        && v == "outlook.timezone=\"UTC\"")
            );
        }
    }

    fn sends_calendar_delta_prefer(r: &crate::testserver::RecordedRequest) -> bool {
        r.headers.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("prefer")
                && v == "outlook.timezone=\"UTC\", odata.maxpagesize=50"
        })
    }

    #[test]
    fn calendar_delta_start_encodes_id_window_and_prefer_headers() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/calendars/A%2Fb%2B%3D/calendarView/delta?".into(),
            status: 200,
            headers: vec![],
            body: format!(
                r#"{{"value":[{},{{"id":"GONE","@removed":{{"reason":"deleted"}}}}],"@odata.nextLink":"/cal-delta/A?page=2"}}"#,
                sample_event_json("E1", "2026-07-18T09:00:00"),
            ),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let page = c
            .calendar_delta(CalendarDeltaCursor::Start {
                calendar_id: "A/b+=".to_string(),
                from_utc: "2026-07-18T00:00:00Z".to_string(),
                to_utc: "2026-07-19T00:00:00Z".to_string(),
            })
            .unwrap();
        let EventDeltaItem::Upsert(e) = &page.items[0] else {
            panic!("an upsert first")
        };
        assert_eq!((e.id.as_str(), e.calendar_id.as_str()), ("E1", "A/b+="));
        assert_eq!(page.items[1], EventDeltaItem::Removed("GONE".to_string()));
        assert_eq!(page.next_link.as_deref(), Some("/cal-delta/A?page=2"));
        let reqs = srv.requests();
        assert_eq!(
            reqs[0].path,
            "/me/calendars/A%2Fb%2B%3D/calendarView/delta\
             ?startDateTime=2026-07-18T00:00:00Z&endDateTime=2026-07-19T00:00:00Z"
        );
        assert!(sends_calendar_delta_prefer(&reqs[0]));
    }

    #[test]
    fn calendar_delta_link_cursor_uses_url_as_is() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/cal-delta/A?token=2".into(),
            status: 200,
            headers: vec![],
            body: format!(
                r#"{{"value":[{}],"@odata.deltaLink":"/cal-delta/A?token=3"}}"#,
                sample_event_json("E1", "2026-07-18T09:00:00"),
            ),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let link = format!("{}/cal-delta/A?token=2", srv.base_url);
        let page = c.calendar_delta(CalendarDeltaCursor::Link(link)).unwrap();
        let EventDeltaItem::Upsert(e) = &page.items[0] else {
            panic!("an upsert")
        };
        // A link knows no calendar: the caller sets it.
        assert_eq!(e.calendar_id, "");
        assert_eq!(page.delta_link.as_deref(), Some("/cal-delta/A?token=3"));
        let reqs = srv.requests();
        assert_eq!(reqs[0].path, "/cal-delta/A?token=2");
        // The time zone preference is not in the link: it is sent again.
        assert!(sends_calendar_delta_prefer(&reqs[0]));
    }

    #[test]
    fn calendar_delta_page_without_any_link_is_a_parse_error() {
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/no-link".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[]}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/no-value".into(),
                status: 200,
                headers: vec![],
                body: r#"{"@odata.deltaLink":"/x"}"#.into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        for path in ["/no-link", "/no-value"] {
            let got = c.calendar_delta(CalendarDeltaCursor::Link(path.to_string()));
            assert!(matches!(got, Err(GraphError::Parse(_))), "{path}: {got:?}");
        }
    }

    #[test]
    fn list_calendars_follows_next_link() {
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/calendars?$skip=100".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"C2","name":"Birthdays"},{"name":"no id"}]}"#.into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/calendars?".into(),
                status: 200,
                headers: vec![],
                body: r##"{"value":[{"id":"C1","name":"Calendar","color":"auto","hexColor":"",
                    "isDefaultCalendar":true,"canEdit":true,
                    "owner":{"name":"Ann","address":"ann@x.com"}}],
                    "@odata.nextLink":"/me/calendars?$skip=100"}"##
                    .into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let cals = c.list_calendars().unwrap();
        let ids: Vec<&str> = cals.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["C1", "C2"]);
        assert!(cals[0].is_default && cals[0].can_edit);
        assert_eq!(cals[0].owner_addr, "ann@x.com");
        let reqs = srv.requests();
        assert_eq!(
            reqs[0].path,
            "/me/calendars?$select=id,name,color,hexColor,isDefaultCalendar,canEdit,owner&$top=100"
        );
        assert_eq!(reqs[1].path, "/me/calendars?$skip=100");
    }

    #[test]
    fn list_calendars_fails_whole_when_a_page_fails() {
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/calendars?$skip=100".into(),
                status: 500,
                headers: vec![],
                body: "{}".into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/calendars?".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"id":"C1"}],"@odata.nextLink":"/me/calendars?$skip=100"}"#
                    .into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(c.list_calendars().is_err());
    }

    #[test]
    fn calendar_view_401_maps_to_unauthorized() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/calendarView".into(),
            status: 401,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(
            c.calendar_view("2026-07-18T00:00:00Z", "2026-07-19T00:00:00Z"),
            Err(GraphError::Unauthorized)
        ));
    }

    #[test]
    fn get_master_categories_parses_value_array() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/outlook/masterCategories".into(),
            status: 200,
            headers: vec![],
            body: r#"{"value":[{"id":"c1","displayName":"Work","color":"preset0"},{"id":"c2","displayName":"Urgent","color":"preset1"}]}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let cats = c.get_master_categories().unwrap();
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0].display_name, "Work");
        assert_eq!(cats[1].color, "preset1");
    }

    #[test]
    fn set_message_categories_patches_array() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.set_message_categories("M1", &["Work".to_string(), "Urgent".to_string()])
            .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH");
        let sent = json::parse(&reqs[0].body).unwrap();
        let arr = sent.get("categories").and_then(Value::as_array).unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].as_str(), Some("Work"));
    }

    fn photo_route(path: &str, status: u16, body: &str, headers: &[(&str, &str)]) -> Route {
        Route {
            method: "GET".into(),
            path_prefix: path.into(),
            status,
            headers: headers
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            body: body.into(),
        }
    }

    const PHOTO: &str = "/me/photo/$value";
    const PHOTO_48: &str = "/me/photos/48x48/$value";

    #[test]
    fn me_parses_profile() {
        let srv = FakeServer::start(vec![photo_route(
            "/me?$select",
            200,
            r#"{"displayName":"Ada Lovelace","mail":"ada@contoso.com","userPrincipalName":"ada@contoso.onmicrosoft.com"}"#,
            &[],
        )]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let me = c.me().unwrap();
        assert_eq!(me.display_name, "Ada Lovelace");
        assert_eq!(me.address(), "ada@contoso.com");
        assert_eq!(me.user_principal_name, "ada@contoso.onmicrosoft.com");
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            reqs[0].path,
            "/me?$select=displayName,mail,userPrincipalName"
        );
    }

    #[test]
    fn me_without_mail_uses_upn() {
        let srv = FakeServer::start(vec![photo_route(
            "/me?$select",
            200,
            r#"{"displayName":"Ada","mail":null,"userPrincipalName":"ada@outlook.com"}"#,
            &[],
        )]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let me = c.me().unwrap();
        assert_eq!(me.mail, "");
        assert_eq!(me.address(), "ada@outlook.com");
    }

    #[test]
    fn my_photo_returns_bytes_and_type() {
        let srv = FakeServer::start(vec![photo_route(
            PHOTO,
            200,
            "JPEGDATA",
            &[("Content-Type", "image/jpeg; charset=binary")],
        )]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let photo = c.my_photo().unwrap();
        assert_eq!(
            photo,
            Some((b"JPEGDATA".to_vec(), "image/jpeg".to_string()))
        );
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].path, PHOTO);
        let accepts: Vec<&str> = reqs[0]
            .headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("accept"))
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(accepts, ["image/*"]);
    }

    #[test]
    fn my_photo_without_content_type_is_octet_stream() {
        let srv = FakeServer::start(vec![photo_route(PHOTO, 200, "RAW", &[])]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let (_, ty) = c.my_photo().unwrap().unwrap();
        assert_eq!(ty, "application/octet-stream");
    }

    #[test]
    fn my_photo_falls_back_to_48x48() {
        let srv = FakeServer::start(vec![
            photo_route(PHOTO, 404, r#"{"error":{"code":"ImageNotFound"}}"#, &[]),
            photo_route(PHOTO_48, 200, "SMALL", &[("Content-Type", "image/png")]),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let photo = c.my_photo().unwrap();
        assert_eq!(photo, Some((b"SMALL".to_vec(), "image/png".to_string())));
        let paths: Vec<String> = srv.requests().into_iter().map(|r| r.path).collect();
        assert_eq!(paths, [PHOTO, PHOTO_48]);
    }

    #[test]
    fn my_photo_none_when_both_404() {
        let srv = FakeServer::start(vec![
            photo_route(PHOTO, 404, "{}", &[]),
            photo_route(PHOTO_48, 404, "{}", &[]),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert_eq!(c.my_photo(), Ok(None));
        assert_eq!(srv.requests().len(), 2);
    }

    #[test]
    fn my_photo_refuses_oversize() {
        let big = "x".repeat(PHOTO_MAX + 1);
        let srv = FakeServer::start(vec![photo_route(
            PHOTO,
            200,
            &big,
            &[("Content-Type", "image/jpeg")],
        )]);
        let c = GraphClient::new(&srv.base_url, "AT");
        match c.my_photo() {
            Err(GraphError::Parse(m)) => assert!(m.contains("larger than"), "{m}"),
            other => panic!(
                "expected a Parse error, got {:?}",
                other.map(|p| p.map(|(b, t)| (b.len(), t)))
            ),
        }
    }

    #[test]
    fn my_photo_accepts_exactly_the_cap() {
        let max = "x".repeat(PHOTO_MAX);
        let srv = FakeServer::start(vec![photo_route(
            PHOTO,
            200,
            &max,
            &[("Content-Type", "image/jpeg")],
        )]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let (bytes, _) = c.my_photo().unwrap().unwrap();
        assert_eq!(bytes.len(), PHOTO_MAX);
    }

    #[test]
    fn my_photo_propagates_other_errors() {
        // No fallback to 48x48 on anything but a 404.
        for (status, want) in [
            (
                500,
                GraphError::Http {
                    status: 500,
                    body: "boom".into(),
                },
            ),
            (401, GraphError::Unauthorized),
            (
                503,
                GraphError::Throttled {
                    retry_after_secs: 30,
                },
            ),
        ] {
            let srv = FakeServer::start(vec![
                photo_route(PHOTO, status, "boom", &[]),
                photo_route(PHOTO_48, 200, "SMALL", &[("Content-Type", "image/png")]),
            ]);
            let c = GraphClient::new(&srv.base_url, "AT");
            assert_eq!(c.my_photo(), Err(want));
            assert_eq!(srv.requests().len(), 1, "status {status}");
        }
        // A 404 then any other error on 48x48 is that error, not `None`.
        let srv = FakeServer::start(vec![
            photo_route(PHOTO, 404, "{}", &[]),
            photo_route(PHOTO_48, 500, "boom", &[]),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert_eq!(
            c.my_photo(),
            Err(GraphError::Http {
                status: 500,
                body: "boom".into()
            })
        );
    }

    #[test]
    fn read_capped_refuses_an_endless_body() {
        // `repeat` never ends: only the cap stops the read.
        match read_capped(std::io::repeat(b'x'), 1024, photo_too_large) {
            Err(GraphError::Parse(m)) => assert!(m.contains("larger than 1024"), "{m}"),
            other => panic!("expected a Parse error, got {:?}", other.map(|b| b.len())),
        }
    }

    #[test]
    fn read_capped_accepts_exactly_max() {
        use std::io::Read;
        let bytes = read_capped(std::io::repeat(b'x').take(1024), 1024, photo_too_large).unwrap();
        assert_eq!(bytes.len(), 1024);
        assert_eq!(
            read_capped(&b""[..], 1024, photo_too_large).unwrap(),
            Vec::<u8>::new()
        );
    }

    /// A GET of `body` from a fake server, as `send` would hand it on.
    fn response_with(body: &str) -> (FakeServer, ureq::Response) {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/r".into(),
            status: 200,
            headers: vec![],
            body: body.into(),
        }]);
        let resp = ureq::get(&format!("{}/r", srv.base_url)).call().unwrap();
        (srv, resp)
    }

    #[test]
    fn a_response_over_its_cap_is_a_parse_error_not_a_transport_one() {
        // The fake server declares its `Content-Length`: refused before
        // reading (#1978). A response that yields more than it declared, or
        // declares nothing, is stopped by `read_capped` the same way.
        let body = r#"{"a":"0123456789"}"#;
        let (_srv, resp) = response_with(body);
        match parse_body_capped(resp, body.len() - 1) {
            Err(GraphError::Parse(m)) => assert!(m.contains("response is larger than"), "{m}"),
            other => panic!("expected a Parse error, got {other:?}"),
        }
        match read_capped(std::io::repeat(b'x'), 16, response_too_large) {
            Err(GraphError::Parse(m)) => assert!(m.contains("response is larger than 16"), "{m}"),
            other => panic!("expected a Parse error, got {:?}", other.map(|b| b.len())),
        }
        let (_srv, resp) = response_with(body);
        let v = parse_body_capped(resp, body.len()).unwrap();
        assert_eq!(v.get("a").and_then(Value::as_str), Some("0123456789"));
    }

    #[test]
    fn an_oversize_message_body_is_a_parse_error() {
        // Over `BODY_MAX`: before #1978 ureq's `into_string` refused it as an
        // io error, which read as `Transport` (offline, retried for ever).
        let content = "x".repeat(BODY_MAX);
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/BIG?$select=body".into(),
            status: 200,
            headers: vec![],
            body: format!(r#"{{"body":{{"contentType":"text","content":"{content}"}}}}"#),
        }]);
        let c = GraphClient::new(&srv.base_url, "tok");
        match c.get_body("BIG", true) {
            Err(GraphError::Parse(m)) => assert!(m.contains("larger than"), "{m}"),
            other => panic!("expected a Parse error, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn get_automatic_replies_parses_setting() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/mailboxSettings".into(),
            status: 200,
            headers: vec![],
            body: r#"{"automaticRepliesSetting":{"status":"alwaysEnabled","externalAudience":"all","internalReplyMessage":"<p>hi</p>","externalReplyMessage":"bye","scheduledStartDateTime":{"dateTime":"0001-01-01T00:00:00.0000000","timeZone":"UTC"},"scheduledEndDateTime":{"dateTime":"0001-01-01T00:00:00.0000000","timeZone":"UTC"}}}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let r = c.get_automatic_replies().unwrap();
        assert_eq!(r.status, OofStatus::AlwaysEnabled);
        assert_eq!(r.internal_message, "hi");
        assert_eq!(r.external_message, "bye");
    }

    #[test]
    fn set_automatic_replies_scheduled_patches_full_body() {
        use crate::graph::model::ExternalAudience;
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/mailboxSettings".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.set_automatic_replies(&AutomaticReplies {
            status: OofStatus::Scheduled,
            external_audience: ExternalAudience::ContactsOnly,
            internal_message: "Away <b>&</b>\nback".into(),
            external_message: "Out".into(),
            scheduled_start_utc: "2026-07-20T09:00:00Z".into(),
            scheduled_end_utc: "2026-07-27T17:00:00Z".into(),
        })
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH");
        let sent = json::parse(&reqs[0].body).unwrap();
        let setting = sent.get("automaticRepliesSetting").unwrap();
        assert_eq!(
            setting.get("status").and_then(Value::as_str),
            Some("scheduled")
        );
        assert_eq!(
            setting.get("externalAudience").and_then(Value::as_str),
            Some("contactsOnly")
        );
        // plain_to_html escaped the `&`/`<`/`>` and encoded the newline.
        assert_eq!(
            setting.get("internalReplyMessage").and_then(Value::as_str),
            Some("Away &lt;b&gt;&amp;&lt;/b&gt;<br>back")
        );
        // Scheduled → both datetime objects present, timeZone UTC.
        assert_eq!(
            setting
                .get("scheduledStartDateTime")
                .and_then(|d| d.get("dateTime"))
                .and_then(Value::as_str),
            Some("2026-07-20T09:00:00Z")
        );
        assert_eq!(
            setting
                .get("scheduledEndDateTime")
                .and_then(|d| d.get("timeZone"))
                .and_then(Value::as_str),
            Some("UTC")
        );
    }

    #[test]
    fn set_automatic_replies_disabled_omits_datetimes() {
        use crate::graph::model::ExternalAudience;
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/mailboxSettings".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.set_automatic_replies(&AutomaticReplies {
            status: OofStatus::Disabled,
            external_audience: ExternalAudience::All,
            internal_message: "x".into(),
            external_message: "y".into(),
            scheduled_start_utc: "".into(),
            scheduled_end_utc: "".into(),
        })
        .unwrap();
        let reqs = srv.requests();
        let sent = json::parse(&reqs[0].body).unwrap();
        let setting = sent.get("automaticRepliesSetting").unwrap();
        assert!(setting.get("scheduledStartDateTime").is_none());
        assert!(setting.get("scheduledEndDateTime").is_none());
    }

    #[test]
    fn meeting_event_id_reads_expanded_event() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"M1","event":{"id":"E1"}}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert_eq!(c.meeting_event_id("M1").unwrap().as_deref(), Some("E1"));
        // The request carries the $expand for the invite's event id.
        let reqs = srv.requests();
        assert!(reqs[0].path.contains("expand"));
    }

    #[test]
    fn meeting_event_id_is_none_without_event() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M2".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"M2"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert_eq!(c.meeting_event_id("M2").unwrap(), None);
    }

    #[test]
    fn meeting_message_reads_cancellation_and_event() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M1?$expand=microsoft.graph.eventMessage/event(".into(),
            status: 200,
            headers: vec![],
            body: r##"{"id":"M1","@odata.type":"#microsoft.graph.eventMessage",
                "meetingMessageType":"meetingCancelled",
                "event":{"id":"E9","isOrganizer":false}}"##
                .into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert_eq!(
            c.meeting_message("M1").unwrap(),
            MeetingMessage {
                message_type: Some(MeetingMessageType::Cancelled),
                event_id: Some("E9".into()),
                is_organizer: false,
            }
        );
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "GET");
        assert_eq!(
            reqs[0].path,
            "/me/messages/M1?$expand=microsoft.graph.eventMessage/event($select=id,isOrganizer)"
        );
    }

    #[test]
    fn meeting_message_reads_a_response() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M3".into(),
            status: 200,
            headers: vec![],
            body: r##"{"id":"M3","@odata.type":"#microsoft.graph.eventMessageResponse",
                "meetingMessageType":"meetingAccepted",
                "event":{"id":"E3","isOrganizer":true}}"##
                .into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let m = c.meeting_message("M3").unwrap();
        assert_eq!(m.message_type, Some(MeetingMessageType::Accepted));
        assert_eq!(m.event_id.as_deref(), Some("E3"));
        assert!(m.is_organizer);
    }

    #[test]
    fn meeting_message_of_a_plain_message_is_none() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/M2".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"M2","subject":"Lunch?"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let m = c.meeting_message("M2").unwrap();
        assert_eq!(m.message_type, None);
        assert_eq!(m.event_id, None);
        assert!(!m.is_organizer);
    }

    #[test]
    fn meeting_message_id_is_percent_encoded() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/AAMk%2Fid%2Bwith%3Dchars?".into(),
            status: 200,
            headers: vec![],
            body: r#"{"meetingMessageType":"meetingRequest","event":{"id":"E1"}}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let m = c.meeting_message("AAMk/id+with=chars").unwrap();
        assert_eq!(m.message_type, Some(MeetingMessageType::Request));
        assert_eq!(srv.requests().len(), 1);
    }

    #[test]
    fn meeting_message_missing_is_not_found() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/messages/GONE".into(),
            status: 404,
            headers: vec![],
            body: r#"{"error":{"code":"ErrorItemNotFound"}}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(
            c.meeting_message("GONE"),
            Err(GraphError::NotFound)
        ));
    }

    #[test]
    fn get_schedule_posts_body_and_parses_view() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/calendar/getSchedule".into(),
            status: 200,
            headers: vec![],
            body: r#"{"value":[{"scheduleId":"me@x","availabilityView":"000222"},{"scheduleId":"alice@x","availabilityView":"220000"}]}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let entries = c
            .get_schedule(
                &["me@x".to_string(), "alice@x".to_string()],
                "2026-07-21T08:00:00Z",
                "2026-07-21T18:00:00Z",
                30,
            )
            .unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].email, "alice@x");
        assert_eq!(entries[0].availability, "000222");
        let sent = json::parse(&srv.requests()[0].body).unwrap();
        assert_eq!(
            sent.get("availabilityViewInterval").and_then(Value::as_i64),
            Some(30)
        );
        assert_eq!(
            sent.get("schedules")
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            sent.get("startTime")
                .unwrap()
                .get("dateTime")
                .and_then(Value::as_str),
            Some("2026-07-21T08:00:00")
        );
    }

    #[test]
    fn respond_event_decline_with_proposed_time_sends_proposed_new_time() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/E1/decline".into(),
            status: 202,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.respond_event(
            "E1",
            RsvpKind::Decline,
            Some("can we push?"),
            true,
            Some(("2026-07-21T14:00:00Z".into(), "2026-07-21T15:00:00Z".into())),
        )
        .unwrap();
        let sent = json::parse(&srv.requests()[0].body).unwrap();
        let pnt = sent.get("proposedNewTime").unwrap();
        assert_eq!(
            pnt.get("start")
                .unwrap()
                .get("dateTime")
                .and_then(Value::as_str),
            Some("2026-07-21T14:00:00")
        );
        assert_eq!(
            pnt.get("end")
                .unwrap()
                .get("timeZone")
                .and_then(Value::as_str),
            Some("UTC")
        );
    }

    #[test]
    fn respond_event_tentative_with_proposed_time_sends_proposed_new_time() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/E1/tentativelyAccept".into(),
            status: 202,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.respond_event(
            "E1",
            RsvpKind::Tentative,
            Some("maybe later?"),
            true,
            Some(("2026-07-21T14:00:00Z".into(), "2026-07-21T15:00:00Z".into())),
        )
        .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/events/E1/tentativelyAccept");
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(
            sent.get("comment").and_then(Value::as_str),
            Some("maybe later?")
        );
        assert_eq!(
            sent.get("sendResponse").and_then(Value::as_bool),
            Some(true)
        );
        let pnt = sent.get("proposedNewTime").unwrap();
        for (side, want) in [
            ("start", "2026-07-21T14:00:00"),
            ("end", "2026-07-21T15:00:00"),
        ] {
            let dt = pnt.get(side).unwrap();
            assert_eq!(dt.get("dateTime").and_then(Value::as_str), Some(want));
            assert_eq!(dt.get("timeZone").and_then(Value::as_str), Some("UTC"));
        }
    }

    #[test]
    fn respond_event_accept_drops_a_proposed_time() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/E1/accept".into(),
            status: 202,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.respond_event(
            "E1",
            RsvpKind::Accept,
            None,
            true,
            Some(("2026-07-21T14:00:00Z".into(), "2026-07-21T15:00:00Z".into())),
        )
        .unwrap();
        let sent = json::parse(&srv.requests()[0].body).unwrap();
        assert!(sent.get("proposedNewTime").is_none());
    }

    #[test]
    fn rsvp_kind_from_status() {
        assert_eq!(RsvpKind::from_status("accepted"), Some(RsvpKind::Accept));
        assert_eq!(RsvpKind::from_status("declined"), Some(RsvpKind::Decline));
        assert_eq!(
            RsvpKind::from_status("tentativelyAccepted"),
            Some(RsvpKind::Tentative)
        );
        assert_eq!(RsvpKind::from_status("none"), None);
        assert_eq!(RsvpKind::from_status(""), None);
    }

    #[test]
    fn rsvp_proposal_normalises_and_refuses() {
        let (s, e) = ("2026-07-21T14:00:00.0000000", "2026-07-21T15:00:00Z");
        let window = Some((
            "2026-07-21T14:00:00Z".to_string(),
            "2026-07-21T15:00:00Z".to_string(),
        ));
        for kind in [RsvpKind::Decline, RsvpKind::Tentative] {
            assert_eq!(
                rsvp_proposal(kind, true, Some(s), Some(e)),
                Ok(window.clone())
            );
        }
        // No proposal: nothing to check, whatever the kind or flag.
        for (start, end) in [(None, None), (Some(""), Some("")), (Some(""), None)] {
            assert_eq!(rsvp_proposal(RsvpKind::Accept, false, start, end), Ok(None));
        }
        let refused = |kind, send, start, end| rsvp_proposal(kind, send, start, end).is_err();
        assert!(refused(RsvpKind::Accept, true, Some(s), Some(e)), "accept");
        assert!(
            refused(RsvpKind::Tentative, false, Some(s), Some(e)),
            "no response"
        );
        assert!(refused(RsvpKind::Decline, true, Some(s), None), "no end");
        assert!(
            refused(RsvpKind::Decline, true, Some(""), Some(e)),
            "no start"
        );
        assert!(
            refused(RsvpKind::Decline, true, Some(e), Some(s)),
            "end before start"
        );
        assert!(
            refused(RsvpKind::Decline, true, Some(e), Some(e)),
            "empty window"
        );
    }

    #[test]
    fn respond_event_without_proposed_time_omits_the_key() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/E1/accept".into(),
            status: 202,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.respond_event("E1", RsvpKind::Accept, None, true, None)
            .unwrap();
        let sent = json::parse(&srv.requests()[0].body).unwrap();
        assert!(sent.get("proposedNewTime").is_none());
    }

    #[test]
    fn respond_event_accept_posts_comment_and_send_response() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/E1/accept".into(),
            status: 202,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.respond_event("E1", RsvpKind::Accept, Some("ok"), true, None)
            .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "POST");
        assert!(reqs[0].path.ends_with("/accept"));
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(sent.get("comment").and_then(Value::as_str), Some("ok"));
        assert_eq!(
            sent.get("sendResponse").and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn respond_event_decline_and_tentative_hit_the_right_action() {
        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/events/E1/decline".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/events/E1/tentativelyAccept".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.respond_event("E1", RsvpKind::Decline, None, false, None)
            .unwrap();
        c.respond_event("E1", RsvpKind::Tentative, None, false, None)
            .unwrap();
        let reqs = srv.requests();
        assert!(reqs[0].path.ends_with("/decline"));
        assert!(reqs[1].path.ends_with("/tentativelyAccept"));
    }

    #[test]
    fn respond_event_401_maps_to_unauthorized() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/E1/accept".into(),
            status: 401,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(
            c.respond_event("E1", RsvpKind::Accept, None, true, None),
            Err(GraphError::Unauthorized)
        ));
    }

    #[test]
    fn respond_event_id_is_percent_encoded() {
        let raw_id = "AAMk/id+with=chars";
        let encoded_id = "AAMk%2Fid%2Bwith%3Dchars";
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: format!("/me/events/{encoded_id}/accept"),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.respond_event(raw_id, RsvpKind::Accept, None, true, None)
            .unwrap();
        let reqs = srv.requests();
        assert!(reqs[0].path.contains(encoded_id));
        assert!(!reqs[0].path.contains(raw_id));
    }

    /// A route answering a meeting action as Graph does: `202`, no body.
    fn accepted(path: &str) -> Route {
        Route {
            method: "POST".into(),
            path_prefix: path.into(),
            status: 202,
            headers: vec![],
            body: "".into(),
        }
    }

    #[test]
    fn cancel_event_posts_comment() {
        let srv = FakeServer::start(vec![accepted("/me/events/E1/cancel")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.cancel_event("E1", Some("Moved to next week")).unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/events/E1/cancel");
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(
            sent,
            Value::Object(vec![(
                "comment".to_string(),
                Value::Str("Moved to next week".to_string())
            )])
        );
    }

    #[test]
    fn cancel_event_without_comment_sends_empty() {
        let srv = FakeServer::start(vec![accepted("/me/events/E1/cancel")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.cancel_event("E1", None).unwrap();
        let sent = json::parse(&srv.requests()[0].body).unwrap();
        assert_eq!(sent.get("comment").and_then(Value::as_str), Some(""));
    }

    #[test]
    fn cancel_event_id_is_percent_encoded() {
        let srv = FakeServer::start(vec![accepted("/me/events/AAMk%2Fid%2Bwith%3Dchars/cancel")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.cancel_event("AAMk/id+with=chars", None).unwrap();
        assert_eq!(
            srv.requests()[0].path,
            "/me/events/AAMk%2Fid%2Bwith%3Dchars/cancel"
        );
    }

    #[test]
    fn cancel_event_refused_is_an_error() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/E1/cancel".into(),
            status: 400,
            headers: vec![],
            body: r#"{"error":{"code":"ErrorInvalidRequest"}}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(
            c.cancel_event("E1", None),
            Err(GraphError::Http { status: 400, .. })
        ));
    }

    /// Graph's answer to `dismissReminder` / `snoozeReminder`: `200`, no body.
    fn ok_empty(path: &str) -> Route {
        Route {
            method: "POST".into(),
            path_prefix: path.into(),
            status: 200,
            headers: vec![],
            body: "".into(),
        }
    }

    #[test]
    fn dismiss_reminder_posts_no_body() {
        let srv = FakeServer::start(vec![ok_empty("/me/events/E1/dismissReminder")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.dismiss_reminder("E1").unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/events/E1/dismissReminder");
        // Without a length Exchange answers 411 (#1954).
        assert_eq!(content_length(&reqs[0]), Some("0"));
        assert_eq!(reqs[0].body, "");
    }

    #[test]
    fn snooze_reminder_posts_new_time() {
        let srv = FakeServer::start(vec![ok_empty("/me/events/E1/snoozeReminder")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.snooze_reminder("E1", "2026-11-08T10:15:00Z").unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/events/E1/snoozeReminder");
        assert_eq!(
            reqs[0].body,
            r#"{"newReminderTime":{"dateTime":"2026-11-08T10:15:00","timeZone":"UTC"}}"#
        );
    }

    #[test]
    fn reminder_ids_are_percent_encoded() {
        let srv = FakeServer::start(vec![
            ok_empty("/me/events/AAMk%2Fid%2Bwith%3Dchars/dismissReminder"),
            ok_empty("/me/events/AAMk%2Fid%2Bwith%3Dchars/snoozeReminder"),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.dismiss_reminder("AAMk/id+with=chars").unwrap();
        c.snooze_reminder("AAMk/id+with=chars", "2026-11-08T10:15:00Z")
            .unwrap();
        let paths: Vec<String> = srv.requests().into_iter().map(|r| r.path).collect();
        assert_eq!(
            paths,
            [
                "/me/events/AAMk%2Fid%2Bwith%3Dchars/dismissReminder",
                "/me/events/AAMk%2Fid%2Bwith%3Dchars/snoozeReminder",
            ]
        );
    }

    #[test]
    fn dismiss_reminder_refused_is_an_error() {
        let refused = |path: &str| Route {
            method: "POST".into(),
            path_prefix: path.into(),
            status: 400,
            headers: vec![],
            body: r#"{"error":{"code":"ErrorInvalidRequest"}}"#.into(),
        };
        let srv = FakeServer::start(vec![
            refused("/me/events/E1/dismissReminder"),
            refused("/me/events/E1/snoozeReminder"),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(
            c.dismiss_reminder("E1"),
            Err(GraphError::Http { status: 400, .. })
        ));
        assert!(matches!(
            c.snooze_reminder("E1", "2026-11-08T10:15:00Z"),
            Err(GraphError::Http { status: 400, .. })
        ));
    }

    #[test]
    fn forward_event_posts_recipients_and_comment() {
        let srv = FakeServer::start(vec![accepted("/me/events/AAMk%2Fid/forward")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let to = [
            Recipient {
                name: "Ann".into(),
                address: "ann@x".into(),
            },
            Recipient {
                name: "".into(),
                address: "bob@y".into(),
            },
        ];
        c.forward_event("AAMk/id", &to, Some("FYI")).unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/events/AAMk%2Fid/forward");
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(sent.get("comment").and_then(Value::as_str), Some("FYI"));
        let got: Vec<(&str, &str)> = sent
            .get("toRecipients")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|r| {
                let a = r.get("emailAddress").unwrap();
                (
                    a.get("name").and_then(Value::as_str).unwrap(),
                    a.get("address").and_then(Value::as_str).unwrap(),
                )
            })
            .collect();
        assert_eq!(got, [("Ann", "ann@x"), ("", "bob@y")]);
    }

    #[test]
    fn forward_event_without_comment_sends_empty() {
        let srv = FakeServer::start(vec![accepted("/me/events/E1/forward")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let to = [Recipient {
            name: "Ann".into(),
            address: "ann@x".into(),
        }];
        c.forward_event("E1", &to, None).unwrap();
        let sent = json::parse(&srv.requests()[0].body).unwrap();
        assert_eq!(sent.get("comment").and_then(Value::as_str), Some(""));
    }

    #[test]
    fn reply_forward_and_send_ids_are_percent_encoded() {
        let raw_id = "AAMk/id+with=chars";
        let encoded_id = "AAMk%2Fid%2Bwith%3Dchars";
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: format!("/me/messages/{encoded_id}/send"),
            status: 200,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.send_draft(raw_id).unwrap();
        let reqs = srv.requests();
        assert!(reqs[0].path.contains(encoded_id));
        assert!(!reqs[0].path.contains(raw_id));
    }

    #[test]
    fn people_parses_ranked_directory_results() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/people".into(),
            status: 200,
            headers: vec![],
            body: r#"{"value":[
                {"displayName":"Ann Lee","scoredEmailAddresses":[{"address":"ann@x.com","relevanceScore":9.0}]},
                {"displayName":"No Email Person","scoredEmailAddresses":[]},
                {"displayName":"Bob Jones","scoredEmailAddresses":[{"address":"bob@x.com","relevanceScore":8.0}]}
            ]}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let people = c.people().unwrap();
        // The address-less entry is skipped; rank is the position in the response.
        assert_eq!(people.len(), 2);
        assert_eq!(people[0].name, "Ann Lee");
        assert_eq!(people[0].address, "ann@x.com");
        assert_eq!(people[0].rank, 0);
        assert_eq!(people[1].address, "bob@x.com");
        assert_eq!(people[1].rank, 2); // original index preserved (the skipped entry was #1)
    }

    #[test]
    fn people_propagates_a_403_as_an_error() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/people".into(),
            status: 403,
            headers: vec![],
            body: r#"{"error":{"code":"Authorization_RequestDenied"}}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(c.people().is_err());
    }

    #[test]
    fn contacts_follows_next_links_and_keeps_every_contact() {
        // The page-2 route is listed first: the fake server answers the first
        // route whose path_prefix matches (see
        // `calendar_view_follows_next_link_pagination`).
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/contacts?$skip=200".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[
                    {"id":"C3","displayName":"Erin","emailAddresses":[{"name":"Erin","address":"Erin@X.com"}]}
                ]}"#
                .into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/contacts".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[
                    {"id":"C1","displayName":"Dave Doe","emailAddresses":[{"name":"Dave","address":"dave@x.com"},{"name":"Dave home","address":"dave@home.example"}]},
                    {"id":"C2","displayName":"No Address","emailAddresses":[]},
                    {"displayName":"No id"}
                ],"@odata.nextLink":"/me/contacts?$skip=200"}"#
                .into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let got = c.contacts().unwrap();
        let ids: Vec<&str> = got.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["C1", "C2", "C3"], "an item without an id is skipped");
        let addresses: Vec<&str> = got[0]
            .fields
            .email_addresses
            .iter()
            .map(|e| e.address.as_str())
            .collect();
        assert_eq!(addresses, ["dave@x.com", "dave@home.example"]);
        assert_eq!(got[0].fields.email_addresses[1].name, "Dave home");
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 2);
        assert!(
            reqs[0].path.starts_with("/me/contacts?"),
            "{}",
            reqs[0].path
        );
        assert!(reqs[0].path.contains("$top=200"), "{}", reqs[0].path);
        assert!(
            reqs[0].path.contains(&format!("$select={CONTACT_SELECT}")),
            "{}",
            reqs[0].path
        );
        assert_eq!(reqs[1].path, "/me/contacts?$skip=200");
    }

    #[test]
    fn contacts_parses_every_record_field() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/contacts".into(),
            status: 200,
            headers: vec![],
            body: r#"{"value":[
                {"id":"C1","changeKey":"CK1","givenName":"Ann","middleName":"B","surname":"Cole",
                 "displayName":"Ann Cole","fileAs":"Cole, Ann",
                 "emailAddresses":[{"name":"Ann","address":"ann@x.com"},{"address":"ann@home.example"}],
                 "businessPhones":["+1 555 0100","+1 555 0101"],"homePhones":["+1 555 0200"],
                 "mobilePhone":"+1 555 0300","companyName":"Contoso","jobTitle":"Engineer",
                 "department":"R&D",
                 "businessAddress":{"street":"1 Main St","city":"Springfield","state":"IL","postalCode":"62701","countryOrRegion":"United States"},
                 "homeAddress":{"street":"2 Elm St","city":"Shelbyville"},
                 "birthday":"1980-05-17T11:59:00Z","personalNotes":"likes tea",
                 "categories":["Friends","Work"]},
                {"id":"C2","changeKey":null,"givenName":null,"emailAddresses":null,
                 "businessPhones":null,"businessAddress":null,"homeAddress":{},
                 "birthday":null},
                {"id":"C3","birthday":"not a date"}
            ]}"#
            .into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let got = c.contacts().unwrap();
        let ann = &got[0];
        assert_eq!(ann.id, "C1");
        assert_eq!(ann.change_key, "CK1");
        let f = &ann.fields;
        assert_eq!(
            (
                f.given_name.as_str(),
                f.middle_name.as_str(),
                f.surname.as_str()
            ),
            ("Ann", "B", "Cole")
        );
        assert_eq!(f.display_name, "Ann Cole");
        assert_eq!(f.file_as, "Cole, Ann");
        assert_eq!(
            f.email_addresses,
            vec![
                Recipient {
                    name: "Ann".into(),
                    address: "ann@x.com".into()
                },
                Recipient {
                    name: "".into(),
                    address: "ann@home.example".into()
                },
            ]
        );
        assert_eq!(f.business_phones, ["+1 555 0100", "+1 555 0101"]);
        assert_eq!(f.home_phones, ["+1 555 0200"]);
        assert_eq!(f.mobile_phone, "+1 555 0300");
        assert_eq!(f.company_name, "Contoso");
        assert_eq!(f.job_title, "Engineer");
        assert_eq!(f.department, "R&D");
        assert_eq!(
            f.business_address,
            PhysicalAddress {
                street: "1 Main St".into(),
                city: "Springfield".into(),
                state: "IL".into(),
                postal_code: "62701".into(),
                country_or_region: "United States".into(),
            }
        );
        assert_eq!(f.home_address.street, "2 Elm St");
        assert_eq!(f.home_address.city, "Shelbyville");
        assert_eq!(f.home_address.postal_code, "");
        assert_eq!(f.birthday, "1980-05-17", "the date part only");
        assert_eq!(f.personal_notes, "likes tea");
        assert_eq!(f.categories, ["Friends", "Work"]);
        // Null and missing read as empty.
        assert_eq!(
            got[1],
            FolderContact {
                id: "C2".into(),
                ..FolderContact::default()
            }
        );
        assert_eq!(got[2].fields.birthday, "", "not a date");
    }

    #[test]
    fn contact_gets_one_record_and_404_is_not_found() {
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/contacts/gone".into(),
                status: 404,
                headers: vec![],
                body: "{}".into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/contacts/C%2F1".into(),
                status: 200,
                headers: vec![],
                body: r#"{"id":"C/1","changeKey":"CK","displayName":"Ann"}"#.into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let got = c.contact("C/1").unwrap();
        assert_eq!(got.id, "C/1");
        assert_eq!(got.fields.display_name, "Ann");
        assert!(matches!(c.contact("gone"), Err(GraphError::NotFound)));
        let reqs = srv.requests();
        assert!(
            reqs[0]
                .path
                .starts_with(&format!("/me/contacts/C%2F1?$select={CONTACT_SELECT}")),
            "{}",
            reqs[0].path
        );
    }

    fn sample_contact_fields() -> ContactFields {
        ContactFields {
            given_name: "Ann".into(),
            surname: "Cole".into(),
            email_addresses: vec![
                Recipient {
                    name: "Ann".into(),
                    address: " ann@x.com ".into(),
                },
                Recipient {
                    name: "Blank".into(),
                    address: "  ".into(),
                },
            ],
            business_phones: vec!["+1 555 0100".into()],
            home_address: PhysicalAddress {
                city: "Shelbyville".into(),
                ..PhysicalAddress::default()
            },
            birthday: "1980-05-17".into(),
            categories: vec!["Friends".into()],
            ..ContactFields::default()
        }
    }

    #[test]
    fn create_contact_posts_the_nonempty_fields_and_parses_the_answer() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/contacts".into(),
            status: 201,
            headers: vec![],
            body: r#"{"id":"G1","changeKey":"CK1","givenName":"Ann","surname":"Cole",
                      "displayName":"Ann Cole","fileAs":"Cole, Ann"}"#
                .into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let created = c.create_contact(&sample_contact_fields()).unwrap();
        assert_eq!(created.id, "G1");
        assert_eq!(created.change_key, "CK1");
        assert_eq!(created.fields.display_name, "Ann Cole");
        assert_eq!(created.fields.file_as, "Cole, Ann");
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/contacts");
        let sent = json::parse(&reqs[0].body).unwrap();
        let Value::Object(entries) = &sent else {
            panic!("{}", reqs[0].body)
        };
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "givenName",
                "surname",
                "emailAddresses",
                "businessPhones",
                "homeAddress",
                "birthday",
                "categories"
            ],
            "only the non-empty properties"
        );
        let emails = sent
            .get("emailAddresses")
            .and_then(Value::as_array)
            .unwrap();
        assert_eq!(emails.len(), 1, "the blank address is dropped");
        assert_eq!(
            emails[0].get("address").and_then(Value::as_str),
            Some("ann@x.com")
        );
        assert_eq!(emails[0].get("name").and_then(Value::as_str), Some("Ann"));
        let home = sent.get("homeAddress").unwrap();
        assert_eq!(
            home.get("city").and_then(Value::as_str),
            Some("Shelbyville")
        );
        assert_eq!(home.get("street").and_then(Value::as_str), Some(""));
        assert_eq!(
            sent.get("birthday").and_then(Value::as_str),
            Some("1980-05-17T12:00:00Z"),
            "noon UTC of the day"
        );
    }

    #[test]
    fn update_contact_patches_only_the_given_fields() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/contacts/G%2F1".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"G/1"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let mut f = sample_contact_fields();
        f.birthday.clear();
        f.job_title = "".into();
        let mut set = ContactFieldSet::BIRTHDAY;
        set.insert(ContactFieldSet::JOB_TITLE);
        c.update_contact("G/1", &f, set).unwrap();
        // An empty set sends nothing.
        c.update_contact("G/1", &f, ContactFieldSet::default())
            .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(reqs[0].path, "/me/contacts/G%2F1");
        assert_eq!(reqs[0].body, r#"{"jobTitle":"","birthday":null}"#);
    }

    #[test]
    fn delete_contact_404_is_not_found() {
        let srv = FakeServer::start_limited(vec![
            (
                Route {
                    method: "DELETE".into(),
                    path_prefix: "/me/contacts/G1".into(),
                    status: 204,
                    headers: vec![],
                    body: "".into(),
                },
                Some(1),
            ),
            (
                Route {
                    method: "DELETE".into(),
                    path_prefix: "/me/contacts/G1".into(),
                    status: 404,
                    headers: vec![],
                    body: "{}".into(),
                },
                None,
            ),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.delete_contact("G1").unwrap();
        assert!(matches!(c.delete_contact("G1"), Err(GraphError::NotFound)));
        assert_eq!(srv.requests()[0].method, "DELETE");
    }

    #[test]
    fn contact_403_on_write_is_http_403() {
        let denied = |method: &str| Route {
            method: method.into(),
            path_prefix: "/me/contacts".into(),
            status: 403,
            headers: vec![],
            body: r#"{"error":{"code":"ErrorAccessDenied","message":"Access is denied."}}"#.into(),
        };
        let srv = FakeServer::start(vec![denied("POST"), denied("PATCH"), denied("DELETE")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let f = sample_contact_fields();
        let is_403 =
            |r: Result<(), GraphError>| matches!(r, Err(GraphError::Http { status: 403, .. }));
        assert!(is_403(c.create_contact(&f).map(|_| ())));
        assert!(is_403(c.update_contact("G1", &f, ContactFieldSet::SURNAME)));
        assert!(is_403(c.delete_contact("G1")));
        let err = c.delete_contact("G1").unwrap_err().to_string();
        assert_eq!(err, "HTTP 403: Access is denied.");
    }

    #[test]
    fn contact_field_set_names_round_trip() {
        let names = ContactFieldSet::ALL.to_names();
        assert_eq!(names.len(), 17);
        assert_eq!(ContactFieldSet::from_names(&names), ContactFieldSet::ALL);
        let mut set = ContactFieldSet::SURNAME;
        set.insert(ContactFieldSet::HOME_ADDRESS);
        assert_eq!(set.to_names(), ["surname", "homeAddress"]);
        assert_eq!(
            ContactFieldSet::from_names(&["surname", "nickName", "homeAddress"]),
            set,
            "an unknown name is dropped"
        );
        // The names are Graph's, which `contact_body_json` sends.
        let body = contact_body_json(&ContactFields::default(), ContactFieldSet::ALL);
        let v = json::parse(&body).unwrap();
        for n in names {
            assert!(v.get(n).is_some(), "{n} in {body}");
        }
    }

    #[test]
    fn contact_field_set_changed_and_non_empty() {
        let a = sample_contact_fields();
        assert_eq!(ContactFieldSet::changed(&a, &a), ContactFieldSet::default());
        let mut b = a.clone();
        b.home_address.state = "IL".into();
        b.categories.clear();
        let mut want = ContactFieldSet::HOME_ADDRESS;
        want.insert(ContactFieldSet::CATEGORIES);
        assert_eq!(ContactFieldSet::changed(&a, &b), want);
        assert_eq!(
            ContactFieldSet::non_empty(&ContactFields::default()),
            ContactFieldSet::default()
        );
        // An address list of blanks only is empty.
        let blanks = ContactFields {
            email_addresses: vec![Recipient {
                name: "x".into(),
                address: " ".into(),
            }],
            ..ContactFields::default()
        };
        assert!(ContactFieldSet::non_empty(&blanks).is_empty());
    }

    #[test]
    fn contacts_propagates_a_403() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/contacts".into(),
            status: 403,
            headers: vec![],
            body: r#"{"error":{"code":"ErrorAccessDenied"}}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(c.contacts().is_err());
    }

    #[test]
    fn contacts_later_page_error_is_an_error() {
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/contacts?$skip=200".into(),
                status: 500,
                headers: vec![],
                body: "{}".into(),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/contacts".into(),
                status: 200,
                headers: vec![],
                body: r#"{"value":[{"displayName":"Dave","emailAddresses":[{"address":"dave@x.com"}]}],"@odata.nextLink":"/me/contacts?$skip=200"}"#.into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(c.contacts().is_err());
    }

    #[test]
    fn contacts_page_without_value_is_an_error() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/contacts".into(),
            status: 200,
            headers: vec![],
            body: r#"{"error":"nope"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(c.contacts().is_err());
    }

    #[test]
    fn add_file_attachment_posts_a_file_attachment_with_base64_bytes() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/attachments".into(),
            status: 201,
            headers: vec![],
            body: r#"{"id":"att1"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.add_file_attachment("M1", "hello.txt", "text/plain", b"hello")
            .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "POST");
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(
            sent.get("@odata.type").and_then(Value::as_str),
            Some("#microsoft.graph.fileAttachment")
        );
        assert_eq!(sent.get("name").and_then(Value::as_str), Some("hello.txt"));
        assert_eq!(
            sent.get("contentType").and_then(Value::as_str),
            Some("text/plain")
        );
        assert_eq!(
            sent.get("contentBytes").and_then(Value::as_str),
            Some("aGVsbG8=") // base64("hello")
        );
    }

    #[test]
    fn chunk_ranges_covers_total_in_chunk_sized_pieces() {
        assert_eq!(chunk_ranges(0, 0, 100), Vec::<(usize, usize)>::new());
        assert_eq!(chunk_ranges(0, 100, 100), vec![(0, 100)]);
        assert_eq!(
            chunk_ranges(0, 250, 100),
            vec![(0, 100), (100, 200), (200, 250)]
        );
        // A payload just over INLINE_MAX still fits inside one CHUNK_SIZE chunk
        // (CHUNK_SIZE is much bigger than INLINE_MAX), matching the "big"
        // payload used in `add_attachment_routes_small_inline_and_large_to_upload_session`.
        let total = INLINE_MAX + 100;
        assert_eq!(chunk_ranges(0, total, CHUNK_SIZE), vec![(0, total)]);
        // Multiple full CHUNK_SIZE-sized chunks plus a smaller remainder.
        let total2 = CHUNK_SIZE * 2 + 500;
        let ranges = chunk_ranges(0, total2, CHUNK_SIZE);
        assert_eq!(
            ranges,
            vec![
                (0, CHUNK_SIZE),
                (CHUNK_SIZE, CHUNK_SIZE * 2),
                (CHUNK_SIZE * 2, total2)
            ]
        );
        let sum: usize = ranges.iter().map(|(s, e)| e - s).sum();
        assert_eq!(sum, total2);
    }

    #[test]
    fn add_attachment_routes_small_inline_and_large_to_upload_session() {
        // small: routes to inline POST /attachments
        let small = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/attachments".into(),
            status: 201,
            headers: vec![],
            body: r#"{"id":"a"}"#.into(),
        }]);
        let c = GraphClient::new(&small.base_url, "T");
        c.add_attachment("M1", "s.txt", "text/plain", b"tiny")
            .unwrap();
        assert!(
            small
                .requests()
                .iter()
                .any(|r| r.path.contains("/attachments")
                    && !r.path.contains("createUploadSession"))
        );

        // large: createUploadSession -> chunked PUT(s) to the returned uploadUrl.
        //
        // Two separate FakeServers, not one: the createUploadSession response
        // body must embed the uploadUrl's base, but a FakeServer's base_url
        // (its ephemeral port) is only known once it's already started with
        // its full route list — so the upload target is started first (its
        // base_url is then a known value), and that value is baked into the
        // *other* server's createUploadSession route.
        let upload_srv = FakeServer::start(vec![Route {
            method: "PUT".into(),
            path_prefix: "/upload/xyz".into(),
            status: 201,
            headers: vec![],
            body: "".into(),
        }]);
        let session_body = format!(r#"{{"uploadUrl":"{}/upload/xyz"}}"#, upload_srv.base_url);
        let session_srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/attachments/createUploadSession".into(),
            status: 200,
            headers: vec![],
            body: session_body,
        }]);
        let c2 = GraphClient::new(&session_srv.base_url, "T");
        let big = vec![7u8; 3 * 1024 * 1024 + 100]; // just over INLINE_MAX -> one upload session, >=1 chunk
        c2.add_attachment("M1", "big.bin", "application/octet-stream", &big)
            .unwrap();

        let session_reqs = session_srv.requests();
        assert_eq!(
            session_reqs.len(),
            1,
            "exactly one createUploadSession POST"
        );
        assert!(session_reqs[0].path.contains("createUploadSession"));

        let put_reqs = upload_srv.requests();
        assert!(
            !put_reqs.is_empty(),
            "at least one PUT to the upload session"
        );
        let mut covered = 0usize;
        for r in &put_reqs {
            assert_eq!(r.method, "PUT");
            let content_range = r
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-range"))
                .map(|(_, v)| v.clone())
                .expect("PUT carries a Content-Range header");
            let rest = content_range
                .strip_prefix("bytes ")
                .expect("Content-Range starts with 'bytes '");
            let (range, total_str) = rest.split_once('/').expect("Content-Range has a total");
            let (start_str, end_str) = range.split_once('-').expect("Content-Range has a range");
            let start: usize = start_str.parse().expect("range start is a number");
            let end_incl: usize = end_str.parse().expect("range end is a number");
            let total: usize = total_str.parse().expect("total is a number");
            assert_eq!(total, big.len());
            assert_eq!(start, covered, "chunks are contiguous, no gaps/overlaps");
            assert_eq!(
                r.body.len(),
                end_incl - start + 1,
                "body matches Content-Range span"
            );
            covered = end_incl + 1;
        }
        assert_eq!(covered, big.len(), "PUT(s) cover the whole payload");
    }

    #[test]
    fn add_file_attachment_with_a_content_id_posts_is_inline_and_content_id() {
        // #1949: a small inline picture is a fileAttachment with both fields.
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/attachments".into(),
            status: 201,
            headers: vec![],
            body: r#"{"id":"att1"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.add_attachment_with("M1", "pic.png", "image/png", b"hello", Some("pic1"))
            .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].path, "/me/messages/M1/attachments");
        let sent = json::parse(&reqs[0].body).unwrap();
        assert_eq!(
            sent.get("@odata.type").and_then(Value::as_str),
            Some("#microsoft.graph.fileAttachment")
        );
        assert_eq!(sent.get("isInline").and_then(Value::as_bool), Some(true));
        assert_eq!(sent.get("contentId").and_then(Value::as_str), Some("pic1"));
        assert_eq!(
            sent.get("contentType").and_then(Value::as_str),
            Some("image/png")
        );
        assert_eq!(
            sent.get("contentBytes").and_then(Value::as_str),
            Some("aGVsbG8=")
        );
    }

    #[test]
    fn add_file_attachment_without_a_content_id_sends_neither_field() {
        // #1949: a plain attachment's body is today's: no isInline, no contentId.
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/attachments".into(),
            status: 201,
            headers: vec![],
            body: r#"{"id":"att1"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.add_attachment_with("M1", "a.txt", "text/plain", b"hello", None)
            .unwrap();
        c.add_attachment("M1", "a.txt", "text/plain", b"hello")
            .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 2);
        for r in &reqs {
            let sent = json::parse(&r.body).unwrap();
            assert!(sent.get("isInline").is_none(), "{}", r.body);
            assert!(sent.get("contentId").is_none(), "{}", r.body);
        }
        assert_eq!(reqs[0].body, reqs[1].body, "None is add_attachment");
    }

    #[test]
    fn upload_session_with_a_content_id_sends_is_inline_and_content_id_in_the_attachment_item() {
        // #1949: a large inline file goes through an upload session whose
        // AttachmentItem carries both fields; without one, neither.
        let upload_srv = FakeServer::start(vec![Route {
            method: "PUT".into(),
            path_prefix: "/upload/xyz".into(),
            status: 201,
            headers: vec![],
            body: "".into(),
        }]);
        let session_body = format!(r#"{{"uploadUrl":"{}/upload/xyz"}}"#, upload_srv.base_url);
        let session_srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/attachments/createUploadSession".into(),
            status: 200,
            headers: vec![],
            body: session_body,
        }]);
        let c = GraphClient::new(&session_srv.base_url, "T");
        let big = vec![7u8; INLINE_MAX + 100];
        c.add_attachment_with("M1", "big.png", "image/png", &big, Some("pic1"))
            .unwrap();
        c.add_attachment_with("M1", "big.bin", "application/octet-stream", &big, None)
            .unwrap();

        let reqs = session_srv.requests();
        assert_eq!(reqs.len(), 2);
        let item = |i: usize| {
            json::parse(&reqs[i].body)
                .unwrap()
                .get("AttachmentItem")
                .cloned()
                .expect("an AttachmentItem")
        };
        let inline = item(0);
        assert_eq!(inline.get("isInline").and_then(Value::as_bool), Some(true));
        assert_eq!(
            inline.get("contentId").and_then(Value::as_str),
            Some("pic1")
        );
        assert_eq!(
            inline.get("attachmentType").and_then(Value::as_str),
            Some("file")
        );
        assert_eq!(
            inline.get("contentType").and_then(Value::as_str),
            Some("image/png")
        );
        let plain = item(1);
        assert!(plain.get("isInline").is_none());
        assert!(plain.get("contentId").is_none());
        assert!(!upload_srv.requests().is_empty(), "the bytes were PUT");
    }

    /// What an upload told its observer (#1941).
    #[derive(Default)]
    struct Recorder {
        sessions: Vec<Option<String>>,
        progress: Vec<(u64, u64)>,
    }

    impl UploadObserver for Recorder {
        fn session(&mut self, upload_url: Option<&str>) {
            self.sessions.push(upload_url.map(str::to_string));
        }
        fn progress(&mut self, sent: u64, total: u64) {
            self.progress.push((sent, total));
        }
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

    /// A Graph server whose createUploadSession names `upload_url`.
    fn session_server(upload_url: &str) -> FakeServer {
        FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/attachments/createUploadSession".into(),
            status: 200,
            headers: vec![],
            body: format!(r#"{{"uploadUrl":"{upload_url}"}}"#),
        }])
    }

    /// The `(path, Content-Range)` of each PUT `srv` received.
    fn put_ranges(srv: &FakeServer) -> Vec<(String, String)> {
        srv.requests()
            .iter()
            .filter(|r| r.method == "PUT")
            .map(|r| {
                let range = r
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-range"))
                    .map(|(_, v)| v.clone())
                    .expect("a Content-Range");
                (r.path.clone(), range)
            })
            .collect()
    }

    /// Two full chunks and a short one.
    const BIG: usize = CHUNK_SIZE * 2 + 500;

    fn upload_big(
        c: &GraphClient,
        resume_url: Option<&str>,
        obs: &mut Recorder,
    ) -> Result<(), GraphError> {
        let big = vec![b'x'; BIG];
        c.upload_attachment(
            "M1",
            "big.bin",
            "application/octet-stream",
            &big,
            None,
            resume_url,
            obs,
        )
    }

    #[test]
    fn upload_attachment_reports_progress_after_each_chunk() {
        let upload_srv = FakeServer::start(vec![put_route("/upload/s1", 202)]);
        let url = format!("{}/upload/s1", upload_srv.base_url);
        let session_srv = session_server(&url);
        let c = GraphClient::new(&session_srv.base_url, "T");
        let mut obs = Recorder::default();
        upload_big(&c, None, &mut obs).unwrap();

        assert_eq!(obs.sessions, vec![Some(url)]);
        let (c1, c2, t) = (CHUNK_SIZE as u64, 2 * CHUNK_SIZE as u64, BIG as u64);
        assert_eq!(obs.progress, vec![(c1, t), (c2, t), (t, t)]);
        assert_eq!(upload_srv.requests().len(), 3, "no GET of a new session");
    }

    #[test]
    fn a_small_attachment_reports_one_progress_event() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/messages/M1/attachments".into(),
            status: 201,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "T");
        let mut obs = Recorder::default();
        c.upload_attachment(
            "M1",
            "a.txt",
            "text/plain",
            b"hello",
            None,
            Some("http://127.0.0.1:1/never"),
            &mut obs,
        )
        .unwrap();
        assert_eq!(obs.progress, vec![(5, 5)]);
        assert!(obs.sessions.is_empty());
        assert_eq!(srv.requests().len(), 1, "one POST, no session");
    }

    #[test]
    fn upload_resumes_a_session_from_next_expected_ranges() {
        let start = CHUNK_SIZE;
        let upload_srv = FakeServer::start(vec![
            get_route(
                "/upload/s1",
                200,
                &format!(
                    r#"{{"expirationDateTime":"2026-11-10T00:00:00Z","nextExpectedRanges":["{start}-"]}}"#
                ),
            ),
            put_route("/upload/s1", 202),
        ]);
        let url = format!("{}/upload/s1", upload_srv.base_url);
        let session_srv = session_server("http://127.0.0.1:1/unused");
        let c = GraphClient::new(&session_srv.base_url, "T");
        let mut obs = Recorder::default();
        upload_big(&c, Some(&url), &mut obs).unwrap();

        assert!(session_srv.requests().is_empty(), "no new session");
        assert!(obs.sessions.is_empty(), "the stored session stays");
        let gets: Vec<_> = upload_srv
            .requests()
            .into_iter()
            .filter(|r| r.method == "GET")
            .collect();
        assert_eq!(gets.len(), 1);
        assert!(
            !gets[0]
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("authorization")),
            "the session GET carries no bearer"
        );
        let t = BIG as u64;
        assert_eq!(
            put_ranges(&upload_srv),
            vec![
                (
                    "/upload/s1".to_string(),
                    format!("bytes {}-{}/{BIG}", CHUNK_SIZE, 2 * CHUNK_SIZE - 1)
                ),
                (
                    "/upload/s1".to_string(),
                    format!("bytes {}-{}/{BIG}", 2 * CHUNK_SIZE, BIG - 1)
                ),
            ]
        );
        let (c1, c2) = (CHUNK_SIZE as u64, 2 * CHUNK_SIZE as u64);
        assert_eq!(
            obs.progress,
            vec![(c1, t), (c2, t), (t, t)],
            "the resume reports where it goes on from first"
        );
    }

    #[test]
    fn upload_with_an_expired_session_opens_a_new_one_from_byte_zero() {
        let upload_srv = FakeServer::start(vec![
            get_route("/upload/old", 404, ""),
            put_route("/upload/new", 202),
        ]);
        let old = format!("{}/upload/old", upload_srv.base_url);
        let new = format!("{}/upload/new", upload_srv.base_url);
        let session_srv = session_server(&new);
        let c = GraphClient::new(&session_srv.base_url, "T");
        let mut obs = Recorder::default();
        upload_big(&c, Some(&old), &mut obs).unwrap();

        assert_eq!(session_srv.requests().len(), 1, "one new session");
        assert_eq!(obs.sessions, vec![Some(new)]);
        let ranges = put_ranges(&upload_srv);
        assert_eq!(ranges.len(), 3);
        assert!(ranges.iter().all(|(p, _)| p == "/upload/new"));
        assert_eq!(ranges[0].1, format!("bytes 0-{}/{BIG}", CHUNK_SIZE - 1));
        assert_eq!(obs.progress[0], (CHUNK_SIZE as u64, BIG as u64));
    }

    #[test]
    fn a_session_get_with_no_usable_range_opens_a_new_session() {
        // Each answer is no place to go on from: no ranges, a start past the
        // end, a range that is not a number, a body that is not JSON.
        for body in [
            r#"{"nextExpectedRanges":[]}"#.to_string(),
            format!(r#"{{"nextExpectedRanges":["{BIG}-"]}}"#),
            r#"{"nextExpectedRanges":["abc-"]}"#.to_string(),
            r#"{}"#.to_string(),
            "not json".to_string(),
        ] {
            let upload_srv = FakeServer::start(vec![
                get_route("/upload/old", 200, &body),
                put_route("/upload/new", 202),
            ]);
            let old = format!("{}/upload/old", upload_srv.base_url);
            let new = format!("{}/upload/new", upload_srv.base_url);
            let session_srv = session_server(&new);
            let c = GraphClient::new(&session_srv.base_url, "T");
            let mut obs = Recorder::default();
            upload_big(&c, Some(&old), &mut obs).unwrap();
            assert_eq!(session_srv.requests().len(), 1, "{body}");
            assert_eq!(obs.sessions, vec![Some(new)], "{body}");
            assert_eq!(
                put_ranges(&upload_srv)[0].1,
                format!("bytes 0-{}/{BIG}", CHUNK_SIZE - 1),
                "{body}"
            );
        }
    }

    #[test]
    fn a_session_get_resumes_from_the_lowest_range_start() {
        let upload_srv = FakeServer::start(vec![
            get_route(
                "/upload/s1",
                200,
                &format!(
                    r#"{{"nextExpectedRanges":["{}-{}","{}-"]}}"#,
                    2 * CHUNK_SIZE,
                    BIG - 1,
                    CHUNK_SIZE
                ),
            ),
            put_route("/upload/s1", 202),
        ]);
        let url = format!("{}/upload/s1", upload_srv.base_url);
        let c = GraphClient::new("http://127.0.0.1:1", "T");
        let mut obs = Recorder::default();
        upload_big(&c, Some(&url), &mut obs).unwrap();
        assert_eq!(
            put_ranges(&upload_srv)[0].1,
            format!("bytes {}-{}/{BIG}", CHUNK_SIZE, 2 * CHUNK_SIZE - 1)
        );
    }

    #[test]
    fn a_refused_resumed_put_falls_back_to_a_new_session() {
        let upload_srv = FakeServer::start(vec![
            get_route(
                "/upload/old",
                200,
                &format!(r#"{{"nextExpectedRanges":["{}-"]}}"#, 2 * CHUNK_SIZE),
            ),
            put_route("/upload/old", 416),
            put_route("/upload/new", 202),
        ]);
        let old = format!("{}/upload/old", upload_srv.base_url);
        let new = format!("{}/upload/new", upload_srv.base_url);
        let session_srv = session_server(&new);
        let c = GraphClient::new(&session_srv.base_url, "T");
        let mut obs = Recorder::default();
        upload_big(&c, Some(&old), &mut obs).unwrap();

        assert_eq!(obs.sessions, vec![None, Some(new)], "forgotten, replaced");
        assert_eq!(session_srv.requests().len(), 1);
        let ranges = put_ranges(&upload_srv);
        assert_eq!(ranges.len(), 4, "one refused, then three from byte 0");
        assert_eq!(ranges[0].0, "/upload/old");
        assert_eq!(ranges[1].0, "/upload/new");
        assert_eq!(ranges[1].1, format!("bytes 0-{}/{BIG}", CHUNK_SIZE - 1));
        let t = BIG as u64;
        assert_eq!(
            obs.progress,
            vec![
                (2 * CHUNK_SIZE as u64, t),
                (CHUNK_SIZE as u64, t),
                (2 * CHUNK_SIZE as u64, t),
                (t, t)
            ],
            "the resume's report, then the new session's chunks: `sent` goes down"
        );
    }

    #[test]
    fn a_refused_put_of_a_new_session_forgets_it_and_fails() {
        let upload_srv = FakeServer::start(vec![put_route("/upload/s1", 400)]);
        let url = format!("{}/upload/s1", upload_srv.base_url);
        let session_srv = session_server(&url);
        let c = GraphClient::new(&session_srv.base_url, "T");
        let mut obs = Recorder::default();
        let err = upload_big(&c, None, &mut obs).unwrap_err();
        assert!(
            matches!(err, GraphError::Http { status: 400, .. }),
            "{err:?}"
        );
        assert_eq!(obs.sessions, vec![Some(url), None]);
        assert_eq!(session_srv.requests().len(), 1, "no second session");
    }

    #[test]
    fn a_401_or_404_from_an_upload_session_put_is_not_unauthorized() {
        for status in [401u16, 404] {
            let upload_srv = FakeServer::start(vec![put_route("/upload/s1", status)]);
            let url = format!("{}/upload/s1", upload_srv.base_url);
            let session_srv = session_server(&url);
            let c = GraphClient::new(&session_srv.base_url, "T");
            let mut obs = Recorder::default();
            let err = upload_big(&c, None, &mut obs).unwrap_err();
            assert!(
                matches!(&err, GraphError::Http { status: s, .. } if *s == status),
                "{status}: {err:?}"
            );
            assert_eq!(obs.sessions, vec![Some(url), None], "{status}");
        }
    }

    #[test]
    fn a_5xx_or_throttled_put_keeps_the_session() {
        for status in [500u16, 503, 429] {
            let upload_srv = FakeServer::start_limited(vec![
                (put_route("/upload/s1", 202), Some(1)),
                (put_route("/upload/s1", status), None),
            ]);
            let url = format!("{}/upload/s1", upload_srv.base_url);
            let session_srv = session_server(&url);
            let c = GraphClient::new(&session_srv.base_url, "T");
            let mut obs = Recorder::default();
            let err = upload_big(&c, None, &mut obs).unwrap_err();
            assert!(
                matches!(
                    err,
                    GraphError::Http { status: 500, .. } | GraphError::Throttled { .. }
                ),
                "{status}: {err:?}"
            );
            assert_eq!(obs.sessions, vec![Some(url)], "{status}: kept");
            assert_eq!(
                obs.progress,
                vec![(CHUNK_SIZE as u64, BIG as u64)],
                "{status}"
            );
        }
    }

    fn sample_input() -> EventInput {
        EventInput {
            subject: "Sync".into(),
            start_utc: "2026-07-20T11:00:00Z".into(),
            end_utc: "2026-07-20T12:00:00Z".into(),
            is_all_day: false,
            location: "Room 1".into(),
            attendees: vec![AttendeeInput {
                name: "Bob".into(),
                addr: "bob@x.com".into(),
                r#type: String::new(),
            }],
            body_html: "<p>agenda</p>".into(),
            recurrence: None,
            show_as: "busy".into(),
            sensitivity: "normal".into(),
            importance: "normal".into(),
            response_requested: true,
            allow_new_time_proposals: true,
            fields: EventFieldSet::ALL,
            ..EventInput::default()
        }
    }

    #[test]
    fn event_body_includes_recurrence_when_present() {
        use crate::graph::model::{Recurrence, RecurrenceKind};
        let input = EventInput {
            subject: "Standup".into(),
            start_utc: "2026-07-20T09:00:00Z".into(),
            end_utc: "2026-07-20T09:15:00Z".into(),
            is_all_day: false,
            location: "".into(),
            attendees: vec![],
            body_html: "".into(),
            recurrence: Some(Recurrence {
                kind: RecurrenceKind::Daily,
                interval: 1,
                days_of_week: vec![],
                day_of_month: 0,
                start_date: "2026-07-20".into(),
                until: None,
            }),
            fields: EventFieldSet::ALL,
            ..EventInput::default()
        };
        let body = json::parse(&event_body_json(&input)).unwrap();
        assert!(body.get("recurrence").is_some());
        assert_eq!(
            body.get("recurrence")
                .unwrap()
                .get("pattern")
                .unwrap()
                .get("type")
                .and_then(Value::as_str),
            Some("daily")
        );

        let mut plain = input.clone();
        plain.recurrence = None;
        let body = json::parse(&event_body_json(&plain)).unwrap();
        assert!(body.get("recurrence").is_none());
    }

    #[test]
    fn create_event_in_posts_to_the_calendars_events() {
        // #2048: the calendar id is encoded as `calendarView`'s is, and the
        // body is `create_event`'s.
        let answer = r#"{"id":"EV1","subject":"Sync"}"#;
        let srv = FakeServer::start(vec![
            Route {
                method: "POST".into(),
                path_prefix: "/me/calendars/A%2Fb%2B%3D/events".into(),
                status: 201,
                headers: vec![],
                body: answer.into(),
            },
            Route {
                method: "POST".into(),
                path_prefix: "/me/events".into(),
                status: 201,
                headers: vec![],
                body: answer.into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "T");
        let ev = c.create_event_in("A/b+=", &sample_input()).unwrap();
        assert_eq!(ev.id, "EV1");
        assert_eq!(ev.calendar_id, "A/b+=", "the URL decides the calendar");
        let default = c.create_event(&sample_input()).unwrap();
        assert_eq!(default.calendar_id, "");
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "POST");
        assert_eq!(reqs[0].path, "/me/calendars/A%2Fb%2B%3D/events");
        assert_eq!(reqs[1].path, "/me/events");
        assert_eq!(reqs[0].body, reqs[1].body);
        assert_eq!(reqs[0].body, event_body_json(&sample_input()));
    }

    #[test]
    fn create_event_posts_expected_body() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(), path_prefix: "/me/events".into(), status: 201, headers: vec![],
            body: r#"{"id":"EV1","subject":"Sync","start":{"dateTime":"2026-07-20T11:00:00.0000000","timeZone":"UTC"},"end":{"dateTime":"2026-07-20T12:00:00.0000000","timeZone":"UTC"},"isAllDay":false,"location":{"displayName":"Room 1"},"organizer":{"emailAddress":{"name":"Me","address":"me@x"}},"responseStatus":{"response":"organizer"},"attendees":[],"bodyPreview":"agenda","webLink":"","lastModifiedDateTime":""}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "T");
        let ev = c.create_event(&sample_input()).unwrap();
        assert_eq!(ev.id, "EV1");
        let body = srv.requests()[0].body.clone();
        let sent = json::parse(&body).unwrap();
        assert_eq!(sent.get("subject").and_then(Value::as_str), Some("Sync"));
        // start/end are {dateTime (no Z), timeZone: "UTC"}
        let start = sent.get("start").unwrap();
        assert_eq!(
            start.get("dateTime").and_then(Value::as_str),
            Some("2026-07-20T11:00:00")
        );
        assert_eq!(start.get("timeZone").and_then(Value::as_str), Some("UTC"));
        assert_eq!(sent.get("isAllDay").and_then(Value::as_bool), Some(false));
        assert_eq!(
            sent.get("location")
                .and_then(|l| l.get("displayName"))
                .and_then(Value::as_str),
            Some("Room 1")
        );
        let att = sent.get("attendees").and_then(Value::as_array).unwrap();
        assert_eq!(
            att[0]
                .get("emailAddress")
                .and_then(|e| e.get("address"))
                .and_then(Value::as_str),
            Some("bob@x.com")
        );
        assert_eq!(att[0].get("type").and_then(Value::as_str), Some("required"));
        // A create sends every group it is given (#1994).
        assert_eq!(sent.get("showAs").and_then(Value::as_str), Some("busy"));
        assert_eq!(
            sent.get("responseRequested").and_then(Value::as_bool),
            Some(true)
        );
        assert!(sent.get("isReminderOn").is_some());
    }

    /// The keys of a sent JSON object, in order.
    fn keys(body: &str) -> Vec<String> {
        match json::parse(body).unwrap() {
            Value::Object(o) => o.into_iter().map(|(k, _)| k).collect(),
            other => panic!("not an object: {other:?}"),
        }
    }

    /// Every input field set, every group sent: what a create of a full
    /// calendar-client event looks like (#1994).
    fn full_input() -> EventInput {
        EventInput {
            attendees: vec![
                AttendeeInput {
                    name: "Bob".into(),
                    addr: "bob@x.com".into(),
                    r#type: "optional".into(),
                },
                AttendeeInput {
                    name: "Room".into(),
                    addr: "room@x.com".into(),
                    r#type: "resource".into(),
                },
                AttendeeInput {
                    name: "Ann".into(),
                    addr: "ann@x.com".into(),
                    r#type: String::new(),
                },
            ],
            show_as: "tentative".into(),
            sensitivity: "private".into(),
            importance: "high".into(),
            categories: vec!["Red".into(), "Travel".into()],
            is_reminder_on: true,
            reminder_minutes: 30,
            is_online_meeting: true,
            online_meeting_provider: "teamsForBusiness".into(),
            response_requested: false,
            allow_new_time_proposals: false,
            ..sample_input()
        }
    }

    #[test]
    fn event_body_sends_calendar_client_fields() {
        let sent = json::parse(&event_body_json(&full_input())).unwrap();
        let s = |k: &str| sent.get(k).and_then(Value::as_str).map(str::to_string);
        let b = |k: &str| sent.get(k).and_then(Value::as_bool);
        assert_eq!(s("showAs").as_deref(), Some("tentative"));
        assert_eq!(s("sensitivity").as_deref(), Some("private"));
        assert_eq!(s("importance").as_deref(), Some("high"));
        let cats: Vec<&str> = sent
            .get("categories")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(cats, ["Red", "Travel"]);
        assert_eq!(b("isReminderOn"), Some(true));
        assert_eq!(
            sent.get("reminderMinutesBeforeStart")
                .and_then(Value::as_i64),
            Some(30)
        );
        assert_eq!(b("isOnlineMeeting"), Some(true));
        assert_eq!(
            s("onlineMeetingProvider").as_deref(),
            Some("teamsForBusiness")
        );
        assert_eq!(b("responseRequested"), Some(false));
        assert_eq!(b("allowNewTimeProposals"), Some(false));
        let types: Vec<&str> = sent
            .get("attendees")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|a| a.get("type").and_then(Value::as_str))
            .collect();
        // `""` sends `required`.
        assert_eq!(types, ["optional", "resource", "required"]);
    }

    #[test]
    fn event_body_sends_only_the_given_fields() {
        let cases: [(EventFieldSet, &[&str]); 14] = [
            (EventFieldSet::SUBJECT, &["subject"]),
            (EventFieldSet::TIMES, &["start", "end", "isAllDay"]),
            (EventFieldSet::LOCATION, &["location"]),
            (EventFieldSet::ATTENDEES, &["attendees"]),
            (EventFieldSet::BODY, &["body"]),
            // `sample_input` has no recurrence: nothing to send.
            (EventFieldSet::RECURRENCE, &[]),
            (EventFieldSet::SHOW_AS, &["showAs"]),
            (EventFieldSet::SENSITIVITY, &["sensitivity"]),
            (EventFieldSet::IMPORTANCE, &["importance"]),
            (EventFieldSet::CATEGORIES, &["categories"]),
            (
                EventFieldSet::REMINDER,
                &["isReminderOn", "reminderMinutesBeforeStart"],
            ),
            (
                EventFieldSet::ONLINE_MEETING,
                &["isOnlineMeeting", "onlineMeetingProvider"],
            ),
            (EventFieldSet::RESPONSE_REQUESTED, &["responseRequested"]),
            (
                EventFieldSet::ALLOW_NEW_TIME_PROPOSALS,
                &["allowNewTimeProposals"],
            ),
        ];
        for (group, want) in cases {
            let input = EventInput {
                fields: group,
                ..full_input()
            };
            assert_eq!(keys(&event_body_json(&input)), want, "{group:?}");
        }
        // The online meeting without a provider sends only the flag: Graph
        // then uses the calendar's default provider.
        let input = EventInput {
            fields: EventFieldSet::ONLINE_MEETING,
            online_meeting_provider: String::new(),
            ..full_input()
        };
        assert_eq!(keys(&event_body_json(&input)), ["isOnlineMeeting"]);
        // An empty set sends an empty object.
        let input = EventInput {
            fields: EventFieldSet::default(),
            ..full_input()
        };
        assert_eq!(event_body_json(&input), "{}");
    }

    #[test]
    fn event_body_skips_empty_enum_strings() {
        let input = EventInput {
            show_as: String::new(),
            sensitivity: String::new(),
            importance: String::new(),
            online_meeting_provider: String::new(),
            ..full_input()
        };
        let k = keys(&event_body_json(&input));
        for absent in [
            "showAs",
            "sensitivity",
            "importance",
            "onlineMeetingProvider",
        ] {
            assert!(!k.iter().any(|x| x == absent), "{absent} sent as \"\"");
        }
        assert!(k.iter().any(|x| x == "isOnlineMeeting"));
    }

    /// Pinned before #2039 named time zones: with no zone named, a full
    /// create's body, both recurrence shapes, is byte-for-byte this.
    #[test]
    fn event_body_without_zones_is_unchanged() {
        use crate::graph::model::{Recurrence, RecurrenceKind, RecurrencePattern, RecurrenceRange};
        let weekly = EventInput {
            recurrence: Some(Recurrence {
                kind: RecurrenceKind::Weekly,
                interval: 1,
                days_of_week: vec!["monday".into()],
                day_of_month: 0,
                start_date: "2026-07-20".into(),
                until: Some("2026-12-21".into()),
            }),
            ..full_input()
        };
        assert_eq!(
            event_body_json(&weekly),
            concat!(
                r#"{"subject":"Sync","start":{"dateTime":"2026-07-20T11:00:00","timeZone":"UTC"},"#,
                r#""end":{"dateTime":"2026-07-20T12:00:00","timeZone":"UTC"},"isAllDay":false,"#,
                r#""location":{"displayName":"Room 1"},"#,
                r#""attendees":[{"emailAddress":{"address":"bob@x.com","name":"Bob"},"type":"optional"},"#,
                r#"{"emailAddress":{"address":"room@x.com","name":"Room"},"type":"resource"},"#,
                r#"{"emailAddress":{"address":"ann@x.com","name":"Ann"},"type":"required"}],"#,
                r#""body":{"contentType":"HTML","content":"<p>agenda</p>"},"#,
                r#""recurrence":{"pattern":{"type":"weekly","interval":1,"daysOfWeek":["monday"],"#,
                r#""firstDayOfWeek":"sunday"},"range":{"type":"endDate","startDate":"2026-07-20","#,
                r#""endDate":"2026-12-21"}},"#,
                r#""showAs":"tentative","sensitivity":"private","importance":"high","#,
                r#""categories":["Red","Travel"],"isReminderOn":true,"reminderMinutesBeforeStart":30,"#,
                r#""isOnlineMeeting":true,"onlineMeetingProvider":"teamsForBusiness","#,
                r#""responseRequested":false,"allowNewTimeProposals":false}"#,
            )
        );
        let numbered = EventInput {
            series_recurrence: Some(EventRecurrence {
                pattern: RecurrencePattern {
                    r#type: "relativeMonthly".into(),
                    interval: 1,
                    days_of_week: vec!["monday".into()],
                    first_day_of_week: "monday".into(),
                    index: "third".into(),
                    ..Default::default()
                },
                range: RecurrenceRange {
                    r#type: "numbered".into(),
                    start_date: "2026-07-20".into(),
                    number_of_occurrences: 6,
                    recurrence_time_zone: "W. Europe Standard Time".into(),
                    ..Default::default()
                },
            }),
            is_all_day: true,
            start_utc: "2026-07-20T00:00:00Z".into(),
            end_utc: "2026-07-21T00:00:00Z".into(),
            ..full_input()
        };
        assert_eq!(
            event_body_json(&numbered),
            concat!(
                r#"{"subject":"Sync","start":{"dateTime":"2026-07-20T00:00:00","timeZone":"UTC"},"#,
                r#""end":{"dateTime":"2026-07-21T00:00:00","timeZone":"UTC"},"isAllDay":true,"#,
                r#""location":{"displayName":"Room 1"},"#,
                r#""attendees":[{"emailAddress":{"address":"bob@x.com","name":"Bob"},"type":"optional"},"#,
                r#"{"emailAddress":{"address":"room@x.com","name":"Room"},"type":"resource"},"#,
                r#"{"emailAddress":{"address":"ann@x.com","name":"Ann"},"type":"required"}],"#,
                r#""body":{"contentType":"HTML","content":"<p>agenda</p>"},"#,
                r#""recurrence":{"pattern":{"type":"relativeMonthly","interval":1,"daysOfWeek":["monday"],"#,
                r#""firstDayOfWeek":"monday","index":"third"},"range":{"type":"numbered","#,
                r#""startDate":"2026-07-20","numberOfOccurrences":6,"#,
                r#""recurrenceTimeZone":"W. Europe Standard Time"}},"#,
                r#""showAs":"tentative","sensitivity":"private","importance":"high","#,
                r#""categories":["Red","Travel"],"isReminderOn":true,"reminderMinutesBeforeStart":30,"#,
                r#""isOnlineMeeting":true,"onlineMeetingProvider":"teamsForBusiness","#,
                r#""responseRequested":false,"allowNewTimeProposals":false}"#,
            )
        );
    }

    /// `sample_input` named in two zones (#2039): 09:00 in Berlin's winter
    /// (UTC+1) is 08:00 UTC, and the end is 08:30 in London (UTC+0).
    fn zoned_input() -> EventInput {
        EventInput {
            start_utc: "2026-01-12T08:00:00Z".into(),
            end_utc: "2026-01-12T08:30:00Z".into(),
            start_time_zone: "W. Europe Standard Time".into(),
            start_local: "2026-01-12T09:00:00".into(),
            end_time_zone: "Europe/London".into(),
            end_local: "2026-01-12T08:30:00".into(),
            ..sample_input()
        }
    }

    #[test]
    fn event_body_sends_each_end_in_its_named_zone() {
        let sent = json::parse(&event_body_json(&zoned_input())).unwrap();
        assert_eq!(
            sent.get("start").unwrap().to_string(),
            r#"{"dateTime":"2026-01-12T09:00:00","timeZone":"W. Europe Standard Time"}"#
        );
        assert_eq!(
            sent.get("end").unwrap().to_string(),
            r#"{"dateTime":"2026-01-12T08:30:00","timeZone":"Europe/London"}"#
        );
        // One end named, the other in UTC as before.
        let input = EventInput {
            end_time_zone: String::new(),
            ..zoned_input()
        };
        let sent = json::parse(&event_body_json(&input)).unwrap();
        assert_eq!(
            sent.get("start")
                .and_then(|v| v.get("timeZone"))
                .and_then(Value::as_str),
            Some("W. Europe Standard Time")
        );
        assert_eq!(
            sent.get("end").unwrap().to_string(),
            r#"{"dateTime":"2026-01-12T08:30:00","timeZone":"UTC"}"#
        );
        // `TIMES` not in the set: no `start`/`end`, zoned or not.
        let input = EventInput {
            fields: EventFieldSet::SUBJECT,
            ..zoned_input()
        };
        assert_eq!(keys(&event_body_json(&input)), ["subject"]);
    }

    /// The `range.recurrenceTimeZone` of a body's recurrence, if any.
    fn range_zone(body: &str) -> Option<String> {
        json::parse(body)
            .unwrap()
            .get("recurrence")?
            .get("range")?
            .get("recurrenceTimeZone")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn weekly_monday() -> crate::graph::model::Recurrence {
        use crate::graph::model::{Recurrence, RecurrenceKind};
        Recurrence {
            kind: RecurrenceKind::Weekly,
            interval: 1,
            days_of_week: vec!["monday".into()],
            day_of_month: 0,
            start_date: "2026-01-12".into(),
            until: Some("2026-06-29".into()),
        }
    }

    fn lossless_monday(zone: &str) -> EventRecurrence {
        use crate::graph::model::{RecurrencePattern, RecurrenceRange};
        EventRecurrence {
            pattern: RecurrencePattern {
                r#type: "weekly".into(),
                interval: 1,
                days_of_week: vec!["monday".into()],
                first_day_of_week: "monday".into(),
                ..Default::default()
            },
            range: RecurrenceRange {
                r#type: "numbered".into(),
                start_date: "2026-01-12".into(),
                number_of_occurrences: 10,
                recurrence_time_zone: zone.into(),
                ..Default::default()
            },
        }
    }

    #[test]
    fn event_body_series_recurrence_zone_is_the_start_zone() {
        // The create-side recurrence gains the start's zone, after its
        // range's own keys.
        let input = EventInput {
            recurrence: Some(weekly_monday()),
            ..zoned_input()
        };
        let body = event_body_json(&input);
        assert_eq!(
            json::parse(&body)
                .unwrap()
                .get("recurrence")
                .unwrap()
                .to_string(),
            concat!(
                r#"{"pattern":{"type":"weekly","interval":1,"daysOfWeek":["monday"],"#,
                r#""firstDayOfWeek":"sunday"},"range":{"type":"endDate","#,
                r#""startDate":"2026-01-12","endDate":"2026-06-29","#,
                r#""recurrenceTimeZone":"W. Europe Standard Time"}}"#,
            )
        );
        // A lossless recurrence's own zone gives way to the start's.
        for own in ["", "Pacific Standard Time"] {
            let input = EventInput {
                series_recurrence: Some(lossless_monday(own)),
                ..zoned_input()
            };
            assert_eq!(
                range_zone(&event_body_json(&input)).as_deref(),
                Some("W. Europe Standard Time"),
                "own zone {own:?}"
            );
        }
        // So does a series edit that sends only the recurrence.
        let input = EventInput {
            series_recurrence: Some(lossless_monday("UTC")),
            fields: EventFieldSet::RECURRENCE,
            ..zoned_input()
        };
        let body = event_body_json(&input);
        assert_eq!(keys(&body), ["recurrence"]);
        assert_eq!(
            range_zone(&body).as_deref(),
            Some("W. Europe Standard Time")
        );
        // The end's zone plays no part: a start in UTC sends the
        // recurrence as before.
        let input = EventInput {
            start_time_zone: String::new(),
            series_recurrence: Some(lossless_monday("Pacific Standard Time")),
            ..zoned_input()
        };
        assert_eq!(
            range_zone(&event_body_json(&input)).as_deref(),
            Some("Pacific Standard Time")
        );
        let input = EventInput {
            start_time_zone: String::new(),
            recurrence: Some(weekly_monday()),
            ..zoned_input()
        };
        assert_eq!(range_zone(&event_body_json(&input)), None);
    }

    #[test]
    fn event_body_malformed_zone_pair_sends_utc() {
        let utc = |s: &str| format!(r#"{{"dateTime":"{s}","timeZone":"UTC"}}"#);
        for bad in [
            "",
            "2026-01-12 09:00:00",
            "2026-01-12T09:00:00Z",
            "2026-01-12T09:00:00.0000000",
            "2026-02-30T09:00:00",
            "2026-01-12T24:00:00",
            // 25 h ahead of its UTC time: the wrong day.
            "2026-01-13T09:00:00",
        ] {
            let input = EventInput {
                start_local: bad.into(),
                end_local: bad.into(),
                recurrence: Some(weekly_monday()),
                ..zoned_input()
            };
            let body = event_body_json(&input);
            let sent = json::parse(&body).unwrap();
            assert_eq!(
                sent.get("start").unwrap().to_string(),
                utc("2026-01-12T08:00:00"),
                "{bad:?}"
            );
            assert_eq!(
                sent.get("end").unwrap().to_string(),
                utc("2026-01-12T08:30:00"),
                "{bad:?}"
            );
            // A start sent in UTC names no recurrence zone either, so
            // `startDate` is never read in a zone `start` is not in.
            assert_eq!(range_zone(&body), None, "{bad:?}");
            let input = EventInput {
                start_local: bad.into(),
                series_recurrence: Some(lossless_monday("Pacific Standard Time")),
                ..zoned_input()
            };
            assert_eq!(
                range_zone(&event_body_json(&input)).as_deref(),
                Some("Pacific Standard Time"),
                "{bad:?}: the series' own zone, as before"
            );
        }
    }

    #[test]
    fn event_zone_error_checks_each_named_end() {
        let utc = "2026-01-12T08:00:00Z";
        let zone = "W. Europe Standard Time";
        // No zone: nothing to check, whatever the wall clock.
        assert_eq!(event_zone_error("", "junk", "junk"), None);
        assert_eq!(event_zone_error(zone, "2026-01-12T09:00:00", utc), None);
        // Graph's range of zones, UTC−12 to UTC+14, both ends included.
        assert_eq!(event_zone_error(zone, "2026-01-12T22:00:00", utc), None);
        assert_eq!(event_zone_error(zone, "2026-01-11T20:00:00", utc), None);
        assert!(event_zone_error(zone, "2026-01-12T22:00:01", utc).is_some());
        assert!(event_zone_error(zone, "2026-01-11T19:59:59", utc).is_some());
        // Not the wall clock's shape, or not a real date and time.
        for bad in [
            "",
            "2026-01-12T09:00",
            "2026-01-12T09:00:00Z",
            "2026-01-12T09:00:00+01:00",
            "2026-13-12T09:00:00",
            "0000-00-00T00:00:00",
            "2026-+1-12T09:00:00",
            "2026-01-12T09:00:60",
        ] {
            assert!(event_zone_error(zone, bad, utc).is_some(), "{bad:?}");
        }
        // A named end needs its UTC time in the store's shape.
        assert!(event_zone_error(zone, "2026-01-12T09:00:00", "2026-01-12T08:00:00").is_some());
        assert!(event_zone_error(zone, "2026-01-12T09:00:00", "").is_some());
    }

    #[test]
    fn event_field_set_names_round_trip() {
        assert_eq!(
            EventFieldSet::from_names(&EventFieldSet::ALL.to_names()),
            EventFieldSet::ALL
        );
        assert_eq!(EventFieldSet::ALL.to_names().len(), 14);
        let set = EventFieldSet::SUBJECT.union(EventFieldSet::REMINDER);
        assert_eq!(set.to_names(), ["subject", "reminder"]);
        // An unknown name (a newer build's group) is dropped.
        assert_eq!(
            EventFieldSet::from_names(&["subject", "reminder", "futureThing"]),
            set
        );
        assert!(EventFieldSet::default().is_empty());
        assert!(EventFieldSet::ALL.contains(EventFieldSet::LEGACY_UPDATE));
        let mut s = EventFieldSet::ALL;
        s.remove(EventFieldSet::REMINDER);
        assert!(!s.contains(EventFieldSet::REMINDER));
        assert!(s.contains(EventFieldSet::SUBJECT));
    }

    #[test]
    fn update_event_patches_only_changed_fields() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/events/EV1".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"EV1","subject":"New","isOnlineMeeting":true,"onlineMeetingProvider":"teamsForBusiness","onlineMeeting":{"joinUrl":"https://teams.microsoft.com/l/meetup-join/abc"}}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "T");
        let input = EventInput {
            subject: "New".into(),
            fields: EventFieldSet::SUBJECT,
            ..full_input()
        };
        let ev = c.update_event("EV1", &input).unwrap().unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(keys(&reqs[0].body), ["subject"]);
        // Graph's answer is the updated event.
        assert_eq!(ev.id, "EV1");
        assert_eq!(ev.join_url, "https://teams.microsoft.com/l/meetup-join/abc");
    }

    #[test]
    fn update_event_without_an_event_in_the_answer_is_ok_none() {
        let srv = FakeServer::start(vec![
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/events/E204".into(),
                status: 204,
                headers: vec![],
                body: "".into(),
            },
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/events/Eobj".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "T");
        assert_eq!(c.update_event("E204", &sample_input()).unwrap(), None);
        assert_eq!(c.update_event("Eobj", &sample_input()).unwrap(), None);
    }

    #[test]
    fn update_event_patches_and_delete_hits_delete() {
        let srv = FakeServer::start(vec![
            Route {
                method: "PATCH".into(),
                path_prefix: "/me/events/EV1".into(),
                status: 200,
                headers: vec![],
                body: "{}".into(),
            },
            Route {
                method: "DELETE".into(),
                path_prefix: "/me/events/EV2".into(),
                status: 204,
                headers: vec![],
                body: "".into(),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "T");
        c.update_event("EV1", &sample_input()).unwrap();
        c.delete_event("EV2").unwrap();
        assert!(
            srv.requests()
                .iter()
                .any(|r| r.method == "PATCH" && r.path.contains("/me/events/EV1"))
        );
        assert!(
            srv.requests()
                .iter()
                .any(|r| r.method == "DELETE" && r.path.contains("/me/events/EV2"))
        );
    }

    fn has_utc_prefer(r: &crate::testserver::RecordedRequest) -> bool {
        r.headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("prefer") && v == "outlook.timezone=\"UTC\"")
    }

    #[test]
    fn event_instances_gets_the_range_with_utc_and_pages() {
        // The master id holds `/`, `+` and `=`; the second page is the more
        // specific prefix, so it goes first (first prefix match wins).
        let srv = FakeServer::start(vec![
            Route {
                method: "GET".into(),
                path_prefix: "/me/events/M%2Fx%2B%3D/instances?$skiptoken=P2".into(),
                status: 200,
                headers: vec![],
                body: format!(
                    r#"{{"value":[{}]}}"#,
                    sample_event_json("OCC2", "2026-07-21T09:00:00")
                ),
            },
            Route {
                method: "GET".into(),
                path_prefix: "/me/events/M%2Fx%2B%3D/instances?startDateTime=".into(),
                status: 200,
                headers: vec![],
                body: format!(
                    r#"{{"value":[{}],"@odata.nextLink":"/me/events/M%2Fx%2B%3D/instances?$skiptoken=P2"}}"#,
                    sample_event_json("OCC1", "2026-07-14T09:00:00"),
                ),
            },
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let events = c
            .event_instances("M/x+=", "2026-07-01T00:00:00Z", "2026-08-01T00:00:00Z")
            .unwrap();
        let ids: Vec<&str> = events.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["OCC1", "OCC2"]);
        assert_eq!(events[0].start_utc, "2026-07-14T09:00:00Z");
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(
            reqs[0].path,
            "/me/events/M%2Fx%2B%3D/instances?startDateTime=2026-07-01T00:00:00Z\
             &endDateTime=2026-08-01T00:00:00Z&$top=50"
        );
        assert_eq!(
            reqs[1].path,
            "/me/events/M%2Fx%2B%3D/instances?$skiptoken=P2"
        );
        assert!(reqs.iter().all(|r| r.method == "GET" && has_utc_prefer(r)));
    }

    #[test]
    fn get_event_returns_the_master_with_its_recurrence() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/events/M%2F1".into(),
            status: 200,
            headers: vec![],
            body: r#"{"id":"M/1","subject":"Review","type":"seriesMaster",
                "start":{"dateTime":"2026-07-14T09:00:00.0000000","timeZone":"UTC"},
                "end":{"dateTime":"2026-07-14T10:00:00.0000000","timeZone":"UTC"},
                "recurrence":{"pattern":{"type":"relativeMonthly","interval":1,"month":0,
                "dayOfMonth":0,"daysOfWeek":["tuesday"],"firstDayOfWeek":"monday","index":"second"},
                "range":{"type":"numbered","startDate":"2026-07-14","endDate":"0001-01-01",
                "recurrenceTimeZone":"","numberOfOccurrences":10}}}"#
                .into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let master = c.get_event("M/1").unwrap();
        assert_eq!(master.id, "M/1");
        assert_eq!(master.event_type, "seriesMaster");
        assert_eq!(master.start_utc, "2026-07-14T09:00:00Z");
        let rec = master.recurrence.unwrap();
        assert_eq!(rec.pattern.r#type, "relativeMonthly");
        assert_eq!(rec.pattern.index, "second");
        assert_eq!(rec.pattern.first_day_of_week, "monday");
        assert_eq!(rec.range.r#type, "numbered");
        assert_eq!(rec.range.number_of_occurrences, 10);
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "GET");
        assert_eq!(reqs[0].path, "/me/events/M%2F1");
        assert!(has_utc_prefer(&reqs[0]));
    }

    #[test]
    fn get_event_without_id_is_a_parse_error() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/events/M".into(),
            status: 200,
            headers: vec![],
            body: r#"{"subject":"no id","type":"seriesMaster"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(c.get_event("M"), Err(GraphError::Parse(_))));
    }

    #[test]
    fn update_occurrence_patches_its_id_without_recurrence() {
        // Pin: an occurrence's edit is `update_event` on the occurrence's id
        // with the changed groups; a synced occurrence carries no
        // recurrence, so none is sent even with the group set.
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/events/OCC".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "T");
        let input = EventInput {
            subject: "Moved".into(),
            fields: EventFieldSet::SUBJECT.union(EventFieldSet::RECURRENCE),
            ..sample_input()
        };
        c.update_event("OCC", &input).unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(reqs[0].path, "/me/events/OCC");
        assert_eq!(keys(&reqs[0].body), ["subject"]);
    }

    #[test]
    fn update_series_patches_the_master_with_its_full_recurrence() {
        use crate::graph::model::{RecurrencePattern, RecurrenceRange};
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/events/MASTER".into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "T");
        let input = EventInput {
            subject: "Review".into(),
            series_recurrence: Some(EventRecurrence {
                pattern: RecurrencePattern {
                    r#type: "relativeMonthly".into(),
                    interval: 1,
                    days_of_week: vec!["tuesday".into()],
                    first_day_of_week: "monday".into(),
                    index: "second".into(),
                    ..Default::default()
                },
                range: RecurrenceRange {
                    r#type: "numbered".into(),
                    start_date: "2026-07-14".into(),
                    number_of_occurrences: 10,
                    ..Default::default()
                },
            }),
            fields: EventFieldSet::SUBJECT.union(EventFieldSet::RECURRENCE),
            ..sample_input()
        };
        c.update_event("MASTER", &input).unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(reqs[0].path, "/me/events/MASTER");
        assert_eq!(keys(&reqs[0].body), ["subject", "recurrence"]);
        let body = json::parse(&reqs[0].body).unwrap();
        assert_eq!(
            body.get("recurrence").unwrap().to_string(),
            r#"{"pattern":{"type":"relativeMonthly","interval":1,"daysOfWeek":["tuesday"],"firstDayOfWeek":"monday","index":"second"},"range":{"type":"numbered","startDate":"2026-07-14","numberOfOccurrences":10}}"#
        );
        // The same body through `update_event_body`, as a queued series op
        // sends it.
        c.update_event_body("MASTER", event_body_json(&input))
            .unwrap();
        assert_eq!(srv.requests()[1].body, reqs[0].body);
    }

    // --- A flagged message's reminder (#2054) ------------------------------

    fn get_route(prefix: &str, status: u16, body: &str) -> Route {
        Route {
            method: "GET".into(),
            path_prefix: prefix.into(),
            status,
            headers: vec![],
            body: body.into(),
        }
    }

    #[test]
    fn message_expand_names_the_reminder_class_and_protection_properties() {
        // The `$filter` inside the expand, decoded, is exactly the three
        // reminder property ids, the message class id and the three
        // protection ids (#2088) the model reads.
        use crate::graph::model::{
            CONTENT_CLASS_PROP, MESSAGE_CLASS_PROP, MSIP_LABELS_PROP, REMINDER_TIME_PROP,
            SENSITIVITY_PROP,
        };
        let decoded = MESSAGE_EXPAND
            .replace("%20", " ")
            .replace("%27", "'")
            .replace("%7B", "{")
            .replace("%7D", "}");
        assert_eq!(
            decoded,
            format!(
                "singleValueExtendedProperties($filter=id eq '{REMINDER_SET_PROP}' or \
                 id eq '{REMINDER_TIME_PROP}' or id eq '{REMINDER_SIGNAL_TIME_PROP}' or \
                 id eq '{MESSAGE_CLASS_PROP}' or id eq '{SENSITIVITY_PROP}' or \
                 id eq '{MSIP_LABELS_PROP}' or id eq '{CONTENT_CLASS_PROP}')"
            )
        );
        assert!(!MESSAGE_EXPAND.contains(' '));
    }

    #[test]
    fn delta_page_reads_each_message_protection() {
        // The protection comes back in the expand beside the class (Graph's
        // short tag form, a lower-cased GUID, a string Sensitivity): each
        // item reads its own, and every item carried the class, so each
        // writes, the plain one its default.
        use crate::graph::model::{MessageProtection, Sensitivity};
        let body = r#"{"value":[
            {"id":"M1","subject":"s","singleValueExtendedProperties":[
                {"id":"String 0x1a","value":"IPM.Note"},
                {"id":"String {00020386-0000-0000-c000-000000000046} Name msip_labels",
                 "value":"MSIP_Label_1b2c3d4e-0000-0000-0000-000000000001_Enabled=true; MSIP_Label_1b2c3d4e-0000-0000-0000-000000000001_Name=Confidential"}]},
            {"id":"M2","subject":"s","singleValueExtendedProperties":[
                {"id":"String 0x1a","value":"IPM.Note"},
                {"id":"Integer 0x36","value":"3"}]},
            {"id":"M3","subject":"s","singleValueExtendedProperties":[
                {"id":"String 0x1a","value":"IPM.Note"},
                {"id":"String {00020386-0000-0000-c000-000000000046} Name content-class",
                 "value":"rpmsg.message"}]},
            {"id":"M4","subject":"s","singleValueExtendedProperties":[
                {"id":"String 0x1a","value":"IPM.Note"}]}
            ],"@odata.deltaLink":"http://x/d"}"#;
        let srv = FakeServer::start(vec![get_route("/me/mailFolders/F1/", 200, body)]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let page = c.delta(DeltaCursor::Full("F1".to_string())).unwrap();
        let got: Vec<_> = page
            .items
            .iter()
            .map(|i| match i {
                crate::graph::model::DeltaItem::Upsert(m, f) => {
                    (m.id.as_str(), m.protection.clone(), f.writes_protection())
                }
                other => panic!("{other:?}"),
            })
            .collect();
        let labelled = MessageProtection {
            has_label: true,
            label_name: "Confidential".into(),
            ..Default::default()
        };
        let confidential = MessageProtection {
            sensitivity: Sensitivity::Confidential,
            ..Default::default()
        };
        let irm = MessageProtection {
            is_rights_protected: true,
            ..Default::default()
        };
        assert_eq!(
            got,
            [
                ("M1", labelled, true),
                ("M2", confidential, true),
                ("M3", irm, true),
                ("M4", MessageProtection::default(), true),
            ]
        );
        assert!(
            srv.requests()[0]
                .path
                .ends_with(&format!("&$expand={MESSAGE_EXPAND}"))
        );
    }

    #[test]
    fn delta_page_reads_each_message_class_as_its_meeting_type() {
        // The class comes back in the expand Graph's way (the short id
        // form): each meeting class is its type, a plain message `None`,
        // and every item carried it, so each writes.
        let item = |id: &str, class: &str| {
            format!(
                r#"{{"id":"{id}","subject":"s","singleValueExtendedProperties":[
                    {{"id":"String 0x1a","value":"{class}"}}]}}"#
            )
        };
        let body = format!(
            r#"{{"value":[{},{},{}],"@odata.deltaLink":"http://x/d"}}"#,
            item("M1", "IPM.Schedule.Meeting.Canceled"),
            item("M2", "IPM.Schedule.Meeting.Resp.Tent"),
            item("M3", "IPM.Note")
        );
        let srv = FakeServer::start(vec![get_route("/me/mailFolders/F1/", 200, &body)]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let page = c.delta(DeltaCursor::Full("F1".to_string())).unwrap();
        let got: Vec<_> = page
            .items
            .iter()
            .map(|i| match i {
                crate::graph::model::DeltaItem::Upsert(m, f) => (
                    m.id.as_str(),
                    m.meeting_message_type,
                    f.writes_meeting_type(),
                ),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            got,
            [
                ("M1", Some(MeetingMessageType::Cancelled), true),
                ("M2", Some(MeetingMessageType::TentativelyAccepted), true),
                ("M3", None, true),
            ]
        );
        assert!(
            srv.requests()[0]
                .path
                .ends_with(&format!("&$expand={MESSAGE_EXPAND}"))
        );
    }

    #[test]
    fn search_messages_reads_the_meeting_type_of_each_hit() {
        let srv = FakeServer::start(vec![search_page(
            "/me/messages?",
            r#"{"id":"M1","parentFolderId":"F1","singleValueExtendedProperties":[
                {"id":"String 0x001A","value":"IPM.Schedule.Meeting.Request"}]},
               {"id":"M2","parentFolderId":"F1"}"#,
            None,
        )]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let hits = c.search_messages(None, "budget", 5).unwrap();
        let got: Vec<_> = hits
            .iter()
            .map(|h| (h.message.meeting_message_type, h.fields.reminder_unasked))
            .collect();
        assert_eq!(
            got,
            [(Some(MeetingMessageType::Request), false), (None, false)]
        );
    }

    #[test]
    fn search_messages_retries_without_expand_on_400() {
        // Only the first request asks for the expand; a refusal sends it
        // once more without it, marks its hits unasked, and a nextLink is
        // then followed as Graph minted it.
        let plain = format!(
            "/me/messages?$search=%22budget%22&$select={MESSAGE_SELECT},parentFolderId&$top=5"
        );
        let srv = FakeServer::start(vec![
            get_route(&format!("{plain}&$expand="), 400, "{}"),
            search_page(
                "/me/messages?$skiptoken=2",
                r#"{"id":"M2","parentFolderId":"F1"}"#,
                None,
            ),
            search_page(
                &plain,
                r#"{"id":"M1","parentFolderId":"F1"}"#,
                Some("/me/messages?$skiptoken=2"),
            ),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let hits = c.search_messages(None, "budget", 5).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.fields.reminder_unasked));
        let paths: Vec<String> = srv.requests().into_iter().map(|r| r.path).collect();
        assert_eq!(
            paths,
            [
                format!("{plain}&$expand={MESSAGE_EXPAND}"),
                plain.clone(),
                "/me/messages?$skiptoken=2".to_string()
            ]
        );
    }

    #[test]
    fn get_message_expands_the_reminder() {
        let srv = FakeServer::start(vec![get_route(
            "/me/messages/",
            200,
            r#"{"id":"A/B","subject":"Hi"}"#,
        )]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.get_message("A/B").unwrap();
        assert_eq!(
            srv.requests()[0].path,
            format!("/me/messages/A%2FB?$select={MESSAGE_SELECT}&$expand={MESSAGE_EXPAND}")
        );
    }

    #[test]
    fn delta_link_is_sent_as_it_is() {
        // A link Graph minted carries the expand it accepted: nothing is
        // added to it.
        let srv = FakeServer::start(vec![delta_route()]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let link = format!(
            "{}/me/mailFolders/F1/messages/delta?$deltatoken=t",
            srv.base_url
        );
        let page = c.delta(DeltaCursor::Link(link)).unwrap();
        assert!(!page.expand_refused, "a link is no first call");
        assert_eq!(
            srv.requests()[0].path,
            "/me/mailFolders/F1/messages/delta?$deltatoken=t"
        );
    }

    #[test]
    fn delta_head_retries_without_expand_on_400() {
        let plain = "/me/mailFolders/F1/messages/delta\
                     ?$filter=receivedDateTime%20ge%202026-10-23T00:00:00Z\
                     &$orderby=receivedDateTime%20desc&$select=";
        let plain = format!("{plain}{MESSAGE_SELECT}");
        let srv = FakeServer::start(vec![
            get_route(
                &format!("{plain}&$expand="),
                400,
                r#"{"error":{"code":"BadRequest","message":"expand not supported"}}"#,
            ),
            get_route(
                &plain,
                200,
                r#"{"value":[{"id":"M1","subject":"s"}],"@odata.deltaLink":"http://x/d"}"#,
            ),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let page = c
            .delta(DeltaCursor::Head {
                folder_id: "F1".to_string(),
                since: "2026-10-23T00:00:00Z".to_string(),
            })
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.delta_link.as_deref(), Some("http://x/d"));
        // The page says it was read without the reminder, item by item.
        assert!(page.expand_refused);
        assert!(matches!(
            &page.items[0],
            crate::graph::model::DeltaItem::Upsert(_, f) if f.reminder_unasked && !f.writes_reminder()
        ));
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].path, format!("{plain}&$expand={MESSAGE_EXPAND}"));
        assert_eq!(reqs[1].path, plain);
        assert!(sends_maxpagesize(&reqs[1]));
    }

    #[test]
    fn delta_full_retries_without_expand_on_400_only() {
        let plain = format!(
            "/me/mailFolders/F1/messages/delta\
             ?$orderby=receivedDateTime%20desc&$select={MESSAGE_SELECT}"
        );
        // A 400 on the expand: the retry without it answers.
        let srv = FakeServer::start(vec![
            get_route(&format!("{plain}&$expand="), 400, "{}"),
            get_route(
                &plain,
                200,
                r#"{"value":[],"@odata.deltaLink":"http://x/d"}"#,
            ),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.delta(DeltaCursor::Full("F1".to_string())).unwrap();
        assert_eq!(srv.requests().len(), 2);
        // Another refusal (a 403) is no reason to retry.
        let srv = FakeServer::start(vec![get_route(&plain, 403, "{}")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(
            c.delta(DeltaCursor::Full("F1".to_string())),
            Err(GraphError::Http { status: 403, .. })
        ));
        assert_eq!(srv.requests().len(), 1);
    }

    #[test]
    fn get_message_retries_without_expand_on_400() {
        let plain = format!("/me/messages/M1?$select={MESSAGE_SELECT}");
        let body = r#"{"id":"M1","subject":"Hi","from":{"emailAddress":{"address":"a@x"}},"receivedDateTime":"2026-01-01T00:00:00Z"}"#;
        let srv = FakeServer::start(vec![
            get_route(&format!("{plain}&$expand="), 400, "{}"),
            get_route(&plain, 200, body),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let (m, f) = c.get_message("M1").unwrap();
        assert_eq!(m.subject, "Hi");
        assert!(!f.reminder, "no reminder came");
        assert!(
            f.reminder_unasked && !f.writes_reminder(),
            "and it was not asked for"
        );
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[1].path, plain);
        // A 400 the retry answers too is returned as it is.
        let srv = FakeServer::start(vec![get_route("/me/messages/", 400, "{}")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(
            c.get_message("M1"),
            Err(GraphError::Http { status: 400, .. })
        ));
        assert_eq!(srv.requests().len(), 2);
    }

    #[test]
    fn get_message_hit_retries_without_expand_on_400() {
        let plain = format!("/me/messages/M1?$select={MESSAGE_SELECT},parentFolderId");
        let srv = FakeServer::start(vec![
            get_route(&format!("{plain}&$expand="), 400, "{}"),
            get_route(&plain, 200, r#"{"id":"M1","parentFolderId":"F2"}"#),
        ]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let hit = c.get_message_hit("M1").unwrap();
        assert_eq!(hit.folder_id, "F2");
        assert!(hit.fields.reminder_unasked);
        assert_eq!(srv.requests()[1].path, plain);
    }

    #[test]
    fn get_message_reads_the_reminder() {
        let body = r#"{"id":"M1","subject":"Hi","flag":{"flagStatus":"flagged"},
            "singleValueExtendedProperties":[
              {"id":"Boolean {00062008-0000-0000-c000-000000000046} Id 0x8503","value":"true"},
              {"id":"SystemTime {00062008-0000-0000-c000-000000000046} Id 0x8560","value":"2026-11-08T10:15:00Z"}]}"#;
        let srv = FakeServer::start(vec![get_route("/me/messages/M1", 200, body)]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let (m, f) = c.get_message("M1").unwrap();
        assert!(m.is_reminder_on);
        assert_eq!(m.reminder_signal_at, "2026-11-08T10:15:00Z");
        assert!(f.reminder && !f.reminder_unasked);
    }

    fn ok_patch(path: &str) -> Route {
        Route {
            method: "PATCH".into(),
            path_prefix: path.into(),
            status: 200,
            headers: vec![],
            body: "{}".into(),
        }
    }

    #[test]
    fn dismiss_message_reminder_patches_reminder_set_false() {
        let srv = FakeServer::start(vec![ok_patch("/me/messages/")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.dismiss_message_reminder("A/B+C=").unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(reqs[0].path, "/me/messages/A%2FB%2BC%3D");
        assert_eq!(
            reqs[0].body,
            r#"{"singleValueExtendedProperties":[{"id":"Boolean {00062008-0000-0000-C000-000000000046} Id 0x8503","value":"false"}]}"#
        );
    }

    #[test]
    fn snooze_message_reminder_patches_signal_time() {
        let srv = FakeServer::start(vec![ok_patch("/me/messages/")]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.snooze_message_reminder("A/B", "2026-11-08T10:15:00Z")
            .unwrap();
        let reqs = srv.requests();
        assert_eq!(reqs[0].method, "PATCH");
        assert_eq!(reqs[0].path, "/me/messages/A%2FB");
        assert_eq!(
            reqs[0].body,
            r#"{"singleValueExtendedProperties":[{"id":"SystemTime {00062008-0000-0000-C000-000000000046} Id 0x8560","value":"2026-11-08T10:15:00Z"}]}"#
        );
    }

    #[test]
    fn message_reminder_refused_is_an_error() {
        let srv = FakeServer::start(vec![Route {
            method: "PATCH".into(),
            path_prefix: "/me/messages/M1".into(),
            status: 400,
            headers: vec![],
            body: "{}".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        assert!(matches!(
            c.dismiss_message_reminder("M1"),
            Err(GraphError::Http { status: 400, .. })
        ));
    }
}
