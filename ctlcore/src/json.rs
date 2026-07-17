//! A minimal JSON value type with a hand-written parser and serializer.
//!
//! In the spirit of the rest of this workspace (hand-written ZIP, XML, PDF, and
//! the docxwasm emitter) we don't pull a serialization crate for the handful of
//! shapes the control protocol needs. Unlike `docxwasm::json` — which only
//! *emits* — the control channel must also *parse* incoming requests, so this
//! module carries a small recursive-descent parser too.
//!
//! Objects preserve insertion order (a `Vec` of pairs), which keeps emitted
//! output stable and readable; lookups are linear, which is fine for the tiny
//! objects a control request carries.

use std::fmt::Write as _;

/// A parsed JSON value.
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

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
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

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(a) => Some(a),
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

    /// Build an object from owned pairs.
    pub fn obj(pairs: Vec<(&str, Json)>) -> Json {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Num(n) => {
                if n.is_finite() {
                    // Emit integers without a trailing ".0".
                    if n.fract() == 0.0 && n.abs() < 1e15 {
                        let _ = write!(out, "{}", *n as i64);
                    } else {
                        let _ = write!(out, "{n}");
                    }
                } else {
                    // JSON has no NaN/Infinity; null is the conventional stand-in.
                    out.push_str("null");
                }
            }
            Json::Str(s) => write_escaped(out, s),
            Json::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Json::Obj(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_escaped(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
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

/// Compact JSON serialization (no insignificant whitespace). `to_string()` comes
/// free from this via the standard `ToString` blanket impl.
impl std::fmt::Display for Json {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = String::new();
        self.write(&mut out);
        f.write_str(&out)
    }
}

/// Append `s` as a quoted, escaped JSON string (including the quotes).
fn write_escaped(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
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
            && matches!(self.b[self.i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
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
    fn round_trips_scalars_and_containers() {
        for src in [
            "null",
            "true",
            "false",
            "0",
            "-12",
            "3.5",
            "\"hi\"",
            "[]",
            "{}",
            "[1,2,3]",
            "{\"a\":1,\"b\":[true,null]}",
        ] {
            let v = Json::parse(src).expect(src);
            assert_eq!(v.to_string(), src, "round-trip of {src}");
        }
    }

    #[test]
    fn parses_and_reemits_escapes() {
        let v = Json::parse("\"a\\nb\\t\\\"c\\\"\"").unwrap();
        assert_eq!(v.as_str(), Some("a\nb\t\"c\""));
        assert_eq!(v.to_string(), "\"a\\nb\\t\\\"c\\\"\"");
    }

    #[test]
    fn parses_unicode_and_surrogate_pairs() {
        assert_eq!(Json::parse("\"\\u00e9\"").unwrap().as_str(), Some("é"));
        // U+1F600 GRINNING FACE as a surrogate pair.
        assert_eq!(Json::parse("\"\\ud83d\\ude00\"").unwrap().as_str(), Some("😀"));
    }

    #[test]
    fn preserves_raw_utf8_in_strings() {
        let v = Json::parse("\"café — 世界\"").unwrap();
        assert_eq!(v.as_str(), Some("café — 世界"));
    }

    #[test]
    fn typed_accessors() {
        let v = Json::parse("{\"n\":42,\"neg\":-1,\"f\":2.5,\"s\":\"x\",\"b\":true}").unwrap();
        assert_eq!(v.get_usize("n"), Some(42));
        assert_eq!(v.get("neg").unwrap().as_i64(), Some(-1));
        assert_eq!(v.get("neg").unwrap().as_usize(), None);
        assert_eq!(v.get("f").unwrap().as_usize(), None);
        assert_eq!(v.get_str("s"), Some("x"));
        assert_eq!(v.get("b").unwrap().as_bool(), Some(true));
    }

    #[test]
    fn rejects_trailing_junk_and_bad_input() {
        assert!(Json::parse("1 2").is_err());
        assert!(Json::parse("{").is_err());
        assert!(Json::parse("[1,]").is_err());
        assert!(Json::parse("").is_err());
    }

    #[test]
    fn ignores_insignificant_whitespace() {
        let v = Json::parse("  {  \"a\" : [ 1 , 2 ]  }  ").unwrap();
        assert_eq!(v.to_string(), "{\"a\":[1,2]}");
    }
}
