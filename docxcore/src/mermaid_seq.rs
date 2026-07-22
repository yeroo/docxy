//! Parser for `sequenceDiagram` Mermaid sources into a structured model.
//!
//! This module is parse-only: it turns a sequence-diagram source string into
//! a [`SequenceDiagram`] model (participants, messages, alt/else frames,
//! notes). Layout and DrawingML emission are handled by later tasks; this
//! module never panics on malformed input (parsing is total).

use std::collections::HashMap;

/// A participant (lifeline) in the diagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    pub id: String,
    pub label: String,
    /// Layout: x position, filled in by Task 2. Defaults to 0 here.
    #[allow(dead_code)]
    pub x: i64,
    /// Layout: lifeline column width, filled in by Task 2. Defaults to 0 here.
    #[allow(dead_code)]
    pub w: i64,
}

/// Message arrow style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKind {
    Solid,
    Dashed,
}

/// A single message (arrow) between two participants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub from: usize,
    pub to: usize,
    pub text: String,
    pub kind: MsgKind,
    pub self_msg: bool,
    pub row: usize,
}

/// An `alt`/`else`/`end` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub label: String,
    pub else_label: Option<String>,
    pub span_first: usize,
    pub span_last: usize,
    pub row_start: usize,
    pub else_row: Option<usize>,
    pub row_end: usize,
}

/// A `Note over A,B: text` annotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub span_first: usize,
    pub span_last: usize,
    pub text: String,
    pub row: usize,
}

/// The full parsed sequence diagram.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SequenceDiagram {
    pub participants: Vec<Participant>,
    pub messages: Vec<Message>,
    pub frames: Vec<Frame>,
    pub notes: Vec<Note>,
    pub rows: usize,
}

/// Returns true if `src`'s first non-empty, non-`%%`-comment line is a
/// `sequenceDiagram` header (case-insensitive, trimmed).
pub fn is_sequence(src: &str) -> bool {
    for line in src.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with("%%") {
            continue;
        }
        return t.to_lowercase() == "sequencediagram";
    }
    false
}

/// Parser scratch state, tracking participant lookup and open frame stack
/// while walking the source line by line.
struct ParseState {
    diagram: SequenceDiagram,
    index: HashMap<String, usize>,
    frame_stack: Vec<usize>,
    row: usize,
}

impl ParseState {
    fn new() -> Self {
        ParseState {
            diagram: SequenceDiagram::default(),
            index: HashMap::new(),
            frame_stack: Vec::new(),
            row: 0,
        }
    }

    /// Look up a participant by id, auto-creating a bare one (label == id)
    /// on first sight, in first-seen order.
    fn get(&mut self, id: &str) -> usize {
        if let Some(&i) = self.index.get(id) {
            return i;
        }
        let i = self.diagram.participants.len();
        self.diagram.participants.push(Participant {
            id: id.to_string(),
            label: id.to_string(),
            x: 0,
            w: 0,
        });
        self.index.insert(id.to_string(), i);
        i
    }

    /// Declare/dedup a participant with an explicit label (from a
    /// `participant`/`actor` statement). If already known (e.g. auto-created
    /// by an earlier message reference), updates its label in place.
    fn declare(&mut self, id: &str, label: &str) {
        if let Some(&i) = self.index.get(id) {
            self.diagram.participants[i].label = label.to_string();
        } else {
            let i = self.diagram.participants.len();
            self.diagram.participants.push(Participant {
                id: id.to_string(),
                label: label.to_string(),
                x: 0,
                w: 0,
            });
            self.index.insert(id.to_string(), i);
        }
    }

    /// Widen the innermost open frame's column span to include `cols`.
    fn touch_frame_span(&mut self, cols: &[usize]) {
        if cols.is_empty() {
            return;
        }
        if let Some(&fi) = self.frame_stack.last() {
            if let Some(f) = self.diagram.frames.get_mut(fi) {
                let lo = cols.iter().copied().min().unwrap();
                let hi = cols.iter().copied().max().unwrap();
                f.span_first = f.span_first.min(lo);
                f.span_last = f.span_last.max(hi);
            }
        }
    }
}

