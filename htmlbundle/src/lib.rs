//! `htmlbundle` — docxy's **editable HTML** container.
//!
//! `sample.docx` is exported as `sample.docx.html`: one self-contained HTML file
//! that opens offline in a browser as a suite-style editor. The file carries:
//!
//! - the web UI (`web/app.css`, `web/engine.js`, `web/app.js`) and the suite's
//!   ribbon snapshot (`web/ribbon-docx.json`), inlined;
//! - the `docxwasm` engine, base64 in an `application/x-docxy-engine` block;
//! - the **original OOXML package**, base64 in the last element of the file, an
//!   `application/x-docxy-payload` block, after a one-line JSON [`Meta`].
//!
//! Getting a `.docx` back out ([`unwrap`]) is just decoding the payload, so a
//! bundle nobody edited gives back the exact bytes it was made from.
//!
//! ## Rebuild contract
//!
//! The page saves itself by rebuilding its own file. [`wrap`] fills
//! `web/shell.html` in two passes: first the static slots (`{{title}}`,
//! `{{csp}}`), giving the *template*; then the data slots. The template is
//! stored in the file (the `docxy-shell` block, as a JSON string), and every
//! data slot is the raw text of one `<script>`/`<style>` element, so the page
//! can refill the template from `element.textContent` plus a new payload. That
//! rebuild equals [`rewrap`] byte for byte, which holds because:
//!
//! - the file is LF-only (the HTML parser folds CRLF, which would change the
//!   text read back), and
//! - no slot's text contains `</script`, `</style` or `<!--` (checked here),
//!   so the parser's raw-text state reads each element back unchanged.

pub mod base64;
pub mod engine_build;
pub mod sha256;

use std::fmt;

/// The bundle's Content-Security-Policy. `default-src 'none'` with no
/// `connect-src` makes the page provably offline; inline code and the wasm
/// compile are the only allowances.
pub const CSP: &str = "default-src 'none'; script-src 'unsafe-inline' 'wasm-unsafe-eval'; \
style-src 'unsafe-inline'; img-src data: blob:; font-src data:";

/// Opening tag of the payload block. It is always the last element of a
/// bundle, so readers find it with `rfind`.
pub const PAYLOAD_OPEN: &str = "<script type=\"application/x-docxy-payload\" id=\"docxy-payload\">";
const SCRIPT_CLOSE: &str = "</script>";

/// The inlined web UI for a format.
#[derive(Debug, Clone, Copy)]
pub struct Assets<'a> {
    pub shell: &'a str,
    pub css: &'a str,
    pub engine_js: &'a str,
    pub app_js: &'a str,
    pub ribbon_json: &'a str,
}

/// The docx web UI, compiled into this crate.
pub fn docx_assets() -> Assets<'static> {
    Assets {
        shell: include_str!("../web/shell.html"),
        css: include_str!("../web/app.css"),
        engine_js: include_str!("../web/engine.js"),
        app_js: include_str!("../web/app.js"),
        ribbon_json: include_str!("../web/ribbon-docx.json"),
    }
}

/// Why a bundle could not be made or read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The file has no docxy payload block: it is not an editable-HTML bundle.
    NotABundle,
    /// The payload block is present but unreadable.
    Malformed(String),
    /// The payload does not hash to the `payloadSha256` recorded next to it:
    /// the file was damaged or altered outside docxy.
    IntegrityMismatch { expected: String, actual: String },
    /// An inlined asset contains a sequence that would end its element early.
    UnsafeAsset(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotABundle => write!(f, "not a docxy editable HTML file (no payload block)"),
            Error::Malformed(why) => write!(f, "damaged docxy HTML payload: {why}"),
            Error::IntegrityMismatch { expected, actual } => write!(
                f,
                "the embedded document fails its integrity check \
                 (payloadSha256 {expected}, actual {actual}); the file was altered outside docxy"
            ),
            Error::UnsafeAsset(why) => write!(f, "cannot inline web asset: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// Bundle metadata: a flat JSON object of strings, kept in key order so a
/// rewrite changes only the value it means to.
///
/// Known keys: `format`, `sourceName`, `sourceSha256` (the original file at
/// export; never changes), `payloadSha256` (the embedded package; recomputed on
/// every save), `docxyVersion`, `exportedAt`. Unknown keys survive a rewrap.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Meta {
    pub fields: Vec<(String, String)>,
}

impl Meta {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        match self.fields.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value,
            None => self.fields.push((key.to_string(), value)),
        }
    }

    pub fn format(&self) -> &str {
        self.get("format").unwrap_or("")
    }
    pub fn source_name(&self) -> &str {
        self.get("sourceName").unwrap_or("")
    }
    pub fn source_sha256(&self) -> &str {
        self.get("sourceSha256").unwrap_or("")
    }
    pub fn payload_sha256(&self) -> &str {
        self.get("payloadSha256").unwrap_or("")
    }

    /// Serialize as one line of JSON. `<`, `>` and `&` are `\u`-escaped so the
    /// text is inert inside a `<script>` element; `web/engine.js` `metaJson`
    /// mirrors this exactly.
    pub fn to_json(&self) -> String {
        let body: Vec<String> = self
            .fields
            .iter()
            .map(|(k, v)| format!("{}:{}", json_string(k), json_string(v)))
            .collect();
        format!("{{{}}}", body.join(","))
    }

    /// Parse the flat string-valued object [`Meta::to_json`] writes.
    pub fn from_json(text: &str) -> Result<Meta, Error> {
        parse_flat_object(text).map_err(|why| Error::Malformed(format!("metadata: {why}")))
    }
}

