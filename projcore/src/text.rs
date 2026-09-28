//! Text conventions shared by the readers that fill the model.

/// Map `\r\n` and a lone `\r` to `\n`, the one newline the model's multi-line
/// text (task, resource and assignment notes) uses whatever it was read from.
/// Nothing is trimmed: a note that is only a line break stays `"\n"`.
pub fn normalize_newlines(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            chars.next_if_eq(&'\n');
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::normalize_newlines;

    #[test]
    fn crlf_and_lone_cr_become_lf_without_trimming() {
        assert_eq!(normalize_newlines("\r\n"), "\n");
        assert_eq!(normalize_newlines("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(normalize_newlines("a\r\rb"), "a\n\nb");
        assert_eq!(normalize_newlines("a\n\rb"), "a\n\nb");
        assert_eq!(normalize_newlines("\r"), "\n");
        assert_eq!(normalize_newlines("a\nb — ✓"), "a\nb — ✓");
        assert_eq!(normalize_newlines(""), "");
    }
}
