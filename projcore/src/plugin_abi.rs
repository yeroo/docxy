//! The C ABI of a project format plugin, version [`ABI_VERSION`].
//!
//! A format plugin is a shared library that reads and/or writes file types
//! the hosts do not know themselves. Hosts load every `yppx_*` library they
//! find (see `formats::plugin`, behind the `plugins` feature) and register
//! each type it declares in their [`crate::formats::Registry`].
//!
//! The interchange across the boundary is **MSPDI XML text**, the model's
//! own faithful serialisation ([`crate::mspdi`]) and the main part of a
//! `.yppx`. No model structs cross the boundary; the ABI is C and tiny:
//!
//! ```c
//! typedef struct { uint8_t* ptr; size_t len; } YppxBuf;
//!
//! uint32_t yppx_plugin_abi(void);                         // returns 1
//! int32_t  yppx_plugin_formats(YppxBuf* out);             // JSON array of {ext, label, read, write, default}
//! int32_t  yppx_plugin_read (const char* ext, const uint8_t* file, size_t len, YppxBuf* mspdi_out);
//! int32_t  yppx_plugin_write(const char* ext, const uint8_t* mspdi, size_t len,
//!                            const uint8_t* source_file, size_t source_len,   // bytes of the file this plan
//!                            YppxBuf* file_out);                               // was opened from, or NULL
//! void     yppx_plugin_free (YppxBuf*);
//! int32_t  yppx_plugin_last_error(YppxBuf* utf8_out);     // message for the last non-zero return
//! ```
//!
//! `YppxBuf` is `{uint8_t* ptr; size_t len;}` owned by the plugin and released
//! with `yppx_plugin_free`. The host passes an empty buffer (`ptr` NULL, `len`
//! 0); a call that fills it hands it to the host, which copies the bytes and
//! frees every buffer whose `ptr` is not NULL, whatever the return code.
//! Return codes: 0 ok ([`OK`]), 1 unsupported extension
//! ([`UNSUPPORTED_EXTENSION`]), 2 malformed input ([`MALFORMED_INPUT`]), 3
//! unsupported content ([`UNSUPPORTED_CONTENT`]: the message names the first
//! field that cannot be written), 4 internal error ([`INTERNAL_ERROR`]). The
//! host never unloads a plugin while a call runs and calls from one thread at
//! a time.
//!
//! - `ext` is the extension the call is for, as the plugin declared it,
//!   lower-case and without the dot, as NUL-terminated UTF-8.
//! - `yppx_plugin_read` turns the bytes of a file into MSPDI XML (UTF-8).
//! - `yppx_plugin_write` turns MSPDI XML into the bytes of a file.
//!   `source_file` is how what the model does not hold survives a save: when
//!   the plan is saved under the extension it was opened from, the host
//!   passes back the bytes of that file, and the plugin may copy whatever it
//!   does not regenerate from the MSPDI. Otherwise it is NULL with
//!   `source_len` 0.
//! - `yppx_plugin_last_error` fills `utf8_out` with a message for the last
//!   non-zero return and returns 0, or returns non-zero when it has none.
//! - `yppx_plugin_formats` fills `out` with the [formats JSON](parse_formats).
//!
//! A host refuses a library whose `yppx_plugin_abi` is not [`ABI_VERSION`],
//! or that lacks any of the six symbols, and keeps every library it loaded
//! for the life of the process.

use std::os::raw::c_char;

/// The ABI version this host speaks: `yppx_plugin_abi` must return it.
pub const ABI_VERSION: u32 = 1;

/// Every plugin library's file name starts with this, before the platform's
/// shared-library extension (`.dll`, `.so`, `.dylib`).
pub const FILE_PREFIX: &str = "yppx_";

/// `uint32_t yppx_plugin_abi(void)`: see [`AbiFn`].
pub const SYMBOL_ABI: &str = "yppx_plugin_abi";
/// `int32_t yppx_plugin_formats(YppxBuf* out)`: see [`FormatsFn`].
pub const SYMBOL_FORMATS: &str = "yppx_plugin_formats";
/// `int32_t yppx_plugin_read(...)`: see [`ReadFn`].
pub const SYMBOL_READ: &str = "yppx_plugin_read";
/// `int32_t yppx_plugin_write(...)`: see [`WriteFn`].
pub const SYMBOL_WRITE: &str = "yppx_plugin_write";
/// `void yppx_plugin_free(YppxBuf*)`: see [`FreeFn`].
pub const SYMBOL_FREE: &str = "yppx_plugin_free";
/// `int32_t yppx_plugin_last_error(YppxBuf* utf8_out)`: see [`LastErrorFn`].
pub const SYMBOL_LAST_ERROR: &str = "yppx_plugin_last_error";