/// A decoded bundle: its metadata and the embedded package bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub meta: Meta,
    pub payload: Vec<u8>,
}

/// Build a fresh bundle around `payload` (the original package bytes of
/// `source_name`). `exported_at` is an ISO-8601 UTC time (see
/// [`utc_timestamp`]).
pub fn wrap(
    assets: &Assets<'_>,
    engine_wasm: &[u8],
    format: &str,
    source_name: &str,
    payload: &[u8],
    docxy_version: &str,
    exported_at: &str,
) -> Result<String, Error> {
    let css = normalize(assets.css);
    let engine_js = normalize(assets.engine_js);
    let app_js = normalize(assets.app_js);
    check_raw_text("app.css", &css)?;
    check_raw_text("engine.js", &engine_js)?;
    check_raw_text("app.js", &app_js)?;

    let digest = sha256::hex_digest(payload);
    let meta = Meta {
        fields: vec![
            ("format".into(), format.into()),
            ("sourceName".into(), source_name.into()),
            ("sourceSha256".into(), digest.clone()),
            ("payloadSha256".into(), digest),
            ("docxyVersion".into(), docxy_version.into()),
            ("exportedAt".into(), exported_at.into()),
        ],
    };

    let title = html_escape(source_name);
    let template = fill(&normalize(assets.shell), &[("title", &title), ("csp", CSP)]);
    let shell = json_string(&template);
    let ribbon = inert_json(&normalize(assets.ribbon_json));
    let engine = base64::encode(engine_wasm);
    let payload_text = payload_block_text(&meta, payload);
    let html = fill(
        &template,
        &[
            ("shell", &shell),
            ("css", &css),
            ("ribbon", &ribbon),
            ("engine", &engine),
            ("engine_js", &engine_js),
            ("app_js", &app_js),
            ("payload", &payload_text),
        ],
    );
    // Readers find the payload with `rfind`, so it must be the file's last
    // element (a marker spelled earlier, in a script, is then harmless).
    let (start, end) = payload_span(&html)?;
    if html[start..end] != payload_text || html[end..].contains("<script") {
        return Err(Error::UnsafeAsset(
            "shell.html must end with the payload block".into(),
        ));
    }
    Ok(html)
}

/// Read the embedded package out of a bundle, verifying `payloadSha256`.
pub fn unwrap(html: &str) -> Result<Bundle, Error> {
    let (start, end) = payload_span(html)?;
    let text = &html[start..end];
    let mut lines = text.trim().splitn(2, '\n');
    let meta = Meta::from_json(lines.next().unwrap_or(""))?;
    let body = lines.next().unwrap_or("");
    let payload =
        base64::decode(body).ok_or_else(|| Error::Malformed("payload is not base64".into()))?;
    let actual = sha256::hex_digest(&payload);
    if meta.payload_sha256() != actual {
        return Err(Error::IntegrityMismatch {
            expected: meta.payload_sha256().to_string(),
            actual,
        });
    }
    Ok(Bundle { meta, payload })
}

