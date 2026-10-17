//! The smallest JSON reader the UI symbols need, written here so the fixture
//! stays dependency-free (a plugin in any language brings its own).

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn num(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn list(&self) -> &[Json] {
        match self {
            Json::Arr(items) => items,
            _ => &[],
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    pub fn parse(text: &str) -> Result<Json, String> {
        let mut p = Parser {
            s: text.as_bytes(),
            i: 0,
        };
        let v = p.value()?;
        p.ws();
        if p.i != p.s.len() {
            return Err("text after the value".into());
        }
        Ok(v)
    }
}

/// `s` quoted and escaped for JSON.
pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
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
    out
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
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

    fn value(&mut self) -> Result<Json, String> {
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
                    items.push(self.value()?);
                    self.ws();
                    if self.eat("]") {
                        return Ok(Json::Arr(items));
                    }
                    if !self.eat(",") {
                        return Err("expected , or ]".into());
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
                        return Err("expected a key".into());
                    }
                    let key = self.string()?;
                    self.ws();
                    if !self.eat(":") {
                        return Err("expected :".into());
                    }
                    fields.push((key, self.value()?));
                    self.ws();
                    if self.eat("}") {
                        return Ok(Json::Obj(fields));
                    }
                    if !self.eat(",") {
                        return Err("expected , or }".into());
                    }
                }
            }
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') if self.eat("true") => Ok(Json::Bool(true)),
            Some(b'f') if self.eat("false") => Ok(Json::Bool(false)),
            Some(b'n') if self.eat("null") => Ok(Json::Null),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.i;
                while self.s.get(self.i).is_some_and(|b| {
                    b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E')
                }) {
                    self.i += 1;
                }
                std::str::from_utf8(&self.s[start..self.i])
                    .ok()
                    .and_then(|t| t.parse::<f64>().ok())
                    .map(Json::Num)
                    .ok_or_else(|| "bad number".to_string())
            }
            _ => Err("unexpected character".into()),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(&b) = self.s.get(self.i) else {
                return Err("unterminated string".into());
            };
            self.i += 1;
            match b {
                b'"' => break,
                b'\\' => {
                    let Some(&e) = self.s.get(self.i) else {
                        return Err("unterminated string".into());
                    };
                    self.i += 1;
                    match e {
                        b'"' | b'\\' | b'/' => out.push(e),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let hex = self
                                .s
                                .get(self.i..self.i + 4)
                                .and_then(|h| std::str::from_utf8(h).ok())
                                .and_then(|h| u32::from_str_radix(h, 16).ok())
                                .ok_or("bad escape")?;
                            self.i += 4;
                            let c = char::from_u32(hex).unwrap_or('\u{FFFD}');
                            let mut utf8 = [0; 4];
                            out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
                        }
                        _ => return Err("bad escape".into()),
                    }
                }
                b => out.push(b),
            }
        }
        String::from_utf8(out).map_err(|_| "invalid UTF-8".to_string())
    }
}