/// The six symbols every plugin exports, in the order of the C declarations.
pub const SYMBOLS: [&str; 6] = [
    SYMBOL_ABI,
    SYMBOL_FORMATS,
    SYMBOL_READ,
    SYMBOL_WRITE,
    SYMBOL_FREE,
    SYMBOL_LAST_ERROR,
];

/// The call succeeded.
pub const OK: i32 = 0;
/// The plugin does not handle the extension it was called for.
pub const UNSUPPORTED_EXTENSION: i32 = 1;
/// The input bytes (a file, or the MSPDI) could not be parsed.
pub const MALFORMED_INPUT: i32 = 2;
/// The input parsed but holds something the plugin cannot represent; the
/// message names the first such field.
pub const UNSUPPORTED_CONTENT: i32 = 3;
/// Anything else went wrong inside the plugin.
pub const INTERNAL_ERROR: i32 = 4;

/// What a return code means, for a message when the plugin gives none.
pub fn code_name(code: i32) -> &'static str {
    match code {
        OK => "ok",
        UNSUPPORTED_EXTENSION => "unsupported extension",
        MALFORMED_INPUT => "malformed input",
        UNSUPPORTED_CONTENT => "unsupported content",
        INTERNAL_ERROR => "internal error",
        _ => "unknown error",
    }
}

/// A byte buffer the plugin owns: `{uint8_t* ptr; size_t len;}`. The host
/// copies it and gives it back through `yppx_plugin_free`.
#[repr(C)]
#[derive(Debug)]
pub struct YppxBuf {
    pub ptr: *mut u8,
    pub len: usize,
}

impl YppxBuf {
    /// The empty buffer the host passes in: `ptr` NULL, `len` 0.
    pub const fn empty() -> YppxBuf {
        YppxBuf {
            ptr: std::ptr::null_mut(),
            len: 0,
        }
    }
}

impl Default for YppxBuf {
    fn default() -> YppxBuf {
        YppxBuf::empty()
    }
}

/// `uint32_t yppx_plugin_abi(void)`: the ABI version the plugin implements.
pub type AbiFn = unsafe extern "C" fn() -> u32;

/// `int32_t yppx_plugin_formats(YppxBuf* out)`: the formats JSON, UTF-8.
pub type FormatsFn = unsafe extern "C" fn(out: *mut YppxBuf) -> i32;

/// `int32_t yppx_plugin_read(const char* ext, const uint8_t* file, size_t len,
/// YppxBuf* mspdi_out)`: a file's bytes to MSPDI XML.
pub type ReadFn = unsafe extern "C" fn(
    ext: *const c_char,
    file: *const u8,
    len: usize,
    mspdi_out: *mut YppxBuf,
) -> i32;

/// `int32_t yppx_plugin_write(const char* ext, const uint8_t* mspdi, size_t
/// len, const uint8_t* source_file, size_t source_len, YppxBuf* file_out)`:
/// MSPDI XML to a file's bytes, with the bytes of the file the plan was
/// opened from, or NULL.
pub type WriteFn = unsafe extern "C" fn(
    ext: *const c_char,
    mspdi: *const u8,
    len: usize,
    source_file: *const u8,
    source_len: usize,
    file_out: *mut YppxBuf,
) -> i32;

/// `void yppx_plugin_free(YppxBuf*)`: release a buffer the plugin filled.
pub type FreeFn = unsafe extern "C" fn(buf: *mut YppxBuf);

/// `int32_t yppx_plugin_last_error(YppxBuf* utf8_out)`: the message for the
/// last non-zero return.
pub type LastErrorFn = unsafe extern "C" fn(utf8_out: *mut YppxBuf) -> i32;

