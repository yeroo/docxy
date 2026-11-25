//! MHTML (#1898): a Web Page saved as one MIME message, `multipart/related`,
//! as Word's "Single File Web Page" (`.mht`) and a browser's "Webpage,
//! single file" (`.mhtml`) write it.
//!
//! [`parse`] finds the root HTML part (decoded, with its charset and its
//! `Content-Location`, the page's URL) and keeps where every other part's
//! body is, undecoded; [`Mhtml::part_for`] decodes the one an `<img src>`
//! names, by its `Content-Location` or a `cid:` URL naming its
//! `Content-ID`. A malformed message is no MHTML ([`parse`] gives `None`),
//! and the caller reads the bytes as a plain page.
//!
//! Bounds: the top header block is read within [`MAX_HEADER`] bytes, at
//! most [`MAX_PARTS`] parts are kept, each part's headers within
//! [`MAX_HEADER`]; the message is scanned once for its boundaries (charged
//! as scanned), the root part's decoded bytes are charged as decoded, and
//! an image part is decoded only when asked for, refused unread when its
//! decoded size may pass the limit it is asked within.

use super::Budget;
use super::html_image::{base64_decode, percent_decode};

/// The most bytes a header block (the message's or a part's) may take.
pub(crate) const MAX_HEADER: usize = 16 << 10;
/// The most parts one message keeps; later ones are not read.
pub(crate) const MAX_PARTS: usize = 4096;

/// A header block's fields, unfolded: `(name, value)`, names lowercase.
type Fields = Vec<(String, String)>;

/// How a part's body is encoded (`Content-Transfer-Encoding`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    Base64,
    QuotedPrintable,
    /// `7bit`, `8bit`, `binary`, or none given: as it is.
    Identity,
}

/// One part other than the root: where its body is in the message.
#[derive(Debug)]
struct Part {
    /// Its `Content-Location`, as written.
    location: Option<String>,
    /// Its `Content-ID`, angle brackets dropped, lowercase.
    cid: Option<String>,
    encoding: Encoding,
    body: (usize, usize),
}

/// A parsed MHTML message.
#[derive(Debug)]
pub(crate) struct Mhtml<'a> {
    bytes: &'a [u8],
    parts: Vec<Part>,
    /// The root part's decoded bytes: the page.
    pub(crate) html: Vec<u8>,
    /// The root part's `charset`, lowercase, if its `Content-Type` gives
    /// one: it wins over a `<meta charset>` in the page, as a transport
    /// header does in a browser.
    pub(crate) charset: Option<String>,
    /// The root part's `Content-Location`: the page's URL, which its
    /// relative srcs are resolved against.
    pub(crate) location: Option<String>,
}

/// Whether `bytes` start as an MHTML message: a header block (within
/// [`MAX_HEADER`] bytes, up to its blank line) whose `Content-Type` is
/// `multipart/related`. Word's message has a preamble after its headers,
/// Blink's starts with `From: <Saved by Blink>`; both are taken.
pub(crate) fn looks_like(bytes: &[u8]) -> bool {
    top_headers(bytes).is_some_and(|(headers, _)| related_type(&headers).is_some())
}