/// Replace a bundle's payload with `payload`, keeping everything else — the
/// engine, the web UI, `sourceSha256`, the export time — byte for byte. This is
/// how docxy and the suite save an opened `.docx.html`, and needs no engine.
pub fn rewrap(old_html: &str, payload: &[u8]) -> Result<String, Error> {
    let mut meta = unwrap(old_html)?.meta;
    meta.set("payloadSha256", sha256::hex_digest(payload));
    let (start, end) = payload_span(old_html)?;
    let mut out = String::with_capacity(old_html.len() + payload.len() / 2);
    out.push_str(&old_html[..start]);
    out.push_str(&payload_block_text(&meta, payload));
    out.push_str(&old_html[end..]);
    Ok(out)
}

/// The text inside the payload element: a newline, the meta line, a newline,
/// the base64 package, a newline.
fn payload_block_text(meta: &Meta, payload: &[u8]) -> String {
    format!("\n{}\n{}\n", meta.to_json(), base64::encode(payload))
}

/// Byte range of the payload element's text.
fn payload_span(html: &str) -> Result<(usize, usize), Error> {
    let open = html.rfind(PAYLOAD_OPEN).ok_or(Error::NotABundle)?;
    let start = open + PAYLOAD_OPEN.len();
    let len = html[start..]
        .find(SCRIPT_CLOSE)
        .ok_or_else(|| Error::Malformed("payload block is not closed".into()))?;
    Ok((start, start + len))
}

/// Single-pass `{{name}}` substitution. A `{{` not followed by a known
/// `name}}` is copied through, and substituted text is never rescanned, so a
/// value containing `{{css}}` stays literal. `web/engine.js` `fillTemplate`
/// mirrors this exactly.
fn fill(template: &str, slots: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let hit = after.find("}}").and_then(|j| {
            let name = &after[..j];
            slots.iter().find(|(k, _)| *k == name).map(|(_, v)| (j, *v))
        });
        match hit {
            Some((j, value)) => {
                out.push_str(value);
                rest = &after[j + 2..];
            }
            None => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// LF-only line endings (a CRLF checkout of the web assets must not leak into
/// the file, see the module note).
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Refuse raw-text content the HTML parser would not read back verbatim.
fn check_raw_text(name: &str, text: &str) -> Result<(), Error> {
    let lower = text.to_ascii_lowercase();
    for bad in ["</script", "</style", "<!--"] {
        if lower.contains(bad) {
            return Err(Error::UnsafeAsset(format!("{name} contains `{bad}`")));
        }
    }
    Ok(())
}

/// JSON text made inert inside `<script>`: `<` only occurs inside JSON
/// strings, where `<` means the same thing.
fn inert_json(json: &str) -> String {
    json.replace('<', "\\u003c")
}

/// A JSON string literal with `<`, `>`, `&`, U+2028 and U+2029 escaped.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32))
            }
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            // The escaped name is filled into the template before the data
            // slots are; a literal `{{payload}}` in it must not become a slot.
            '{' => out.push_str("&#123;"),
            c => out.push(c),
        }
    }
    out
}

/// Parse `{"k":"v",...}` with string values only.
fn parse_flat_object(text: &str) -> Result<Meta, String> {
    let mut p = text.trim().chars().peekable();
    let mut fields = Vec::new();
    let skip_ws = |p: &mut std::iter::Peekable<std::str::Chars<'_>>| {
        while p.peek().is_some_and(|c| c.is_whitespace()) {
            p.next();
        }
    };
    if p.next() != Some('{') {
        return Err("expected `{`".into());
    }
    skip_ws(&mut p);
    if p.peek() == Some(&'}') {
        p.next();
    } else {
        loop {
            skip_ws(&mut p);
            let key = parse_string(&mut p)?;
            skip_ws(&mut p);
            if p.next() != Some(':') {
                return Err("expected `:`".into());
            }
            skip_ws(&mut p);
            let value = parse_string(&mut p)?;
            fields.push((key, value));
            skip_ws(&mut p);
            match p.next() {
                Some(',') => continue,
                Some('}') => break,
                _ => return Err("expected `,` or `}`".into()),
            }
        }
    }
    skip_ws(&mut p);
    if p.next().is_some() {
        return Err("trailing text".into());
    }
    Ok(Meta { fields })
}