/// One entry of the formats JSON: a file type the plugin reads and/or writes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormatDecl {
    /// The extension, lower-case, without the dot.
    pub ext: String,
    /// The type's name in Open and Save As lists.
    pub label: String,
    /// The plugin reads this type (`yppx_plugin_read`).
    pub read: bool,
    /// The plugin writes this type (`yppx_plugin_write`).
    pub write: bool,
    /// The plugin asks for this type to be the default save type: written,
    /// it is what Save As proposes for a new plan (see
    /// [`crate::formats::Registry::default_save_extension`]).
    pub default: bool,
}

/// Parse the formats JSON `yppx_plugin_formats` returns: an array of
/// objects, one per file type,
///
/// ```json
/// [{"ext":"mpp","label":"Project","read":true,"write":true}]
/// ```
///
/// `ext` (the extension, with or without its dot, any case; letters, digits,
/// `_` and `-`) and `label` (a non-empty name for Open and Save As lists) are
/// required strings; `read`, `write` and `default` are booleans, false when
/// absent. `"default": true` on a type the plugin writes makes it the type
/// Save As proposes for a new plan
/// ([`crate::formats::Registry::default_save_extension`]). Other keys are
/// ignored, so a later ABI can add some.
pub fn parse_formats(json: &str) -> Result<Vec<FormatDecl>, String> {
    let mut parser = Parser {
        s: json.as_bytes(),
        i: 0,
    };
    let value = parser.value(0)?;
    parser.ws();
    if parser.i != parser.s.len() {
        return Err(parser.error("text after the formats array"));
    }
    let Json::Arr(items) = value else {
        return Err("the formats JSON is not an array".into());
    };
    items
        .into_iter()
        .enumerate()
        .map(|(n, item)| decl(n + 1, item))
        .collect()
}

/// The formats JSON for `decls`, as [`parse_formats`] reads it.
pub fn formats_json(decls: &[FormatDecl]) -> String {
    let mut out = String::from("[");
    for (n, d) in decls.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        out.push_str("{\"ext\":");
        quote(&mut out, &d.ext);
        out.push_str(",\"label\":");
        quote(&mut out, &d.label);
        out.push_str(&format!(",\"read\":{},\"write\":{}", d.read, d.write));
        if d.default {
            out.push_str(",\"default\":true");
        }
        out.push('}');
    }
    out.push(']');
    out
}

fn quote(out: &mut String, s: &str) {
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

/// Entry `n` (1-based) of the formats array.
fn decl(n: usize, item: Json) -> Result<FormatDecl, String> {
    let Json::Obj(fields) = item else {
        return Err(format!("format {n} is not an object"));
    };
    let get = |key: &str| fields.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v);
    let text = |key: &str| match get(key) {
        Some(Json::Str(s)) => Ok(s.trim().to_string()),
        Some(_) => Err(format!("format {n}: \"{key}\" is not a string")),
        None => Err(format!("format {n} has no \"{key}\"")),
    };
    let flag = |key: &str| match get(key) {
        Some(Json::Bool(b)) => Ok(*b),
        None | Some(Json::Null) => Ok(false),
        Some(_) => Err(format!("format {n}: \"{key}\" is not true or false")),
    };
    let raw = text("ext")?;
    let ext = raw.strip_prefix('.').unwrap_or(&raw).to_ascii_lowercase();
    if ext.is_empty()
        || !ext
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!("format {n}: \"{raw}\" is not a file extension"));
    }
    let label = text("label")?;
    if label.is_empty() {
        return Err(format!("format {n}: \"label\" is empty"));
    }
    Ok(FormatDecl {
        ext,
        label,
        read: flag("read")?,
        write: flag("write")?,
        default: flag("default")?,
    })
}