/// `bytes` as an MHTML message, or `None` when they are not one (or one so
/// malformed that it has no HTML root).
pub(crate) fn parse<'a>(bytes: &'a [u8], budget: &Budget) -> Option<Mhtml<'a>> {
    let (headers, body_at) = top_headers(bytes)?;
    let content_type = related_type(&headers)?;
    let params = type_params(&content_type);
    let boundary = field(&params, "boundary").filter(|b| !b.is_empty() && b.len() <= 200)?;
    if !budget.scanned(bytes.len() - body_at) {
        return None;
    }
    let delimiter = format!("--{boundary}");
    let mut parts = Vec::new();
    let mut root: Option<(Fields, (usize, usize))> = None;
    let start = field(&params, "start").map(|s| content_id(&s));
    let root_type = field(&params, "type").map(|t| t.to_ascii_lowercase());
    // The root candidates while no `start` part is found: the first part
    // of the `type` given, else the first `text/html` one.
    let mut first_html: Option<(Fields, (usize, usize))> = None;
    let mut at = find_delimiter(bytes, body_at, delimiter.as_bytes())?;
    loop {
        // Past the delimiter: `--` closes the message; else the rest of
        // its line (transport padding) is skipped.
        let after = at + delimiter.len();
        if bytes[after..].starts_with(b"--") {
            break;
        }
        let line_end = memchr(b'\n', bytes, after)?;
        let part_at = line_end + 1;
        let next = find_delimiter(bytes, part_at, delimiter.as_bytes());
        // The CRLF before the next delimiter is the delimiter's.
        let part_end = match next {
            Some(n) => trim_line_end(bytes, part_at, n),
            None => bytes.len(),
        };
        let (part_headers, body_at) = header_block(bytes, part_at, part_end)?;
        if parts.len() >= MAX_PARTS || !budget.keep::<Part>(64) {
            break;
        }
        let get = |name: &str| field(&part_headers, name);
        let cid = get("content-id").map(|c| content_id(&c));
        let ptype = get("content-type").unwrap_or_default();
        let mime = essence(&ptype);
        let body = (body_at, part_end);
        let is_start = start.is_some() && cid == start;
        let is_type = match root_type.as_deref() {
            Some(t) => mime == t,
            None => mime == "text/html",
        };
        if is_start && root.is_none() {
            root = Some((part_headers, body));
        } else {
            if is_type && first_html.is_none() {
                first_html = Some((part_headers.clone(), body));
            }
            parts.push(Part {
                location: get("content-location").map(|l| l.trim().to_string()),
                cid,
                encoding: encoding(get("content-transfer-encoding").as_deref()),
                body,
            });
        }
        match next {
            Some(n) => at = n,
            None => break,
        }
    }
    let (root_headers, (from, to)) = root.or(first_html)?;
    let get = |name: &str| field(&root_headers, name);
    let root_type = get("content-type").unwrap_or_default();
    if !matches!(
        essence(&root_type).as_str(),
        "text/html" | "application/xhtml+xml"
    ) {
        return None;
    }
    let raw = &bytes[from..to];
    let html = match encoding(get("content-transfer-encoding").as_deref()) {
        Encoding::Base64 => base64_decode(raw)?,
        Encoding::QuotedPrintable => quoted_printable(raw),
        Encoding::Identity => raw.to_vec(),
    };
    if !budget.take_bytes(html.len()) {
        return None;
    }
    let charset = field(&type_params(&root_type), "charset").map(|c| c.to_ascii_lowercase());
    Some(Mhtml {
        bytes,
        parts,
        html,
        charset,
        location: get("content-location").map(|l| l.trim().to_string()),
    })
}

impl Mhtml<'_> {
    /// The body of the part `src` names (`src` resolved against the page's
    /// URL): `None` when no part does; `Some(None)` when one does but its
    /// body is unreadable or may decode to more than `limit` bytes (it is
    /// then not read). A `cid:` URL names a part by its `Content-ID`
    /// (percent-decoded, without case); any other by its
    /// `Content-Location`, exactly or once both are percent-decoded.
    pub(crate) fn part_for(&self, src: &str, limit: usize) -> Option<Option<Vec<u8>>> {
        let part = match src.get(..4).filter(|s| s.eq_ignore_ascii_case("cid:")) {
            Some(_) => {
                let id = String::from_utf8_lossy(&percent_decode(&src[4..])).to_ascii_lowercase();
                let id = content_id(&id);
                self.parts
                    .iter()
                    .find(|p| p.cid.as_deref() == Some(id.as_str()))
            }
            None => self
                .parts
                .iter()
                .find(|p| p.location.as_deref() == Some(src))
                .or_else(|| {
                    let want = percent_decode(src);
                    self.parts.iter().find(|p| {
                        p.location
                            .as_deref()
                            .is_some_and(|l| percent_decode(l) == want)
                    })
                }),
        }?;
        let raw = &self.bytes[part.body.0..part.body.1];
        let bound = match part.encoding {
            Encoding::Base64 => raw.len() / 4 * 3 + 3,
            _ => raw.len(),
        };
        if bound > limit {
            return Some(None);
        }
        Some(match part.encoding {
            Encoding::Base64 => base64_decode(raw),
            Encoding::QuotedPrintable => Some(quoted_printable(raw)),
            Encoding::Identity => Some(raw.to_vec()),
        })
    }
}

