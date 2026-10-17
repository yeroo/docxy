//! A small JSON value with a hand-written parser and serializer: what crosses
//! the plugin ABI ([`crate::plugin_abi`], [`crate::uiplugin`]). projcore
//! takes no serialization crate, as the rest of the workspace does not; the
//! parser is strict (RFC 8259, a nesting limit, surrogate pairs checked) and
//! the serializer compact. Objects keep their key order; a lookup takes the
//! last value of a repeated key, so a later entry overrides an earlier one.

/// A JSON value; the default is `Null`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Json {
    #[default]
    Null,
    Bool(bool),
    /// Every number is an `f64`; [`Json::as_i64`] reads an integral one.
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// An object from `(key, value)` pairs.
    pub fn obj(pairs: Vec<(&str, Json)>) -> Json {
        Json::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    /// A string value.
    pub fn str(s: impl Into<String>) -> Json {
        Json::Str(s.into())
    }

    /// An array of strings.
    pub fn strs<S: AsRef<str>>(items: impl IntoIterator<Item = S>) -> Json {
        Json::Arr(
            items
                .into_iter()
                .map(|s| Json::Str(s.as_ref().to_string()))
                .collect(),
        )
    }

    /// The value of `key` on an object (the last one when repeated).
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
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

    /// The number when it is finite and whole.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Json::Num(n) if n.is_finite() && n.fract() == 0.0 => Some(*n as i64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(pairs) => Some(pairs),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    /// `key`'s string on an object.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Json::as_str)
    }

    /// `key`'s string on an object, or empty.
    pub fn text(&self, key: &str) -> String {
        self.get_str(key).unwrap_or_default().to_string()
    }

    /// `key`'s boolean on an object, false when absent or null.
    pub fn flag(&self, key: &str) -> bool {
        self.get(key).and_then(Json::as_bool).unwrap_or(false)
    }

    /// `key`'s number on an object.
    pub fn num(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Json::as_f64)
    }

    /// `key`'s whole number on an object.
    pub fn int(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(Json::as_i64)
    }

    /// `key`'s array on an object, empty when absent.
    pub fn list(&self, key: &str) -> &[Json] {
        self.get(key).and_then(Json::as_array).unwrap_or(&[])
    }

    /// `key`'s array of strings on an object (non-strings skipped).
    pub fn texts(&self, key: &str) -> Vec<String> {
        self.list(key)
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect()
    }

    /// Parse `text` as one JSON value with nothing after it.
    pub fn parse(text: &str) -> Result<Json, String> {
        let mut parser = Parser {
            s: text.as_bytes(),
            i: 0,
        };
        let value = parser.value(0)?;
        parser.ws();
        if parser.i != parser.s.len() {
            return Err(parser.error("text after the value"));
        }
        Ok(value)
    }

    /// Compact serialization.
    pub fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) => {
                if n.is_finite() {
                    if n.fract() == 0.0 && n.abs() < 1e15 {
                        out.push_str(&format!("{}", *n as i64));
                    } else {
                        out.push_str(&n.to_string());
                    }
                } else {
                    out.push_str("null");
                }
            }
            Json::Str(s) => quote(out, s),
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Json::Obj(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    quote(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

impl std::fmt::Display for Json {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = String::new();
        self.write(&mut out);
        f.write_str(&out)
    }
}

/// Append `s` quoted and escaped.
pub fn quote(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Nesting deeper than this is refused rather than recursed into.
const MAX_DEPTH: usize = 64;

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn error(&self, what: &str) -> String {
        format!("the JSON is malformed at byte {}: {what}", self.i)
    }

    fn ws(&mut self) {
        while self
            .s
            .get(self.i)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.i += 1;
        }
    }

    fn eat(&mut self, word: &str) -> bool {
        let found = self.s[self.i..].starts_with(word.as_bytes());
        if found {
            self.i += word.len();
        }
        found
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        self.ws();
        match self.s.get(self.i) {
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.eat("]") {
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat("]") {
                        return Ok(Json::Arr(items));
                    }
                    if !self.eat(",") {
                        return Err(self.error("expected , or ]"));
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut fields = Vec::new();
                self.ws();
                if self.eat("}") {
                    return Ok(Json::Obj(fields));
                }
                loop {
                    self.ws();
                    if self.s.get(self.i) != Some(&b'"') {
                        return Err(self.error("expected a key"));
                    }
                    let key = self.string()?;
                    self.ws();
                    if !self.eat(":") {
                        return Err(self.error("expected :"));
                    }
                    fields.push((key, self.value(depth + 1)?));
                    self.ws();
                    if self.eat("}") {
                        return Ok(Json::Obj(fields));
                    }
                    if !self.eat(",") {
                        return Err(self.error("expected , or }"));
                    }
                }
            }
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') if self.eat("true") => Ok(Json::Bool(true)),
            Some(b'f') if self.eat("false") => Ok(Json::Bool(false)),
            Some(b'n') if self.eat("null") => Ok(Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected character")),
            None => Err(self.error("unexpected end")),
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.i;
        while self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        self.i - start
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        self.eat("-");
        if self.digits() == 0 {
            return Err(self.error("expected a digit"));
        }
        if self.eat(".") && self.digits() == 0 {
            return Err(self.error("expected a digit"));
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if !self.eat("+") {
                self.eat("-");
            }
            if self.digits() == 0 {
                return Err(self.error("expected a digit"));
            }
        }
        // ASCII digits, sign, point and exponent only: valid UTF-8.
        let text =
            std::str::from_utf8(&self.s[start..self.i]).map_err(|_| self.error("bad number"))?;
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| self.error("bad number"))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let hex = self
            .s
            .get(self.i..self.i + 4)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .ok_or_else(|| self.error("bad \\u escape"))?;
        self.i += 4;
        Ok(hex)
    }

    /// A string starting at its opening quote.
    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(&b) = self.s.get(self.i) else {
                return Err(self.error("unterminated string"));
            };
            self.i += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.s.get(self.i) else {
                        return Err(self.error("unterminated string"));
                    };
                    self.i += 1;
                    let c = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&hi) {
                                if !self.eat("\\u") {
                                    return Err(self.error("unpaired surrogate"));
                                }
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.error("unpaired surrogate"));
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            char::from_u32(code).ok_or_else(|| self.error("unpaired surrogate"))?
                        }
                        _ => return Err(self.error("bad escape")),
                    };
                    let mut utf8 = [0; 4];
                    out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
                }
                b if b < 0x20 => return Err(self.error("control character in a string")),
                b => out.push(b),
            }
        }
        // The input is a `str` and a string ends only at an ASCII quote, so
        // the bytes copied through stay whole UTF-8 sequences.
        String::from_utf8(out).map_err(|_| self.error("invalid UTF-8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_shape_and_keeps_numbers() {
        let v = Json::parse(r#" {"a": [1, -2.5e3, true, null, "xé😀"], "b": {"c": 0}, "a": 7} "#)
            .unwrap();
        // A repeated key reads as its last value; the order is kept.
        assert_eq!(v.int("a"), Some(7));
        assert_eq!(v.get("b").unwrap().int("c"), Some(0));
        let Json::Obj(pairs) = &v else { panic!() };
        assert_eq!(pairs.len(), 3);
        let Json::Arr(items) = &pairs[0].1 else {
            panic!()
        };
        assert_eq!(items[0], Json::Num(1.0));
        assert_eq!(items[1], Json::Num(-2500.0));
        assert_eq!(items[2].as_bool(), Some(true));
        assert!(items[3].is_null());
        assert_eq!(items[4].as_str(), Some("x\u{e9}\u{1F600}"));
        assert_eq!(Json::Num(2.5).as_i64(), None);
        assert_eq!(Json::Num(3.0).as_i64(), Some(3));
    }

    #[test]
    fn serializes_compactly_and_round_trips() {
        let v = Json::obj(vec![
            ("s", Json::str("a\"b\\\n\u{1}")),
            ("n", Json::Num(1.5)),
            ("i", Json::Num(-3.0)),
            ("big", Json::Num(1e20)),
            ("l", Json::strs(["x", "y"])),
            (
                "o",
                Json::obj(vec![("t", Json::Bool(true)), ("z", Json::Null)]),
            ),
        ]);
        let text = v.to_string();
        assert_eq!(
            text,
            r#"{"s":"a\"b\\\n\u0001","n":1.5,"i":-3,"big":100000000000000000000,"l":["x","y"],"o":{"t":true,"z":null}}"#
        );
        assert_eq!(Json::parse(&text).unwrap(), v);
        assert_eq!(Json::Num(f64::NAN).to_string(), "null");
        assert_eq!(Json::Arr(Vec::new()).to_string(), "[]");
    }

    #[test]
    fn accessors_default_on_a_missing_key() {
        let v = Json::parse(r#"{"t":"x","f":true,"l":["a",1,"b"]}"#).unwrap();
        assert_eq!(v.text("t"), "x");
        assert_eq!(v.text("missing"), "");
        assert!(v.flag("f") && !v.flag("missing"));
        assert_eq!(v.texts("l"), ["a", "b"]);
        assert!(v.list("missing").is_empty());
        assert_eq!(v.num("t"), None);
        assert_eq!(Json::Null.get("x"), None);
    }

    #[test]
    fn refuses_malformed_text() {
        let deep = format!("{}{}", "[".repeat(80), "]".repeat(80));
        for text in [
            "",
            "[",
            "[{]",
            r#"{"a" "x"}"#,
            r#"{"a":"x}"#,
            r#"["\q"]"#,
            r#"["\ud800"]"#,
            "[\"a\u{1}\"]",
            "[-]",
            "[1.]",
            "[] x",
            "[tru]",
            deep.as_str(),
        ] {
            let got = Json::parse(text).unwrap_err();
            assert!(got.starts_with("the JSON is malformed"), "{text}: {got}");
        }
    }
}
