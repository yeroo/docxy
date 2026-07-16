//! A minimal JSON string emitter — just enough to serialize the render output
//! for the webview. In the spirit of the rest of the stack (hand-written ZIP,
//! XML, PDF), we don't pull a serialization crate for a handful of shapes.
//!
//! We only ever *emit* JSON here; the command channel from the webview uses a
//! trivial tab-delimited protocol (see `bridge::dispatch`), so no JSON *parser*
//! is needed on the Rust side.

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
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    push_str(&mut out, s);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_control_and_quotes() {
        assert_eq!(quote("a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(quote("line\nbreak\ttab"), r#""line\nbreak\ttab""#);
        assert_eq!(quote("\u{1}"), "\"\\u0001\""); // control char \u0001 -> \\u0001
        assert_eq!(quote("héllo"), "\"héllo\""); // non-ASCII passes through as UTF-8
    }
}
