//! Hand-rolled JSON parser and serializer.
//!
//! No `serde`, no external deps — keeps the dependency tree small, in the
//! same spirit as the repo's from-scratch XML/ZIP code. Used for OAuth
//! token responses, Microsoft Graph REST bodies, and outbox payloads.

use std::fmt;

/// A parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

/// A parse error: a human-readable message plus the byte offset into the
/// input where the problem was detected.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonError {
    pub msg: String,
    pub pos: usize,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.msg, self.pos)
    }
}

impl std::error::Error for JsonError {}

impl fmt::Display for Value {
    /// Serializes to compact JSON. Also provides `.to_string()` via the
    /// standard blanket `ToString` impl.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        self.write_to(&mut out);
        f.write_str(&out)
    }
}

impl Value {
    /// If this is an `Object`, returns the value of the first entry matching `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// If this is a `Str`, returns its contents.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// If this is a `Num` representing an integral value, returns it as `i64`.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Num(n) => Some(*n as i64),
            _ => None,
        }
    }

    /// If this is a `Bool`, returns it.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// If this is an `Array`, returns its elements as a slice.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    fn write_to(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Num(n) => {
                if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e18 {
                    out.push_str(&format!("{}", *n as i64));
                } else {
                    out.push_str(&format!("{n}"));
                }
            }
            Value::Str(s) => write_escaped_string(s, out),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write_to(out);
                }
                out.push(']');
            }
            Value::Object(entries) => {
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_escaped_string(k, out);
                    out.push(':');
                    v.write_to(out);
                }
                out.push('}');
            }
        }
    }
}

fn write_escaped_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Maximum nesting depth (objects/arrays inside objects/arrays) `parse` will
/// descend before giving up. Guards against stack overflow on maliciously
/// or accidentally deeply-nested input (this module parses external
/// OAuth/Graph responses) — a `JsonError` is returned instead of a crash.
const MAX_DEPTH: usize = 128;