/// Parses a `sequenceDiagram` source string into a [`SequenceDiagram`]
/// model. Never panics: malformed or unrecognized lines are ignored.
pub fn parse(src: &str) -> SequenceDiagram {
    let mut st = ParseState::new();

    for raw_line in src.lines() {
        let line = strip_comment(raw_line).trim().to_string();
        if line.is_empty() {
            continue;
        }

        let lower = line.to_lowercase();

        if lower == "sequencediagram" {
            continue;
        }
        if lower.starts_with("title") || lower.starts_with("autonumber") {
            continue;
        }

        if let Some(rest) = strip_prefix_ci(&line, "participant") {
            parse_participant(&mut st, rest);
            continue;
        }
        if let Some(rest) = strip_prefix_ci(&line, "actor") {
            parse_participant(&mut st, rest);
            continue;
        }

        if let Some(rest) = strip_prefix_ci(&line, "alt") {
            let label = rest.trim().to_string();
            let fi = st.diagram.frames.len();
            st.diagram.frames.push(Frame {
                label,
                else_label: None,
                span_first: usize::MAX,
                span_last: 0,
                row_start: st.row,
                else_row: None,
                row_end: st.row,
            });
            st.frame_stack.push(fi);
            continue;
        }
        if let Some(rest) = strip_prefix_ci(&line, "else") {
            if let Some(&fi) = st.frame_stack.last() {
                if let Some(f) = st.diagram.frames.get_mut(fi) {
                    f.else_label = Some(rest.trim().to_string());
                    f.else_row = Some(st.row);
                }
            }
            continue;
        }
        if lower == "end" {
            if let Some(fi) = st.frame_stack.pop() {
                if let Some(f) = st.diagram.frames.get_mut(fi) {
                    f.row_end = st.row.saturating_sub(1).max(f.row_start);
                }
            }
            continue;
        }
        // Other block openers (loop/opt/par/...): their bodies are walked
        // like top-level content, and their closing `end` is consumed above
        // only if a frame happens to be open; otherwise it's a no-op. We
        // don't open a frame for them in this slice (documented: nested
        // non-alt blocks aren't drawn).
        if is_unhandled_block_opener(&lower) {
            continue;
        }

        if let Some(rest) = strip_prefix_ci(&line, "note over") {
            parse_note(&mut st, rest);
            continue;
        }

        parse_message(&mut st, &line);
    }

    st.diagram.rows = st.row;
    st.diagram
}

/// True for known block-opener keywords other than `alt`, whose bodies we
/// walk but don't frame in this slice.
fn is_unhandled_block_opener(lower_line: &str) -> bool {
    for kw in ["loop", "opt", "par", "critical", "rect", "break"] {
        if lower_line == kw
            || lower_line.starts_with(&format!("{kw} "))
            || lower_line.starts_with(&format!("{kw}\t"))
        {
            return true;
        }
    }
    false
}

/// Strips a case-insensitive prefix keyword, requiring it be followed by
/// whitespace or end-of-string, returning the remainder (untrimmed).
fn strip_prefix_ci<'a>(line: &'a str, kw: &str) -> Option<&'a str> {
    let lower = line.to_lowercase();
    if lower == kw {
        return Some("");
    }
    let kw_sp = format!("{kw} ");
    if lower.starts_with(&kw_sp) {
        return Some(&line[kw_sp.len()..]);
    }
    None
}

/// Strips a `%%` end-of-line comment.
fn strip_comment(line: &str) -> &str {
    match line.find("%%") {
        Some(idx) => &line[..idx],
        None => line,
    }
}

/// Parses the remainder of a `participant`/`actor` statement:
/// `ID as Label` or bare `ID`.
fn parse_participant(st: &mut ParseState, rest: &str) {
    let rest = rest.trim();
    if rest.is_empty() {
        return;
    }
    if let Some(idx) = find_word(rest, "as") {
        let id = rest[..idx].trim();
        let label = rest[idx + 2..].trim();
        if id.is_empty() {
            return;
        }
        st.declare(id, if label.is_empty() { id } else { label });
    } else {
        st.declare(rest, rest);
    }
}

/// Finds a standalone word `word` in `s` (surrounded by whitespace),
/// returning the byte index where it starts.
///
/// UTF-8 safe: only ever slices `s` at indices produced by `char_indices`
/// (always valid start boundaries) after confirming the candidate's end
/// index is also a char boundary, so this never panics on multi-byte input
/// (e.g. `participant 中 as 日本語`).
fn find_word(s: &str, word: &str) -> Option<usize> {
    let wlen = word.len();
    if wlen == 0 || s.len() < wlen {
        return None;
    }
    for (i, _) in s.char_indices() {
        let end = i + wlen;
        if end > s.len() || !s.is_char_boundary(end) {
            continue;
        }
        if s[i..end].eq_ignore_ascii_case(word) {
            let before_ok = i == 0 || s[..i].chars().next_back().is_some_and(char::is_whitespace);
            let after_ok =
                end == s.len() || s[end..].chars().next().is_some_and(char::is_whitespace);
            if before_ok && after_ok {
                return Some(i);
            }
        }
    }
    None
}

/// Parses a `Note over A,B: text` / `Note over A: text` statement remainder
/// (the part after `note over`).
fn parse_note(st: &mut ParseState, rest: &str) {
    let rest = rest.trim();
    let (span_part, text) = match rest.split_once(':') {
        Some((s, t)) => (s.trim(), t.trim().to_string()),
        None => (rest, String::new()),
    };
    if span_part.is_empty() {
        return;
    }
    let ids: Vec<&str> = span_part
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if ids.is_empty() {
        return;
    }
    let cols: Vec<usize> = ids.iter().map(|id| st.get(id)).collect();
    let span_first = *cols.iter().min().unwrap();
    let span_last = *cols.iter().max().unwrap();
    let row = st.row;
    st.touch_frame_span(&cols);
    st.diagram.notes.push(Note {
        span_first,
        span_last,
        text,
        row,
    });
    st.row += 1;
}

