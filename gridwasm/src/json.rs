//! A minimal JSON string emitter — just enough to serialize the render output
//! for the webview. In the spirit of the rest of the stack (hand-written ZIP,
//! XML, PDF), we don't pull a serialization crate for a handful of shapes.
//!
//! We only ever *emit* JSON here; the command channel from the webview uses a
//! trivial tab-delimited protocol (see `bridge::dispatch`), so no JSON *parser*
//! is needed on the Rust side — until `grid_ctl` (Task 3), which does need one;
//! see the module note below `quote`/`push_num`.

/// Append `s` as a quoted, escaped JSON string to `out` (including the quotes).
pub fn push_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Everything below 0x20 must be escaped in JSON; emit the \u form.
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The quoted, escaped JSON form of `s` as an owned `String`.
// Unused for now (only `push_str` is needed by Task 1's `view_json`); kept for
// parity with `docxwasm::json` and for later tasks that build one-off strings.
#[allow(dead_code)]
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    push_str(&mut out, s);
    out
}

/// Append a JSON number to `out`, matching `ctlcore::json::Json`'s writer
/// exactly (integers print without a trailing `.0`; non-finite values — which
/// a cell should never hold, since formula errors are `CellValue::Error`
/// strings — fall back to `null`, since JSON has no NaN/Infinity). Task 3's
/// `grid_ctl` (see `bridge::Session::ctl`) needs this for `cell.get` /
/// `sheet.read`'s `"value"` field to match xlsxy's control reply shape
/// byte-for-byte.
pub fn push_num(out: &mut String, n: f64) {
    use std::fmt::Write;
    if n.is_finite() {
        if n.fract() == 0.0 && n.abs() < 1e15 {
            let _ = write!(out, "{}", n as i64);
        } else {
            let _ = write!(out, "{n}");
        }
    } else {
        out.push_str("null");
    }
}

// ---------------------------------------------------------------------------
// Parsing — `grid_ctl` requests
// ---------------------------------------------------------------------------
//
// `grid_ctl` (see `bridge::Session::ctl`) takes the same JSON the host<->agent
// wire carries (`{"verb":…,"args":{…}}`), so unlike the rest of this module
// (which only *emits*), this side must also *parse*. `gridwasm` cannot depend
// on `ctlcore`, so the value type and its minimal recursive-descent parser are
// copied here from `ctlcore/src/json.rs` (same author, same license, std-only,
// ~200 lines) — copied rather than shared so every layer still speaks plain
// JSON without introducing a crate dependency. Only what `ctl` needs is kept:
// the `Json` value, `Json::parse`, and read accessors; the writer/`Display`
// side of `ctlcore::json::Json` is intentionally omitted — this module keeps
// using `push_str`/`quote`/`push_num` above for output, exactly as before.

