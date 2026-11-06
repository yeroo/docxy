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
    AttachmentMeta, AutomaticReplies, Body, DeltaPage, Event, FolderListing, MailFolder,
    MasterCategory, Me, Message, MessageFields, OofStatus, Person, Recipient, Recurrence,
    ScheduleEntry, plain_to_html,
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

/// Where a delta query starts from: the first call for a folder, or a
/// `nextLink`/`deltaLink` URL carried over from a previous page.
#[derive(Debug, Clone, PartialEq)]
pub enum DeltaCursor {
    /// First delta call for this mail folder id.
    Folder(String),
    /// A `@odata.nextLink` (more pages) or `@odata.deltaLink` (resume point
    /// for the next sync) from a prior `DeltaPage`. Already a complete URL,
    /// so it's sent as-is rather than joined with `base`.
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

/// The editable fields of an event, as the create/update calls serialize them.
#[derive(Debug, Clone, PartialEq)]
pub struct EventInput {
    pub subject: String,
    pub start_utc: String, // YYYY-MM-DDTHH:MM:SSZ
    pub end_utc: String,
    pub is_all_day: bool,
    pub location: String,
    pub attendees: Vec<(String, String)>, // (name, address)
    pub body_html: String,
    /// The recurrence pattern for a repeating event, or `None` for a single
    /// event. Serialized into the create/update body by `event_body_json`.
    pub recurrence: Option<Recurrence>,
}

/// The `$select` fields covering every header field `Message::from_json`
/// reads, so delta pages carry everything the local store needs without
/// an extra per-message fetch.
const MESSAGE_SELECT: &str = "id,conversationId,subject,from,toRecipients,ccRecipients,\
receivedDateTime,sentDateTime,isRead,flag,hasAttachments,importance,bodyPreview,categories";

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

    /// GET a folder's `/messages/delta` (first call, `DeltaCursor::Folder`)
    /// or a stored `nextLink`/`deltaLink` (`DeltaCursor::Link`), requesting
    /// pages of 50 via `Prefer: odata.maxpagesize=50`.
    pub fn delta(&self, cursor: DeltaCursor) -> Result<DeltaPage, GraphError> {
        let target = match cursor {
            DeltaCursor::Folder(folder_id) => {
                let folder_id = encode_path_segment(&folder_id);
                format!("/me/mailFolders/{folder_id}/messages/delta?$select={MESSAGE_SELECT}")
            }
            DeltaCursor::Link(url) => url,
        };
        let resp = self.send(
            Method::Get,
            &target,
            None,
            &[("Prefer", "odata.maxpagesize=50")],
        )?;
        let v = parse_body(resp)?;
        DeltaPage::from_json(&v)
            .ok_or_else(|| GraphError::Parse("malformed delta page".to_string()))
    }

    /// GET `/me/messages/{id}?$select=MESSAGE_SELECT`: one message's
    /// stored properties, with which of them the response carried. The
    /// sync engine completes a header-less row with it (#1899).
    pub fn get_message(&self, message_id: &str) -> Result<(Message, MessageFields), GraphError> {
        let message_id = encode_path_segment(message_id);
        let path = format!("/me/messages/{message_id}?$select={MESSAGE_SELECT}");
        let resp = self.send(Method::Get, &path, None, &[])?;
        let v = parse_body(resp)?;
        let m = Message::from_json(&v)
            .ok_or_else(|| GraphError::Parse("malformed message".to_string()))?;
        Ok((m, MessageFields::of(&v)))
    }

