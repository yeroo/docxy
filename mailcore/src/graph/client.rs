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
    AttachmentMeta, Body, DeltaPage, Event, MailFolder, Message, Person, Recipient,
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
            GraphError::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            GraphError::Parse(m) => write!(f, "failed to parse response: {m}"),
            GraphError::Transport(m) => write!(f, "transport error: {m}"),
        }
    }
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

/// Which RSVP action `respond_event` sends, mapped to Graph's
/// `/me/events/{id}/accept|decline|tentativelyAccept` endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsvpKind {
    Accept,
    Decline,
    Tentative,
}

/// The `$select` fields covering every header field `Message::from_json`
/// reads, so delta pages carry everything the local store needs without
/// an extra per-message fetch.
const MESSAGE_SELECT: &str = "id,conversationId,subject,from,toRecipients,ccRecipients,\
receivedDateTime,sentDateTime,isRead,flag,hasAttachments,importance,bodyPreview";

enum Method {
    Get,
    Patch,
    Post,
    Delete,
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
    /// hierarchy later). A `visited` guard skips any folder id already
    /// collected, so a server that (erroneously, or via test-double route
    /// reuse) reports a folder as its own descendant can't recurse forever.
    ///
    /// The top-level call's error always propagates (can't list folders at
    /// all is a real failure). Each folder's `childFolders` sub-call has a
    /// looser policy — see `collect_folder_and_children`.
    pub fn list_folders(&self) -> Result<Vec<MailFolder>, GraphError> {
        let resp = self.send(Method::Get, "/me/mailFolders?$top=100", None, &[])?;
        let v = parse_body(resp)?;
        let items = value_array(&v, "value")?;
        let top: Vec<MailFolder> = items.iter().filter_map(MailFolder::from_json).collect();

        let mut all = Vec::new();
        let mut visited = std::collections::HashSet::new();
        for folder in top {
            self.collect_folder_and_children(folder, &mut all, &mut visited)?;
        }
        Ok(all)
    }

    /// Pushes `folder` onto `out`, then fetches and recurses into its
    /// `/childFolders`. See `list_folders` for the `visited` cycle guard.
    ///
    /// Error policy for the `childFolders` sub-call: `Unauthorized` and
    /// `Throttled` are global conditions the sync engine must handle
    /// centrally (refresh the token / back off), so those propagate out of
    /// `list_folders` same as before. Anything else (`NotFound`, `Http`,
    /// `Transport`, malformed-JSON `Parse`) is treated as "no discoverable
    /// children for this folder" and skipped — a transient failure or a 404
    /// on one deep, obscure subfolder must not discard the folders already
    /// collected (Inbox, Sent, etc.), which is what propagating via `?`
    /// unconditionally would do.
    fn collect_folder_and_children(
        &self,
        folder: MailFolder,
        out: &mut Vec<MailFolder>,
        visited: &mut std::collections::HashSet<String>,
    ) -> Result<(), GraphError> {
        if !visited.insert(folder.id.clone()) {
            return Ok(());
        }
        let path = format!(
            "/me/mailFolders/{}/childFolders?$top=100",
            encode_path_segment(&folder.id)
        );
        out.push(folder);

        let children = match self.fetch_child_folders(&path) {
            Ok(children) => children,
            Err(e @ (GraphError::Unauthorized | GraphError::Throttled { .. })) => return Err(e),
            Err(_) => return Ok(()),
        };
        for child in children {
            self.collect_folder_and_children(child, out, visited)?;
        }
        Ok(())
    }

    /// GET a `childFolders` page and parse it into `MailFolder`s. Split out
    /// of `collect_folder_and_children` so that method's error-tolerance
    /// match has a single fallible call to match on.
    fn fetch_child_folders(&self, path: &str) -> Result<Vec<MailFolder>, GraphError> {
        let resp = self.send(Method::Get, path, None, &[])?;
        let v = parse_body(resp)?;
        let items = value_array(&v, "value")?;
        Ok(items.iter().filter_map(MailFolder::from_json).collect())
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
    ) -> Result<(), GraphError> {
        let id = encode_path_segment(id);
        let action = match kind {
            RsvpKind::Accept => "accept",
            RsvpKind::Decline => "decline",
            RsvpKind::Tentative => "tentativelyAccept",
        };
        let path = format!("/me/events/{id}/{action}");
        let body = Value::Object(vec![
            (
                "comment".to_string(),
                Value::Str(comment.unwrap_or("").to_string()),
            ),
            ("sendResponse".to_string(), Value::Bool(send_response)),
        ])
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

        let result = match &body {
            Some(b) => req.set("Content-Type", "application/json").send_string(b),
            None => req.call(),
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

/// Reads `key` off `v` as a JSON array, erroring with a `Parse` when it's
/// missing or the wrong shape.
fn value_array<'a>(v: &'a Value, key: &str) -> Result<&'a [Value], GraphError> {
    v.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| GraphError::Parse(format!("response has no '{key}' array")))
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
fn base64_decode(s: &str) -> Option<Vec<u8>> {
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

/// Standard base64 (RFC 4648 §4: `+`/`/` alphabet, `=` padding) — the encoding
/// Graph expects for `fileAttachment.contentBytes`. The `pkce` module's
/// base64*url* encoder can't be reused (different alphabet, no padding).
///
/// Not yet called from production code: the outbound-attachments task that
/// wires this into a `fileAttachment` request body lands separately. Until
/// then it's only exercised by `base64_encode_matches_known_vectors` below.
#[allow(dead_code)]
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
        let folders = c.list_folders().unwrap();
        assert_eq!(folders[0].display_name, "Inbox");
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
        let folders = c.list_folders().unwrap();
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
        let folders = c.list_folders().unwrap();
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].id, "F1");
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
    fn respond_event_accept_posts_comment_and_send_response() {
        let srv = FakeServer::start(vec![Route {
            method: "POST".into(),
            path_prefix: "/me/events/E1/accept".into(),
            status: 202,
            headers: vec![],
            body: "".into(),
        }]);
        let c = GraphClient::new(&srv.base_url, "AT");
        c.respond_event("E1", RsvpKind::Accept, Some("ok"), true)
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
        c.respond_event("E1", RsvpKind::Decline, None, false)
            .unwrap();
        c.respond_event("E1", RsvpKind::Tentative, None, false)
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
            c.respond_event("E1", RsvpKind::Accept, None, true),
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
        c.respond_event(raw_id, RsvpKind::Accept, None, true)
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
}