/// The message's top header block (unfolded `(name, value)` pairs, names
/// lowercase) and where its body starts: `None` unless it is a header block
/// that ends with a blank line within [`MAX_HEADER`] bytes.
fn top_headers(bytes: &[u8]) -> Option<(Fields, usize)> {
    let from = if bytes.starts_with(b"\xEF\xBB\xBF") {
        3
    } else {
        0
    };
    header_block(bytes, from, bytes.len()).filter(|(h, _)| !h.is_empty())
}

/// The header block of `bytes[from..to]`: its fields, unfolded, and where
/// the body after its blank line starts. `None` when a line is not a
/// field (`name: value`, or a fold of one) or no blank line ends it within
/// [`MAX_HEADER`] bytes of `from`.
fn header_block(bytes: &[u8], from: usize, to: usize) -> Option<(Fields, usize)> {
    let to = to.min(from.saturating_add(MAX_HEADER)).min(bytes.len());
    let mut fields: Fields = Vec::new();
    let mut at = from;
    while at < to {
        let end = memchr(b'\n', &bytes[..to], at)?;
        let line = &bytes[at..end];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        at = end + 1;
        if line.is_empty() {
            return Some((fields, at));
        }
        let text = String::from_utf8_lossy(line);
        if line[0] == b' ' || line[0] == b'\t' {
            let last = fields.last_mut()?;
            last.1.push(' ');
            last.1.push_str(text.trim());
            continue;
        }
        let colon = text.find(':')?;
        let name = &text[..colon];
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_graphic()) {
            return None;
        }
        fields.push((
            name.to_ascii_lowercase(),
            text[colon + 1..].trim().to_string(),
        ));
    }
    None
}

/// The value of the first of `fields` (a header block's, or a
/// `Content-Type`'s parameters) named `name` (lowercase).
fn field(fields: &[(String, String)], name: &str) -> Option<String> {
    fields
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.clone())
}