/// A parsed JSON value; numbers are checked, not kept.
enum Json {
    Null,
    Bool(bool),
    Num,
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

/// Nesting deeper than this is refused rather than recursed into.
const MAX_DEPTH: usize = 32;

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn error(&self, what: &str) -> String {
        format!("the formats JSON is malformed at byte {}: {what}", self.i)
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
        Ok(Json::Num)
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

    fn decl(ext: &str, label: &str, read: bool, write: bool) -> FormatDecl {
        FormatDecl {
            ext: ext.into(),
            label: label.into(),
            read,
            write,
            default: false,
        }
    }

    #[test]
    fn reads_the_documented_shape() {
        assert_eq!(
            parse_formats(r#"[{"ext":"mpp","label":"Project","read":true,"write":true}]"#).unwrap(),
            vec![decl("mpp", "Project", true, true)]
        );
    }

    #[test]
    fn normalises_extensions_and_defaults_flags() {
        let json = r#" [ {"ext": ".MPX", "label": " Exchange ", "read": true},
            {"ext":"p-6_x","label":"Pé 😀","write":true,"extra":[1,{"a":null},-2.5e3]} ] "#;
        assert_eq!(
            parse_formats(json).unwrap(),
            vec![
                decl("mpx", "Exchange", true, false),
                decl("p-6_x", "P\u{e9} \u{1F600}", false, true),
            ]
        );
        assert_eq!(parse_formats("[]").unwrap(), vec![]);
    }

    #[test]
    fn reads_the_default_flag_and_ignores_unknown_keys() {
        let json = r#"[{"ext":"mpp","label":"Project","read":true,"write":true,"default":true,
                "since":{"abi":2,"note":["later"]}},
            {"ext":"mpx","label":"Exchange","write":true,"default":false,"icon":null},
            {"ext":"p6","label":"P6","read":true,"default":null,"rank":-1.5}]"#;
        assert_eq!(
            parse_formats(json).unwrap(),
            vec![
                FormatDecl {
                    default: true,
                    ..decl("mpp", "Project", true, true)
                },
                decl("mpx", "Exchange", false, true),
                decl("p6", "P6", true, false),
            ]
        );
        let got = parse_formats(r#"[{"ext":"x","label":"X","default":"yes"}]"#).unwrap_err();
        assert!(got.contains("\"default\" is not true or false"), "{got}");
    }

    #[test]
    fn refuses_bad_declarations() {
        for (json, error) in [
            ("{}", "not an array"),
            ("[1]", "format 1 is not an object"),
            (r#"[{"label":"X"}]"#, "format 1 has no \"ext\""),
            (
                r#"[{"ext":"a/b","label":"X"}]"#,
                "\"a/b\" is not a file extension",
            ),
            (r#"[{"ext":".","label":"X"}]"#, "is not a file extension"),
            (r#"[{"ext":"x","label":"  "}]"#, "\"label\" is empty"),
            (r#"[{"ext":"x","label":"X","read":1}]"#, "not true or false"),
            (r#"[{"ext":7,"label":"X"}]"#, "\"ext\" is not a string"),
        ] {
            let got = parse_formats(json).unwrap_err();
            assert!(got.contains(error), "{json}: {got}");
        }
    }

    #[test]
    fn refuses_malformed_json() {
        let deep = format!("{}{}", "[".repeat(40), "]".repeat(40));
        for json in [
            "",
            "[",
            "[{]",
            r#"[{"ext" "x"}]"#,
            r#"[{"ext":"x}]"#,
            r#"["\q"]"#,
            r#"["\ud800"]"#,
            "[\"a\u{1}\"]",
            "[-]",
            "[1.]",
            "[] x",
            "[tru]",
            deep.as_str(),
        ] {
            let got = parse_formats(json).unwrap_err();
            assert!(
                got.starts_with("the formats JSON is malformed"),
                "{json}: {got}"
            );
        }
    }

    #[test]
    fn formats_json_round_trips() {
        let decls = vec![
            decl("mpp", "Project \"2024\"\\\n\u{1}", true, true),
            FormatDecl {
                default: true,
                ..decl("fakeproj", "Fake", false, true)
            },
        ];
        assert_eq!(parse_formats(&formats_json(&decls)).unwrap(), decls);
        // A type that is not the default is written as before the flag.
        assert_eq!(
            formats_json(&[decl("mpp", "P", true, false)]),
            r#"[{"ext":"mpp","label":"P","read":true,"write":false}]"#
        );
        assert!(formats_json(&decls).ends_with(r#""write":true,"default":true}]"#));
    }

    #[test]
    fn codes_have_names() {
        assert_eq!(code_name(OK), "ok");
        assert_eq!(code_name(UNSUPPORTED_EXTENSION), "unsupported extension");
        assert_eq!(code_name(MALFORMED_INPUT), "malformed input");
        assert_eq!(code_name(UNSUPPORTED_CONTENT), "unsupported content");
        assert_eq!(code_name(INTERNAL_ERROR), "internal error");
        assert_eq!(code_name(99), "unknown error");
        assert!(YppxBuf::default().ptr.is_null());
        assert!(SYMBOLS.iter().all(|s| s.starts_with(FILE_PREFIX)));
    }
}
