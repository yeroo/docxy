//! Blocking Microsoft Graph REST client, over `ureq`.
//!
//! `base` is injectable so tests point it at the in-process fake server
//! (`crate::testserver`); production passes `https://graph.microsoft.com/v1.0`.
//! Every request sends `Authorization: Bearer {token}` and
//! `Accept: application/json`. Token refresh is not this client's job — on
//! `GraphError::Unauthorized` it just returns the error; the sync engine
//! (a later task) catches it, refreshes, and retries once. The bearer token
//! is never logged (it's not `Debug`-derived into any log line here).

use crate::graph::model::{AttachmentMeta, Body, DeltaPage, MailFolder};
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
}