/// A `Content-Type`'s type and subtype, lowercase, without parameters.
fn essence(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// The message's `Content-Type`, when it is `multipart/related`.
fn related_type(fields: &[(String, String)]) -> Option<String> {
    let t = field(fields, "content-type")?;
    (essence(&t) == "multipart/related").then_some(t)
}

/// A `Content-Type`'s parameters (`name=value` after its `;`s, a quoted
/// value unquoted), names lowercase.
fn type_params(content_type: &str) -> Fields {
    let mut out = Vec::new();
    let mut rest = match content_type.find(';') {
        Some(i) => &content_type[i + 1..],
        None => return out,
    };
    loop {
        rest = rest.trim_start_matches(|c: char| c == ';' || c.is_ascii_whitespace());
        let Some(eq) = rest.find('=') else {
            break;
        };
        let name = rest[..eq].trim().to_ascii_lowercase();
        let value_at = rest[eq + 1..].trim_start();
        let (value, after) = match value_at.strip_prefix('"') {
            Some(q) => match q.find('"') {
                Some(e) => (q[..e].to_string(), &q[e + 1..]),
                None => (q.to_string(), ""),
            },
            None => {
                let e = value_at.find(';').unwrap_or(value_at.len());
                (value_at[..e].trim().to_string(), &value_at[e..])
            }
        };
        out.push((name, value));
        rest = after;
    }
    out
}

/// A `Content-ID` (or a `start` parameter) without its angle brackets,
/// lowercase.
fn content_id(id: &str) -> String {
    let id = id.trim();
    let id = id.strip_prefix('<').unwrap_or(id);
    let id = id.strip_suffix('>').unwrap_or(id);
    id.to_ascii_lowercase()
}

fn encoding(cte: Option<&str>) -> Encoding {
    match cte.map(|c| c.trim().to_ascii_lowercase()).as_deref() {
        Some("base64") => Encoding::Base64,
        Some("quoted-printable") => Encoding::QuotedPrintable,
        _ => Encoding::Identity,
    }
}

/// Where the next delimiter line `delimiter` (`--boundary`) starts, at or
/// after `from`: at the start of a line.
fn find_delimiter(bytes: &[u8], from: usize, delimiter: &[u8]) -> Option<usize> {
    let mut at = from;
    while at + delimiter.len() <= bytes.len() {
        let i = super::find(&bytes[at..], delimiter)? + at;
        if i == 0 || bytes[i - 1] == b'\n' {
            return Some(i);
        }
        at = i + 1;
    }
    None
}

/// `to` less the line break (CRLF or LF) just before it, not before `from`.
fn trim_line_end(bytes: &[u8], from: usize, to: usize) -> usize {
    let mut end = to;
    if end > from && bytes[end - 1] == b'\n' {
        end -= 1;
        if end > from && bytes[end - 1] == b'\r' {
            end -= 1;
        }
    }
    end
}

fn memchr(b: u8, bytes: &[u8], from: usize) -> Option<usize> {
    bytes
        .get(from..)?
        .iter()
        .position(|&c| c == b)
        .map(|i| from + i)
}

/// Quoted-printable decoded: `=XX` a byte, `=` at a line's end a soft
/// break (removed with its CRLF or LF), any other `=` as it is.
fn quoted_printable(raw: &[u8]) -> Vec<u8> {
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        let c = raw[i];
        if c != b'=' {
            out.push(c);
            i += 1;
            continue;
        }
        // A soft break: `=` then optional white space and the line's end.
        let mut j = i + 1;
        while j < raw.len() && (raw[j] == b' ' || raw[j] == b'\t') {
            j += 1;
        }
        if raw[j..].starts_with(b"\r\n") {
            i = j + 2;
            continue;
        }
        if raw[j..].starts_with(b"\n") || j == raw.len() {
            i = j + 1;
            continue;
        }
        match (
            raw.get(i + 1).and_then(|&a| hex(a)),
            raw.get(i + 2).and_then(|&b| hex(b)),
        ) {
            (Some(a), Some(b)) => {
                out.push(a << 4 | b);
                i += 3;
            }
            _ => {
                out.push(b'=');
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message whose root names an image part `a.png`, base64.
    const MSG: &[u8] = b"Content-Type: multipart/related; boundary=b\n\n\
        --b\nContent-Type: text/html\nContent-Location: http://x/p.html\n\n<p>x</p>\n\
        --b\nContent-Type: image/png\nContent-Location: http://x/a%20b.png\n\
        Content-Transfer-Encoding: base64\n\nTWFu\n--b--\n";

    #[test]
    fn a_part_too_big_for_the_limit_is_not_read_1898() {
        let m = parse(MSG, &Budget::standard()).unwrap();
        assert_eq!(m.location.as_deref(), Some("http://x/p.html"));
        assert_eq!(m.html, b"<p>x</p>");
        // Its decoded bound (3 bytes for 4 of base64, plus slack) passes 3.
        assert_eq!(m.part_for("http://x/a%20b.png", 3), Some(None));
        assert_eq!(
            m.part_for("http://x/a%20b.png", 1 << 20),
            Some(Some(b"Man".to_vec()))
        );
        // Percent-decoded, the location matches too.
        assert_eq!(
            m.part_for("http://x/a b.png", 1 << 20),
            Some(Some(b"Man".to_vec()))
        );
        assert_eq!(m.part_for("http://x/other.png", 1 << 20), None);
    }

    #[test]
    fn a_root_that_is_no_html_is_no_mhtml_1898() {
        let msg = b"Content-Type: multipart/related; boundary=b\n\n--b\nContent-Type: text/plain\n\nhi\n--b--\n";
        assert!(parse(msg, &Budget::standard()).is_none());
        // No boundary.
        assert!(
            parse(
                b"Content-Type: multipart/related\n\n<p>x</p>",
                &Budget::standard()
            )
            .is_none()
        );
    }

    #[test]
    fn quoted_printable_decodes_soft_breaks_and_escapes_1898() {
        let qp = quoted_printable;
        assert_eq!(
            qp(b"a=3Db=\r\nc=\nd= \r\ne=E9=zz="),
            b"a=bcde\xE9=zz".to_vec()
        );
    }
}
