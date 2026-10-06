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

/// The one-line form of multi-line notes for a single-line prompt: `\`
/// becomes `\\` and a newline becomes `\n` (backslash + letter n). Decoding
/// ([`notes_from_line`]) is lenient so a hand-typed buffer never errors.
pub fn notes_to_line(notes: &str) -> String {
    if !notes.contains(['\\', '\n']) {
        return notes.to_owned();
    }
    let mut line = String::with_capacity(notes.len());
    for c in notes.chars() {
        match c {
            '\\' => line.push_str("\\\\"),
            '\n' => line.push_str("\\n"),
            _ => line.push(c),
        }
    }
    line
}

/// Decode [`notes_to_line`]'s escape form back to the notes. `\\` is `\`,
/// `\n` is a newline; any other `\x` — and a trailing lone `\` — is kept
/// verbatim, so a hand-typed buffer cannot fail to decode.
pub fn notes_from_line(line: &str) -> String {
    if !line.contains('\\') {
        return line.to_owned();
    }
    let mut notes = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            notes.push(c);
            continue;
        }
        match chars.peek() {
            Some('\\') => {
                chars.next();
                notes.push('\\');
            }
            Some('n') => {
                chars.next();
                notes.push('\n');
            }
            _ => notes.push('\\'),
        }
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::{normalize_newlines, notes_from_line, notes_to_line};

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

    #[test]
    fn notes_line_codec_round_trips() {
        // Any string comes back from the one-line form unchanged: a literal
        // backslash, newlines, the escape sequences themselves, a trailing
        // lone backslash and non-ASCII text included.
        for notes in [
            "",
            "plain",
            "\\",
            "trailing\\",
            "a\\b",
            "a\\nb",
            "a\\nb",
            "\\n",
            "\\\\n",
            "a\nb",
            "\n",
            "a\n\nb\nc",
            "C:\\path\\to\\file\nsecond line",
            "a — ✓ \\ b\nç",
        ] {
            assert_eq!(notes_from_line(&notes_to_line(notes)), notes, "{notes:?}");
            // The one-line form holds no real newline.
            assert!(!notes_to_line(notes).contains('\n'), "{notes:?}");
        }
        assert_eq!(notes_to_line("a\nb"), "a\\nb");
        assert_eq!(notes_to_line("a\\b"), "a\\\\b");
        assert_eq!(notes_to_line("a\\nb"), "a\\\\nb");
    }

    #[test]
    fn notes_from_line_is_lenient() {
        // Unknown escapes and a trailing lone backslash stay verbatim.
        assert_eq!(notes_from_line("a\\qb"), "a\\qb");
        assert_eq!(notes_from_line("a\\"), "a\\");
        assert_eq!(notes_from_line("\\n"), "\n");
        assert_eq!(notes_from_line("\\\\n"), "\\n");
        assert_eq!(notes_from_line("plain"), "plain");
    }
}