/// Arrow tokens, longest/most-specific first so `-->>` is matched before
/// `->>` etc.
const ARROWS: &[(&str, MsgKind)] = &[
    ("-->>", MsgKind::Dashed),
    ("--)", MsgKind::Dashed),
    ("->>", MsgKind::Solid),
    ("-)", MsgKind::Solid),
    ("-->", MsgKind::Dashed),
    ("->", MsgKind::Solid),
];

/// Parses a message line: `LEFT ARROW RIGHT : text`. Ignores the line if no
/// arrow token or empty participant names are found (total parsing).
fn parse_message(st: &mut ParseState, line: &str) {
    let mut found: Option<(usize, usize, MsgKind)> = None;
    for (tok, kind) in ARROWS {
        if let Some(pos) = line.find(tok) {
            match found {
                Some((fpos, _, _)) if fpos <= pos => {}
                _ => found = Some((pos, tok.len(), *kind)),
            }
        }
    }
    let (pos, tok_len, kind) = match found {
        Some(v) => v,
        None => return,
    };

    let left = line[..pos].trim();
    let after = &line[pos + tok_len..];
    let (right_part, text) = match after.split_once(':') {
        Some((r, t)) => (r.trim(), t.trim().to_string()),
        None => (after.trim(), String::new()),
    };
    // Strip an activation marker like `+`/`-` sometimes suffixed to the
    // target in Mermaid (e.g. `A->>+B:`); keep parsing total/simple here.
    let right = right_part.trim_start_matches(['+', '-']).trim();

    if left.is_empty() || right.is_empty() {
        return;
    }

    let from = st.get(left);
    let to = st.get(right);
    let self_msg = from == to;
    let row = st.row;
    st.touch_frame_span(&[from, to]);
    st.diagram.messages.push(Message {
        from,
        to,
        text,
        kind,
        self_msg,
        row,
    });
    st.row += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_sequence_header() {
        assert!(is_sequence("sequenceDiagram\nA->>B: hi"));
        assert!(is_sequence("%% c\n  sequenceDiagram\n"));
        assert!(!is_sequence("flowchart TD\nA-->B"));
    }

    #[test]
    fn participants_with_alias() {
        let d = parse(
            "sequenceDiagram\nparticipant U as User / AI\nparticipant DL as Desktop\nU->>DL: go",
        );
        assert_eq!(d.participants.len(), 2);
        assert_eq!(d.participants[0].id, "U");
        assert_eq!(d.participants[0].label, "User / AI");
    }

    #[test]
    fn messages_row_ordered_and_self() {
        let d = parse("sequenceDiagram\nA->>B: m1\nB->>B: self\nA-->>B: m2");
        assert_eq!(d.messages.len(), 3);
        assert_eq!(d.messages[0].row, 0);
        assert_eq!(d.messages[1].row, 1);
        assert!(d.messages[1].self_msg);
        assert_eq!(d.messages[2].kind, MsgKind::Dashed);
        // Unknown participants auto-created in first-seen order: A, B.
        assert_eq!(d.participants.len(), 2);
    }

    #[test]
    fn alt_else_end_frame() {
        let d =
            parse("sequenceDiagram\nA->>B: x\nalt cond1\n  A->>B: y\nelse cond2\n  B->>A: z\nend");
        assert_eq!(d.frames.len(), 1);
        let f = &d.frames[0];
        assert_eq!(f.label, "cond1");
        assert_eq!(f.else_label.as_deref(), Some("cond2"));
        assert!(f.row_start <= f.else_row.unwrap() && f.else_row.unwrap() <= f.row_end);
        assert_eq!(f.span_first, 0); // A
        assert_eq!(f.span_last, 1); // B
    }

    #[test]
    fn note_over_span() {
        let d = parse("sequenceDiagram\nparticipant A\nparticipant B\nNote over A,B: hello");
        assert_eq!(d.notes.len(), 1);
        assert_eq!(d.notes[0].span_first, 0);
        assert_eq!(d.notes[0].span_last, 1);
        assert_eq!(d.notes[0].text, "hello");
    }

    #[test]
    fn parse_is_total_on_garbage() {
        let _ = parse("sequenceDiagram\nend\nelse\n->> :\nNote over ZZ");
    }

    #[test]
    fn participant_label_with_multibyte_unicode() {
        let d =
            parse("sequenceDiagram\nparticipant U as José\nparticipant 中 as 日本語\nU->>中: msg");
        assert_eq!(d.participants.len(), 2);
        assert_eq!(d.participants[0].id, "U");
        assert_eq!(d.participants[0].label, "José");
        assert_eq!(d.participants[1].id, "中");
        assert_eq!(d.participants[1].label, "日本語");
        assert_eq!(d.messages.len(), 1);
        assert_eq!(d.messages[0].from, 0);
        assert_eq!(d.messages[0].to, 1);
        assert_eq!(d.messages[0].text, "msg");
    }

    #[test]
    fn parse_is_total_on_multibyte_tokens() {
        let _ = parse("sequenceDiagram\nparticipant 🚀\nNote over 🚀: café\n");
    }
}