    /// GET `/me/messages/{id}?$select=body`. When `prefer_text` is set,
    /// asks Graph to convert the body to plain text via
    /// `Prefer: outlook.body-content-type="text"`; otherwise Graph returns
    /// its native HTML.
    pub fn get_body(&self, message_id: &str, prefer_text: bool) -> Result<Body, GraphError> {
        let message_id = encode_path_segment(message_id);
        let path = format!("/me/messages/{message_id}?$select=body");
        let headers: &[(&str, &str)] = if prefer_text {
            &[("Prefer", "outlook.body-content-type=\"text\"")]
        } else {
            &[]
        };
        let resp = self.send(Method::Get, &path, None, headers)?;
        let v = parse_body(resp)?;
        let body = v
            .get("body")
            .ok_or_else(|| GraphError::Parse("response has no body field".to_string()))?;
        Body::from_json(body).ok_or_else(|| GraphError::Parse("malformed body".to_string()))
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

    /// DELETE `/me/messages/{id}`.
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

    /// GET `/me/calendarView?startDateTime=&endDateTime=&$top=50`, sending
    /// `Prefer: outlook.timezone="UTC"` so every returned event's `start`/
    /// `end` is already a UTC wall-clock time (see `Event::from_json` /
    /// `model::to_utc` for the fixed-width normalization this enables), and
    /// following `@odata.nextLink` — a relative path or a full URL, either
    /// way `full_url` resolves it — until Graph stops sending one.
    pub fn calendar_view(&self, from_utc: &str, to_utc: &str) -> Result<Vec<Event>, GraphError> {
        let mut target =
            format!("/me/calendarView?startDateTime={from_utc}&endDateTime={to_utc}&$top=50");
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
        let dt = |utc: &str| {
            Value::Object(vec![
                (
                    "dateTime".to_string(),
                    Value::Str(utc.trim_end_matches('Z').to_string()),
                ),
                ("timeZone".to_string(), Value::Str("UTC".to_string())),
            ])
        };
        let body = Value::Object(vec![
            (
                "schedules".to_string(),
                Value::Array(schedules.iter().map(|s| Value::Str(s.clone())).collect()),
            ),
            ("startTime".to_string(), dt(start_utc)),
            ("endTime".to_string(), dt(end_utc)),
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
    /// for every call.
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
        if let Some((start, end)) = &proposed {
            let dt = |utc: &str| {
                Value::Object(vec![
                    ("dateTime".to_string(), Value::Str(utc.to_string())),
                    ("timeZone".to_string(), Value::Str("UTC".to_string())),
                ])
            };
            obj.push((
                "proposedNewTime".to_string(),
                Value::Object(vec![
                    ("start".to_string(), dt(start)),
                    ("end".to_string(), dt(end)),
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
        let declared = resp
            .header("Content-Length")
            .and_then(|v| v.trim().parse::<u64>().ok());
        if declared.is_some_and(|n| n > PHOTO_MAX as u64) {
            return Err(photo_too_large(PHOTO_MAX));
        }
        let content_type = resp
            .header("Content-Type")
            .map(|v| v.split(';').next().unwrap_or("").trim())
            .filter(|v| !v.is_empty())
            .unwrap_or("application/octet-stream")
            .to_string();
        let bytes = read_capped(resp.into_reader(), PHOTO_MAX)?;
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
        let resp = self.send(
            Method::Post,
            "/me/events",
            Some(event_body_json(input)),
            &[],
        )?;
        let v = parse_body(resp)?;
        Event::from_json(&v).ok_or_else(|| GraphError::Parse("malformed created event".to_string()))
    }

    /// PATCH `/me/events/{id}` with the same body shape as `create_event`.
    pub fn update_event(&self, id: &str, input: &EventInput) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/events/{id}");
        self.send(Method::Patch, &path, Some(event_body_json(input)), &[])?;
        Ok(())
    }

    /// DELETE `/me/events/{id}` (cancels an organized event / removes an
    /// invited one from the user's calendar).
    pub fn delete_event(&self, id: &str) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let path = format!("/me/events/{id}");
        self.send(Method::Delete, &path, None, &[])?;
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
        if bytes.len() <= INLINE_MAX {
            self.post_file_attachment(message_id, name, content_type, bytes, content_id)
        } else {
            self.upload_large_attachment(message_id, name, content_type, bytes, content_id)
        }
    }

    /// POST `.../attachments/createUploadSession`, then PUT the bytes to the
    /// returned pre-authenticated `uploadUrl` in `CHUNK_SIZE` chunks, each with
    /// a `Content-Range` header, until the last chunk completes.
    fn upload_large_attachment(
        &self,
        message_id: &str,
        name: &str,
        content_type: &str,
        bytes: &[u8],
        content_id: Option<&str>,
    ) -> Result<(), GraphError> {
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

        let total = bytes.len();
        for (start, end) in chunk_ranges(total, CHUNK_SIZE) {
            let range = format!("bytes {}-{}/{}", start, end - 1, total);
            self.put_bytes(&upload_url, &range, &bytes[start..end])?;
        }
        Ok(())
    }

    /// Raw `PUT` of one `chunk` to a Graph upload-session `upload_url` with a
    /// `Content-Range` header and NO bearer (the session URL is already
    /// authorized). A 2xx (202 for an accepted intermediate chunk, 200/201 for
    /// the final one) is Ok; anything else maps to a `GraphError`.
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

/// Reads a successful response's body and parses it as JSON.
fn parse_body(resp: ureq::Response) -> Result<Value, GraphError> {
    let text = resp
        .into_string()
        .map_err(|e| GraphError::Transport(e.to_string()))?;
    json::parse(&text).map_err(|e| GraphError::Parse(e.to_string()))
}

/// The largest profile photo `GraphClient::my_photo` accepts (4 MiB). Graph's
/// photos are a few kilobytes; anything near this is not a photo.
pub const PHOTO_MAX: usize = 4 * 1024 * 1024;

fn photo_too_large(max: usize) -> GraphError {
    GraphError::Parse(format!("profile photo is larger than {max} bytes"))
}

/// Reads `r` to its end, refusing (`photo_too_large`: only photos use it)
/// once it yields more than `max` bytes. At most `max + 1` bytes are ever
/// read, so a chunked or lying response cannot make it buffer more.
fn read_capped(r: impl std::io::Read, max: usize) -> Result<Vec<u8>, GraphError> {
    use std::io::Read;
    let mut buf = Vec::new();
    r.take(max as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| GraphError::Transport(e.to_string()))?;
    if buf.len() > max {
        return Err(photo_too_large(max));
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

/// Serializes an `EventInput` to the JSON body shared by `create_event` and
/// `update_event`. `start`/`end` carry the UTC wall clock (the `Z` suffix
/// stripped) with `timeZone:"UTC"`, matching how `calendar_view` reads events
/// back under the `Prefer: outlook.timezone="UTC"` header.
fn event_body_json(input: &EventInput) -> String {
    let dt = |utc: &str| -> Value {
        Value::Object(vec![
            (
                "dateTime".to_string(),
                Value::Str(utc.trim_end_matches('Z').to_string()),
            ),
            ("timeZone".to_string(), Value::Str("UTC".to_string())),
        ])
    };
    let attendees = Value::Array(
        input
            .attendees
            .iter()
            .map(|(name, addr)| {
                Value::Object(vec![
                    (
                        "emailAddress".to_string(),
                        Value::Object(vec![
                            ("address".to_string(), Value::Str(addr.clone())),
                            ("name".to_string(), Value::Str(name.clone())),
                        ]),
                    ),
                    ("type".to_string(), Value::Str("required".to_string())),
                ])
            })
            .collect(),
    );
    let mut obj = vec![
        ("subject".to_string(), Value::Str(input.subject.clone())),
        ("start".to_string(), dt(&input.start_utc)),
        ("end".to_string(), dt(&input.end_utc)),
        ("isAllDay".to_string(), Value::Bool(input.is_all_day)),
        (
            "location".to_string(),
            Value::Object(vec![(
                "displayName".to_string(),
                Value::Str(input.location.clone()),
            )]),
        ),
        ("attendees".to_string(), attendees),
        (
            "body".to_string(),
            Value::Object(vec![
                ("contentType".to_string(), Value::Str("HTML".to_string())),
                ("content".to_string(), Value::Str(input.body_html.clone())),
            ]),
        ),
    ];
    if let Some(rec) = &input.recurrence {
        obj.push(("recurrence".to_string(), rec.to_json()));
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

/// Splits `total` bytes into consecutive `(start, end)` half-open ranges of at
/// most `chunk` bytes each — `end` exclusive, so `bytes[start..end]` is the
/// slice to send and the last range's `end` always equals `total`. Empty
/// (`total == 0`) yields no ranges.
fn chunk_ranges(total: usize, chunk: usize) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut start = 0;
    while start < total {
        let end = (start + chunk).min(total);
        v.push((start, end));
        start = end;
    }
    v
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

    #[test]
    fn delta_folder_first_call_sends_maxpagesize_prefer_and_parses() {
        let srv = FakeServer::start(vec![Route {
            method: "GET".into(),
            path_prefix: "/me/mailFolders/F1/messages/delta".into(),
            status: 200,
            headers: vec![],
            body: r#"{"value":[],"@odata.deltaLink":"http://x/delta?token=abc"}"#.into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        let page = c.delta(DeltaCursor::Folder("F1".to_string())).unwrap();
        assert_eq!(page.delta_link.as_deref(), Some("http://x/delta?token=abc"));
        let reqs = srv.requests();
        assert!(reqs[0].path.contains("$select="));
        assert!(
            reqs[0]
                .headers
                .iter()
                .any(|(k, v)| k.eq_ignore_ascii_case("prefer") && v == "odata.maxpagesize=50")
        );
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
        match read_capped(std::io::repeat(b'x'), 1024) {
            Err(GraphError::Parse(m)) => assert!(m.contains("larger than 1024"), "{m}"),
            other => panic!("expected a Parse error, got {:?}", other.map(|b| b.len())),
        }
    }

    #[test]
    fn read_capped_accepts_exactly_max() {
        use std::io::Read;
        let bytes = read_capped(std::io::repeat(b'x').take(1024), 1024).unwrap();
        assert_eq!(bytes.len(), 1024);
        assert_eq!(read_capped(&b""[..], 1024).unwrap(), Vec::<u8>::new());
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
            Some("2026-07-21T14:00:00Z")
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
        assert_eq!(chunk_ranges(0, 100), Vec::<(usize, usize)>::new());
        assert_eq!(chunk_ranges(100, 100), vec![(0, 100)]);
        assert_eq!(
            chunk_ranges(250, 100),
            vec![(0, 100), (100, 200), (200, 250)]
        );
        // A payload just over INLINE_MAX still fits inside one CHUNK_SIZE chunk
        // (CHUNK_SIZE is much bigger than INLINE_MAX), matching the "big"
        // payload used in `add_attachment_routes_small_inline_and_large_to_upload_session`.
        let total = INLINE_MAX + 100;
        assert_eq!(chunk_ranges(total, CHUNK_SIZE), vec![(0, total)]);
        // Multiple full CHUNK_SIZE-sized chunks plus a smaller remainder.
        let total2 = CHUNK_SIZE * 2 + 500;
        let ranges = chunk_ranges(total2, CHUNK_SIZE);
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

    fn sample_input() -> EventInput {
        EventInput {
            subject: "Sync".into(),
            start_utc: "2026-07-20T11:00:00Z".into(),
            end_utc: "2026-07-20T12:00:00Z".into(),
            is_all_day: false,
            location: "Room 1".into(),
            attendees: vec![("Bob".into(), "bob@x.com".into())],
            body_html: "<p>agenda</p>".into(),
            recurrence: None,
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
}