/// A parsed JSON value (see the module-level note on where this comes from).
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// All JSON numbers are held as `f64`; integer accessors round-trip exact
    /// values up to 2^53, which is ample for indices and ids.
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    /// Order-preserving object. Duplicate keys are possible in malformed input;
    /// [`Json::get`] returns the first match.
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Look up a key on an object; `None` for non-objects or missing keys.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The raw `f64` of a JSON number (no integrality requirement, unlike
    /// [`Json::as_i64`]) — needed by Task 4's `col.width`, whose `width` is a
    /// fractional Excel column-width unit. Copied from `ctlcore::json::Json`
    /// (see the module note above `Json`), which already has it.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// The elements of a JSON array; `None` for non-arrays. Needed by Task 6's
    /// verbs that take structured array args (`range.set`'s `rows`,
    /// `sheet.pivot`'s `rows`/`cols`/`values`).
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }

    /// The number as an `i64`, but only when it is finite and integral.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(n) if n.is_finite() && n.fract() == 0.0 => Some(*n as i64),
            _ => None,
        }
    }

    /// The number as a `usize`, but only when it is a non-negative integer.
    pub fn as_usize(&self) -> Option<usize> {
        self.as_i64().filter(|n| *n >= 0).map(|n| n as usize)
    }

    // Unused for now — none of the current `ctl` verbs take a boolean arg —
    // kept for parity with `ctlcore::json::Json`'s accessor set and for a
    // later verb that needs one (e.g. `find`'s `case_sensitive`, which
    // `docxwasm`'s `doc.find` already uses).
    #[allow(dead_code)]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Convenience: `obj.get(key).and_then(as_str)`.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Json::as_str)
    }

    /// Convenience: `obj.get(key).and_then(as_usize)`.
    pub fn get_usize(&self, key: &str) -> Option<usize> {
        self.get(key).and_then(Json::as_usize)
    }

    /// Parse a complete JSON document, rejecting trailing junk.
    pub fn parse(s: &str) -> Result<Json, String> {
        let mut p = Parser {
            b: s.as_bytes(),
            i: 0,
        };
        p.ws();
        let v = p.value()?;
        p.ws();
        if p.i != p.b.len() {
            return Err(format!("trailing characters at byte {}", p.i));
        }
        Ok(v)
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.b.get(self.i) {
            None => Err("unexpected end of input".into()),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(&c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(&c) => Err(format!("unexpected byte '{}' at {}", c as char, self.i)),
        }
    }

    fn literal(&mut self, word: &str, val: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(val)
        } else {
            Err(format!("invalid literal at byte {}", self.i))
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        while self.i < self.b.len()
            && matches!(
                self.b[self.i],
                b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-'
            )
        {
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| format!("invalid number '{text}'"))
    }

    fn string(&mut self) -> Result<String, String> {
        // Caller guarantees the current byte is '"'.
        self.i += 1;
        let mut out = String::new();
        loop {
            let c = *self.b.get(self.i).ok_or("unterminated string")?;
            self.i += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or("unterminated escape")?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return Err(format!("bad escape '\\{}'", e as char)),
                    }
                }
                // A raw UTF-8 byte: collect the whole sequence.
                _ => {
                    let seq_start = self.i - 1;
                    let len = utf8_len(c);
                    self.i = seq_start + len;
                    let slice = self
                        .b
                        .get(seq_start..self.i)
                        .ok_or("truncated utf-8 in string")?;
                    out.push_str(std::str::from_utf8(slice).map_err(|e| e.to_string())?);
                }
            }
        }
    }

    /// Parse the four hex digits of a `\uXXXX` escape, joining surrogate pairs.
    fn unicode_escape(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        if (0xD800..=0xDBFF).contains(&hi) {
            // High surrogate: a low surrogate must follow.
            if self.b.get(self.i) == Some(&b'\\') && self.b.get(self.i + 1) == Some(&b'u') {
                self.i += 2;
                let lo = self.hex4()?;
                let c = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                return char::from_u32(c).ok_or_else(|| "invalid surrogate pair".into());
            }
            return Err("lone high surrogate".into());
        }
        char::from_u32(hi).ok_or_else(|| "invalid unicode escape".into())
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let s = self
            .b
            .get(self.i..self.i + 4)
            .ok_or("truncated \\u escape")?;
        let text = std::str::from_utf8(s).map_err(|e| e.to_string())?;
        let v = u32::from_str_radix(text, 16).map_err(|_| "bad hex in \\u escape")?;
        self.i += 4;
        Ok(v)
    }

    fn array(&mut self) -> Result<Json, String> {
        self.i += 1; // '['
        let mut items = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b']') {
            self.i += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(items));
                }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.i)),
            }
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.i += 1; // '{'
        let mut pairs = Vec::new();
        self.ws();
        if self.b.get(self.i) == Some(&b'}') {
            self.i += 1;
            return Ok(Json::Obj(pairs));
        }
        loop {
            self.ws();
            if self.b.get(self.i) != Some(&b'"') {
                return Err(format!("expected object key at byte {}", self.i));
            }
            let key = self.string()?;
            self.ws();
            if self.b.get(self.i) != Some(&b':') {
                return Err(format!("expected ':' at byte {}", self.i));
            }
            self.i += 1;
            let val = self.value()?;
            pairs.push((key, val));
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(pairs));
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.i)),
            }
        }
    }
}

/// The byte length of a UTF-8 sequence given its lead byte.
fn utf8_len(lead: u8) -> usize {
    if lead < 0x80 {
        1
    } else if lead >> 5 == 0b110 {
        2
    } else if lead >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_control_and_quotes() {
        assert_eq!(quote("a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(quote("line\nbreak\ttab"), r#""line\nbreak\ttab""#);
        assert_eq!(quote("\u{1}"), "\"\\u0001\""); // control char -> \\u0001
        assert_eq!(quote("héllo"), "\"héllo\""); // non-ASCII passes through as UTF-8
    }

    #[test]
    fn parses_a_ctl_request_shape() {
        let v =
            Json::parse(r#"{"verb":"cell.set","args":{"ref":"B4","text":"hi","modified":true}}"#)
                .unwrap();
        assert_eq!(v.get_str("verb"), Some("cell.set"));
        let args = v.get("args").unwrap();
        assert_eq!(args.get_str("ref"), Some("B4"));
        assert_eq!(args.get_str("text"), Some("hi"));
        assert_eq!(args.get("modified").unwrap().as_bool(), Some(true));
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(Json::parse("{").is_err());
        assert!(Json::parse("1 2").is_err());
    }

    #[test]
    fn push_num_matches_ctlcore_writer() {
        let mut out = String::new();
        push_num(&mut out, 30.0);
        assert_eq!(out, "30");
        let mut out = String::new();
        push_num(&mut out, 3.75);
        assert_eq!(out, "3.75");
        let mut out = String::new();
        push_num(&mut out, f64::NAN);
        assert_eq!(out, "null");
    }
}