/// Parses a JSON document from `input`.
pub fn parse(input: &str) -> Result<Value, JsonError> {
    let bytes = input.as_bytes();
    let mut p = Parser { bytes, pos: 0 };
    p.skip_ws();
    let value = p.parse_value(0)?;
    p.skip_ws();
    if p.pos != bytes.len() {
        return Err(p.err("trailing data after JSON value"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, msg: &str) -> JsonError {
        JsonError {
            msg: msg.to_string(),
            pos: self.pos,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn expect_byte(&mut self, b: u8) -> Result<(), JsonError> {
        if self.peek() == Some(b) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected '{}'", b as char)))
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Value, JsonError> {
        if depth > MAX_DEPTH {
            return Err(self.err("maximum nesting depth exceeded"));
        }
        self.skip_ws();
        match self.peek() {
            None => Err(self.err("unexpected end of input")),
            Some(b'{') => self.parse_object(depth),
            Some(b'[') => self.parse_array(depth),
            Some(b'"') => Ok(Value::Str(self.parse_string()?)),
            Some(b't') => self.parse_literal("true", Value::Bool(true)),
            Some(b'f') => self.parse_literal("false", Value::Bool(false)),
            Some(b'n') => self.parse_literal("null", Value::Null),
            Some(b) if b == b'-' || b.is_ascii_digit() => self.parse_number(),
            Some(b) => Err(self.err(&format!("unexpected byte '{}'", b as char))),
        }
    }

    fn parse_literal(&mut self, lit: &str, value: Value) -> Result<Value, JsonError> {
        let start = self.pos;
        let end = start + lit.len();
        if end <= self.bytes.len() && &self.bytes[start..end] == lit.as_bytes() {
            self.pos = end;
            Ok(value)
        } else {
            Err(self.err(&format!("expected '{lit}'")))
        }
    }

    fn parse_number(&mut self) -> Result<Value, JsonError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let digits_start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.pos == digits_start {
            return Err(self.err("invalid number: missing digits"));
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            let frac_start = self.pos;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
            if self.pos == frac_start {
                return Err(self.err("invalid number: missing fraction digits"));
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            let exp_start = self.pos;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
            if self.pos == exp_start {
                return Err(self.err("invalid number: missing exponent digits"));
            }
        }
        let span = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| self.err("invalid number: not utf-8"))?;
        let n: f64 = span
            .parse()
            .map_err(|_| self.err("invalid number: could not parse"))?;
        Ok(Value::Num(n))
    }

    fn parse_string(&mut self) -> Result<String, JsonError> {
        self.expect_byte(b'"')?;
        let mut s = String::new();
        loop {
            match self.peek() {
                None => return Err(self.err("unterminated string")),
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(s);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(b'"') => {
                            s.push('"');
                            self.pos += 1;
                        }
                        Some(b'\\') => {
                            s.push('\\');
                            self.pos += 1;
                        }
                        Some(b'/') => {
                            s.push('/');
                            self.pos += 1;
                        }
                        Some(b'b') => {
                            s.push('\u{08}');
                            self.pos += 1;
                        }
                        Some(b'f') => {
                            s.push('\u{0C}');
                            self.pos += 1;
                        }
                        Some(b'n') => {
                            s.push('\n');
                            self.pos += 1;
                        }
                        Some(b'r') => {
                            s.push('\r');
                            self.pos += 1;
                        }
                        Some(b't') => {
                            s.push('\t');
                            self.pos += 1;
                        }
                        Some(b'u') => {
                            self.pos += 1;
                            let cp = self.parse_hex4()?;
                            if (0xD800..=0xDBFF).contains(&cp) {
                                // High surrogate: expect a low surrogate next.
                                if self.peek() == Some(b'\\')
                                    && self.bytes.get(self.pos + 1) == Some(&b'u')
                                {
                                    self.pos += 2;
                                    let low = self.parse_hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&low) {
                                        return Err(self.err("invalid low surrogate"));
                                    }
                                    let c = 0x10000
                                        + (((cp - 0xD800) as u32) << 10)
                                        + (low - 0xDC00) as u32;
                                    let c = char::from_u32(c)
                                        .ok_or_else(|| self.err("invalid surrogate pair"))?;
                                    s.push(c);
                                } else {
                                    return Err(self.err("unpaired high surrogate"));
                                }
                            } else if (0xDC00..=0xDFFF).contains(&cp) {
                                return Err(self.err("unpaired low surrogate"));
                            } else {
                                let c = char::from_u32(cp as u32)
                                    .ok_or_else(|| self.err("invalid unicode escape"))?;
                                s.push(c);
                            }
                        }
                        _ => return Err(self.err("invalid escape sequence")),
                    }
                }
                Some(b) if b < 0x80 => {
                    s.push(b as char);
                    self.pos += 1;
                }
                Some(_) => {
                    // Multi-byte UTF-8 sequence: decode the full char.
                    let rest = std::str::from_utf8(&self.bytes[self.pos..])
                        .map_err(|_| self.err("invalid utf-8"))?;
                    let c = rest
                        .chars()
                        .next()
                        .ok_or_else(|| self.err("invalid utf-8"))?;
                    s.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u16, JsonError> {
        if self.pos + 4 > self.bytes.len() {
            return Err(self.err("truncated unicode escape"));
        }
        let span = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4])
            .map_err(|_| self.err("invalid unicode escape"))?;
        let cp =
            u16::from_str_radix(span, 16).map_err(|_| self.err("invalid unicode escape"))?;
        self.pos += 4;
        Ok(cp)
    }

    fn parse_array(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.expect_byte(b'[')?;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            let v = self.parse_value(depth + 1)?;
            items.push(v);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_ws();
                    if self.peek() == Some(b']') {
                        return Err(self.err("trailing comma in array"));
                    }
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.err("expected ',' or ']' in array")),
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.expect_byte(b'{')?;
        let mut entries = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Object(entries));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(self.err("expected string key in object"));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect_byte(b':')?;
            let value = self.parse_value(depth + 1)?;
            entries.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_ws();
                    if self.peek() == Some(b'}') {
                        return Err(self.err("trailing comma in object"));
                    }
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(entries));
                }
                _ => return Err(self.err("expected ',' or '}' in object")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars_and_nesting() {
        let v = parse(r#"{"a":1,"b":"x","c":[true,null,2.5],"d":{"e":"f"}}"#).unwrap();
        assert_eq!(v.get("a").unwrap().as_i64(), Some(1));
        assert_eq!(v.get("b").unwrap().as_str(), Some("x"));
        let arr = v.get("c").unwrap().as_array().unwrap();
        assert_eq!(arr[0].as_bool(), Some(true));
        assert!(matches!(arr[1], Value::Null));
        assert_eq!(v.get("d").unwrap().get("e").unwrap().as_str(), Some("f"));
    }

    #[test]
    fn handles_escapes_and_unicode() {
        let v = parse(r#"{"s":"line\nbreak é \"q\""}"#).unwrap();
        assert_eq!(v.get("s").unwrap().as_str(), Some("line\nbreak é \"q\""));
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse(r#"{"a":}"#).is_err());
        assert!(parse(r#"{"a":1,}"#).is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn decodes_unicode_escapes() {
        // Basic BMP escape: é is 'é'. Written as a literal `\u` escape
        // (unlike the existing raw-UTF-8-byte `é` in
        // `handles_escapes_and_unicode`), so this actually exercises
        // `parse_hex4` / the non-surrogate branch.
        let v = parse(r#"{"s":"A\u00e9"}"#).unwrap();
        assert_eq!(v.get("s").unwrap().as_str(), Some("Aé"));

        // Surrogate pair 😀 == U+1F600 'grinning face', outside
        // the BMP — exercises the high/low surrogate combining logic.
        let v = parse(r#"{"s":"\uD83D\uDE00"}"#).unwrap();
        assert_eq!(v.get("s").unwrap().as_str(), Some("😀"));

        // Lone/unpaired surrogates are rejected.
        assert!(parse(r#"{"s":"\uD83D"}"#).is_err());
        assert!(parse(r#"{"s":"\uDE00x"}"#).is_err());
    }

    #[test]
    fn rejects_excessive_nesting() {
        let s = "[".repeat(1000);
        assert!(parse(&s).is_err());
    }

    #[test]
    fn roundtrips_via_to_string() {
        let src = r#"{"a":1,"b":[true,"x"]}"#;
        let v = parse(src).unwrap();
        let out = v.to_string();
        let v2 = parse(&out).unwrap();
        assert_eq!(v2.get("b").unwrap().as_array().unwrap()[1].as_str(), Some("x"));
    }

    #[test]
    fn escapes_output_strings() {
        let v = Value::Str("a\"b\nc".into());
        assert_eq!(v.to_string(), r#""a\"b\nc""#);
    }
}