fn parse_string(p: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<String, String> {
    if p.next() != Some('"') {
        return Err("expected a string".into());
    }
    let mut out = String::new();
    loop {
        match p.next().ok_or("unterminated string")? {
            '"' => return Ok(out),
            '\\' => match p.next().ok_or("unterminated escape")? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                'u' => {
                    let hi = parse_hex4(p)?;
                    let c = if (0xD800..0xDC00).contains(&hi) {
                        if p.next() != Some('\\') || p.next() != Some('u') {
                            return Err("lone surrogate".into());
                        }
                        let lo = parse_hex4(p)?;
                        if !(0xDC00..0xE000).contains(&lo) {
                            return Err("bad surrogate pair".into());
                        }
                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                    } else {
                        hi
                    };
                    out.push(char::from_u32(c).ok_or("bad \\u escape")?);
                }
                _ => return Err("bad escape".into()),
            },
            c => out.push(c),
        }
    }
}

fn parse_hex4(p: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Result<u32, String> {
    let mut v = 0;
    for _ in 0..4 {
        let d = p
            .next()
            .and_then(|c| c.to_digit(16))
            .ok_or("bad \\u escape")?;
        v = v * 16 + d;
    }
    Ok(v)
}

// ---- file names ------------------------------------------------------------

/// `sample.docx` → `sample.docx.html`.
pub fn bundle_name(source: &str) -> String {
    format!("{source}.html")
}

/// The original-format extension inside a bundle name, lowercased:
/// `Report.DOCX.html` → `Some("docx")`. `None` when the name is not
/// `<name>.<ext>.html`.
pub fn bundle_inner_ext(path: &str) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    let stem = lower.strip_suffix(".html")?;
    let file = stem.rsplit(['/', '\\']).next().unwrap_or(stem);
    let (name, ext) = file.rsplit_once('.')?;
    (!name.is_empty() && !ext.is_empty()).then(|| ext.to_string())
}

/// Whether `path` is an HTML file name (`.html`/`.htm`, any case). To docxy
/// such a path is only ever a bundle: it is opened by its *content* (the
/// payload block and its `format`), never as a plain package, and saving to it
/// always writes a bundle. The `<name>.docx.html` convention is only a hint:
/// browsers save `sample.docx (1).html`, and people pick `notes.html`.
pub fn is_html_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".html") || lower.ends_with(".htm")
}

/// `sample.docx.html` → `sample.docx` (the path the bundle was exported from,
/// by the naming convention). `None` when `path` is not `<name>.<ext>.html`.
pub fn source_name(path: &str) -> Option<&str> {
    bundle_inner_ext(path)?;
    Some(&path[..path.len() - ".html".len()])
}

/// The original-file name a new bundle saved as `file` records: the name
/// without `.html`/`.htm`, as a `.docx` (`notes.html` → `notes.docx`,
/// `sample.docx.html` → `sample.docx`).
pub fn docx_source_name(file: &str) -> String {
    let lower = file.to_ascii_lowercase();
    let cut = if lower.ends_with(".html") {
        file.len() - 5
    } else if lower.ends_with(".htm") {
        file.len() - 4
    } else {
        file.len()
    };
    let stem = &file[..cut];
    if stem.to_ascii_lowercase().ends_with(".docx") {
        stem.to_string()
    } else {
        format!("{stem}.docx")
    }
}

/// The "changed since export" check: when the file a bundle was exported
/// from (its recorded `sourceName`) still sits in the bundle's folder and no
/// longer hashes to `sourceSha256`, a warning naming it. The bundle's own name
/// does not matter (`sample.docx (1).html` still finds `sample.docx`), and only
/// the recorded name's last component is used, so it cannot point elsewhere.
/// Informational only: the sibling is hashed, never written.
pub fn sibling_warning(bundle_path: &std::path::Path, meta: &Meta) -> Option<String> {
    let name = std::path::Path::new(meta.source_name()).file_name()?;
    let sibling = bundle_path.parent()?.join(name);
    if sibling == bundle_path {
        return None;
    }
    let bytes = std::fs::read(&sibling).ok()?;
    if meta.source_sha256().is_empty() || sha256::hex_digest(&bytes) == meta.source_sha256() {
        return None;
    }
    let name = sibling
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| sibling.display().to_string());
    Some(format!(
        "{name} changed since export; this file keeps its own copy"
    ))
}

/// Seconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn utc_timestamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// The current time as [`utc_timestamp`].
pub fn now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    utc_timestamp(secs)
}

#[cfg(test)]
mod tests;
