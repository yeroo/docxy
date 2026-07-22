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
    /// Layout: x position (EMU), filled in by [`layout`]. Defaults to 0 here.
    pub x: i64,
    /// Layout: lifeline column width (EMU), filled in by [`layout`]. Defaults
    /// to 0 here.
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

// ---------------------------------------------------------------------
// Layout (Task 2): fills `Participant.x`/`w` in EMU and exposes the row/
// height metrics shared by `build_geometry`.
// ---------------------------------------------------------------------

/// EMU per inch, matching `mermaid`'s convention (914,400 EMU = 1 inch).
const EMU_PER_INCH: i64 = 914_400;

/// Participant header-box height: ~0.5".
const HEADER_H: i64 = EMU_PER_INCH / 2;
/// Gap between adjacent lifeline columns: ~0.4".
const COL_GAP: i64 = EMU_PER_INCH * 4 / 10;
/// Left/right canvas margin (reuses the column gap).
const SIDE_MARGIN: i64 = COL_GAP;
/// Space below the last row before the canvas's bottom edge.
const BOTTOM_MARGIN: i64 = EMU_PER_INCH * 3 / 10;
/// Row height for a row with only cross-lifeline messages: ~0.5".
const ROW_STEP: i64 = EMU_PER_INCH / 2;
/// Row height for a row containing a self-message (needs a loop): ~0.75".
const SELF_ROW_STEP: i64 = EMU_PER_INCH * 3 / 4;
/// Estimated text width per character.
const CHAR_W: i64 = EMU_PER_INCH * 9 / 100;
/// Fixed padding added on top of the character-width estimate.
const PAD_W: i64 = EMU_PER_INCH * 2 / 10;
/// Lifeline column width bounds: ~1.3"..=2.6".
const MIN_COL_W: i64 = EMU_PER_INCH * 13 / 10;
const MAX_COL_W: i64 = EMU_PER_INCH * 26 / 10;
/// How far a self-message loop extends right of its own lifeline.
const SELF_LOOP_W: i64 = EMU_PER_INCH / 2;
/// Padding around a frame's bounding box (notes are unpadded: they sit
/// flush with the participant columns and row they span).
const FRAME_PAD: i64 = EMU_PER_INCH / 10;
/// Sensible minimum canvas size, used as a floor for empty/degenerate
/// diagrams so nothing ever collapses to a zero- or negative-size canvas.
const MIN_CANVAS_W: i64 = EMU_PER_INCH * 2;
const MIN_CANVAS_H: i64 = EMU_PER_INCH;

/// Top margin above row 0: header height plus one column gap.
fn top_margin() -> i64 {
    HEADER_H + COL_GAP
}

/// Estimated EMU width of a text label at [`CHAR_W`] per character, plus
/// [`PAD_W`] fixed padding.
fn text_w(s: &str) -> i64 {
    s.chars().count() as i64 * CHAR_W + PAD_W
}

/// Fills in `Participant.x`/`w` for every participant, left to right.
/// Column width is `max(label width, width of any message text touching
/// that column)`, clamped to `[MIN_COL_W, MAX_COL_W]`. A no-op (leaves an
/// empty `Vec`) when there are no participants.
pub fn layout(d: &mut SequenceDiagram) {
    let n = d.participants.len();
    let mut widths = vec![MIN_COL_W; n];
    for (i, w) in widths.iter_mut().enumerate() {
        let mut want = text_w(&d.participants[i].label);
        for m in &d.messages {
            if m.from == i || m.to == i {
                want = want.max(text_w(&m.text));
            }
        }
        *w = want.clamp(MIN_COL_W, MAX_COL_W);
    }
    let mut x = SIDE_MARGIN;
    for (i, p) in d.participants.iter_mut().enumerate() {
        p.w = widths[i];
        p.x = x;
        x += widths[i] + COL_GAP;
    }
}

/// True if any message occupies row `r` and is a self-message (from == to),
/// which needs a taller row to draw its loop.
fn row_is_self(d: &SequenceDiagram, r: usize) -> bool {
    d.messages.iter().any(|m| m.row == r && m.self_msg)
}

/// Cumulative row-top offsets: `tops[r]` is the top y of row `r` (0-indexed,
/// EMU, measured from the canvas origin), and `tops[d.rows]` is the bottom
/// y of the last row (i.e. the top of the bottom margin). Always has
/// `d.rows + 1` entries, so `tops[0] == top_margin()` even when `d.rows ==
/// 0` (an empty diagram still gets a well-defined, non-empty canvas).
fn row_tops(d: &SequenceDiagram) -> Vec<i64> {
    let mut tops = Vec::with_capacity(d.rows + 1);
    let mut acc = top_margin();
    for r in 0..d.rows {
        tops.push(acc);
        acc += if row_is_self(d, r) {
            SELF_ROW_STEP
        } else {
            ROW_STEP
        };
    }
    tops.push(acc);
    tops
}

/// Row height (EMU) of row `r`.
fn row_height(d: &SequenceDiagram, r: usize) -> i64 {
    if row_is_self(d, r) {
        SELF_ROW_STEP
    } else {
        ROW_STEP
    }
}

/// The top y of row `r`, saturating to the last known offset for
/// out-of-range rows (defensive: never panics/indexes out of bounds).
fn row_top_at(tops: &[i64], r: usize) -> i64 {
    tops.get(r)
        .copied()
        .unwrap_or_else(|| tops.last().copied().unwrap_or_else(top_margin))
}

/// The bottom y of row `r` (i.e. the top of row `r + 1`).
fn row_bottom_at(tops: &[i64], r: usize) -> i64 {
    row_top_at(tops, r + 1)
}

/// `(x, w)` of participant `i`, saturating to the last participant (or
/// `(0, 0)` if there are none) for an out-of-range index. Defensive: a
/// frame/note whose span was never widened by a message (e.g. an empty
/// `alt` block) can otherwise carry a garbage index.
fn safe_participant(d: &SequenceDiagram, i: usize) -> (i64, i64) {
    match d.participants.get(i) {
        Some(p) => (p.x, p.w),
        None => d.participants.last().map(|p| (p.x, p.w)).unwrap_or((0, 0)),
    }
}

// ---------------------------------------------------------------------
// Geometry (Task 2): the serializable, render-ready layout consumed by
// the DrawingML emitter (Task 3/4) and the webview (Task 5).
// ---------------------------------------------------------------------

/// A participant's header box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartBox {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub label: String,
}

/// A participant's lifeline (vertical rule below its header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lifeline {
    pub x: i64,
    pub y1: i64,
    pub y2: i64,
}

/// A laid-out message arrow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsgGeom {
    pub x1: i64,
    pub y1: i64,
    pub x2: i64,
    pub y2: i64,
    pub text: String,
    pub dashed: bool,
    pub self_msg: bool,
}

/// A laid-out `alt`/`else` frame box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameGeom {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub label: String,
    pub else_label: Option<String>,
    pub else_y: Option<i64>,
}

/// A laid-out `Note over` box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteGeom {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    pub text: String,
}

/// The full render-ready geometry of a sequence diagram, in EMU. Produced
/// by [`geometry`] (parse + [`layout`] + build); serialized via
/// [`SequenceGeometry::to_json`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SequenceGeometry {
    pub canvas_w: i64,
    pub canvas_h: i64,
    pub participants: Vec<PartBox>,
    pub lifelines: Vec<Lifeline>,
    pub messages: Vec<MsgGeom>,
    pub frames: Vec<FrameGeom>,
    pub notes: Vec<NoteGeom>,
}

/// Parses, lays out, and builds the full [`SequenceGeometry`] for a
/// `sequenceDiagram` source. Total: never panics, even on empty or
/// malformed input (falls back to sensible minimum canvas dimensions).
pub fn geometry(src: &str) -> SequenceGeometry {
    let mut d = parse(src);
    layout(&mut d);
    build_geometry(&d)
}

fn build_geometry(d: &SequenceDiagram) -> SequenceGeometry {
    let tops = row_tops(d);
    // Bottom of the last row (top of the bottom margin); well-defined even
    // when `d.rows == 0`, since `row_tops` always yields >= 1 entry.
    let bottom = tops.last().copied().unwrap_or_else(top_margin);
    let canvas_h = (bottom + BOTTOM_MARGIN).max(MIN_CANVAS_H);
    let lifeline_y2 = canvas_h - BOTTOM_MARGIN;

    let mut participants = Vec::with_capacity(d.participants.len());
    let mut lifelines = Vec::with_capacity(d.participants.len());
    for p in &d.participants {
        participants.push(PartBox {
            x: p.x,
            y: 0,
            w: p.w,
            h: HEADER_H,
            label: p.label.clone(),
        });
        lifelines.push(Lifeline {
            x: p.x + p.w / 2,
            y1: HEADER_H,
            y2: lifeline_y2,
        });
    }

    let mut messages = Vec::with_capacity(d.messages.len());
    for m in &d.messages {
        let (fx, fw) = safe_participant(d, m.from);
        let (tx, tw) = safe_participant(d, m.to);
        let x1 = fx + fw / 2;
        let row_top = row_top_at(&tops, m.row);
        let h = row_height(d, m.row);
        let (x2, y1, y2) = if m.self_msg {
            (
                x1 + SELF_LOOP_W,
                row_top + FRAME_PAD,
                row_top + h - FRAME_PAD,
            )
        } else {
            let center = row_top + h / 2;
            (tx + tw / 2, center, center)
        };
        messages.push(MsgGeom {
            x1,
            y1,
            x2,
            y2,
            text: m.text.clone(),
            dashed: matches!(m.kind, MsgKind::Dashed),
            self_msg: m.self_msg,
        });
    }

    let n = d.participants.len();
    let mut frames = Vec::with_capacity(d.frames.len());
    for f in &d.frames {
        let lo = f.span_first.min(n.saturating_sub(1));
        let hi = f.span_last.min(n.saturating_sub(1));
        let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
        let (lx, _) = safe_participant(d, lo);
        let (rx, rw) = safe_participant(d, hi);
        let x = (lx - FRAME_PAD).max(0);
        let right = rx + rw + FRAME_PAD;
        let w = (right - x).max(0);
        let y = (row_top_at(&tops, f.row_start) - FRAME_PAD).max(0);
        let bottom_y = row_bottom_at(&tops, f.row_end) + FRAME_PAD;
        let h = (bottom_y - y).max(0);
        let else_y = f.else_row.map(|r| row_top_at(&tops, r));
        frames.push(FrameGeom {
            x,
            y,
            w,
            h,
            label: f.label.clone(),
            else_label: f.else_label.clone(),
            else_y,
        });
    }

    let mut notes = Vec::with_capacity(d.notes.len());
    for nt in &d.notes {
        let lo = nt.span_first.min(n.saturating_sub(1));
        let hi = nt.span_last.min(n.saturating_sub(1));
        let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
        let (lx, _) = safe_participant(d, lo);
        let (rx, rw) = safe_participant(d, hi);
        let x = lx;
        let w = (rx + rw - lx).max(0);
        let y = row_top_at(&tops, nt.row);
        let h = (row_bottom_at(&tops, nt.row) - y).max(0);
        notes.push(NoteGeom {
            x,
            y,
            w,
            h,
            text: nt.text.clone(),
        });
    }

    let mut right_max = 0i64;
    for p in &participants {
        right_max = right_max.max(p.x + p.w);
    }
    for f in &frames {
        right_max = right_max.max(f.x + f.w);
    }
    for nt in &notes {
        right_max = right_max.max(nt.x + nt.w);
    }
    let canvas_w = if right_max > 0 {
        (right_max + SIDE_MARGIN).max(MIN_CANVAS_W)
    } else {
        MIN_CANVAS_W
    };

    SequenceGeometry {
        canvas_w,
        canvas_h,
        participants,
        lifelines,
        messages,
        frames,
        notes,
    }
}

impl SequenceGeometry {
    /// Serializes to the kind-tagged JSON shape the webview (Task 5) and
    /// DrawingML dispatch (Task 4) read: `{"kind":"sequence", ...}`.
    /// Optional fields (`elseLabel`/`elseY`) are emitted as JSON `null`
    /// when absent, never omitted, so the shape is uniform across frames.
    pub fn to_json(&self) -> String {
        let mut s = String::new();
        s.push_str("{\"kind\":\"sequence\",\"canvasW\":");
        s.push_str(&self.canvas_w.to_string());
        s.push_str(",\"canvasH\":");
        s.push_str(&self.canvas_h.to_string());

        s.push_str(",\"participants\":[");
        for (i, p) in self.participants.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"x\":{},\"y\":{},\"w\":{},\"h\":{},\"label\":",
                p.x, p.y, p.w, p.h
            ));
            json_str(&mut s, &p.label);
            s.push('}');
        }

        s.push_str("],\"lifelines\":[");
        for (i, l) in self.lifelines.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"x\":{},\"y1\":{},\"y2\":{}}}",
                l.x, l.y1, l.y2
            ));
        }

        s.push_str("],\"messages\":[");
        for (i, m) in self.messages.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"x1\":{},\"y1\":{},\"x2\":{},\"y2\":{},\"text\":",
                m.x1, m.y1, m.x2, m.y2
            ));
            json_str(&mut s, &m.text);
            s.push_str(&format!(
                ",\"dashed\":{},\"self\":{}}}",
                m.dashed, m.self_msg
            ));
        }

        s.push_str("],\"frames\":[");
        for (i, f) in self.frames.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"x\":{},\"y\":{},\"w\":{},\"h\":{},\"label\":",
                f.x, f.y, f.w, f.h
            ));
            json_str(&mut s, &f.label);
            s.push_str(",\"elseLabel\":");
            match &f.else_label {
                Some(t) => json_str(&mut s, t),
                None => s.push_str("null"),
            }
            s.push_str(",\"elseY\":");
            match f.else_y {
                Some(y) => s.push_str(&y.to_string()),
                None => s.push_str("null"),
            }
            s.push('}');
        }

        s.push_str("],\"notes\":[");
        for (i, nt) in self.notes.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                "{{\"x\":{},\"y\":{},\"w\":{},\"h\":{},\"text\":",
                nt.x, nt.y, nt.w, nt.h
            ));
            json_str(&mut s, &nt.text);
            s.push('}');
        }
        s.push_str("]}");
        s
    }
}

/// Minimal JSON string escaping (quotes, backslash, control chars); a
/// local copy of `mermaid`'s `json_str` (kept local per Task 2 scope,
/// which only touches this file).
fn json_str(out: &mut String, s: &str) {
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

    #[test]
    fn layout_columns_and_rows() {
        let g = geometry(
            "sequenceDiagram\nparticipant A\nparticipant B\nparticipant C\nA->>B: m1\nB->>C: m2",
        );
        assert_eq!(g.participants.len(), 3);
        // Columns strictly increasing, non-overlapping.
        assert!(g.participants[0].x + g.participants[0].w <= g.participants[1].x);
        assert!(g.participants[1].x + g.participants[1].w <= g.participants[2].x);
        // A lifeline per participant, spanning below the header.
        assert_eq!(g.lifelines.len(), 3);
        assert!(g.lifelines[0].y2 > g.lifelines[0].y1);
        // Two messages, monotonically increasing y (row order).
        assert_eq!(g.messages.len(), 2);
        assert!(g.messages[1].y1 > g.messages[0].y1);
        assert!(g.canvas_w > 0 && g.canvas_h > 0);
    }

    #[test]
    fn json_tagged_sequence() {
        let j = geometry("sequenceDiagram\nA->>B: hi").to_json();
        assert!(j.contains("\"kind\":\"sequence\""));
        assert!(j.contains("\"lifelines\":[") && j.contains("\"messages\":["));
    }

    #[test]
    fn frame_box_spans_participants_and_rows() {
        let g = geometry(
            "sequenceDiagram\nparticipant A\nparticipant B\nalt c\n A->>B: y\nelse d\n B->>A: z\nend",
        );
        assert_eq!(g.frames.len(), 1);
        let f = &g.frames[0];
        // Spans from A's column to B's column, has an else divider inside its height.
        assert!(f.w > 0 && f.h > 0);
        assert!(f.else_y.unwrap() > f.y && f.else_y.unwrap() < f.y + f.h);
    }

    #[test]
    fn empty_diagram_does_not_panic() {
        let g = geometry("sequenceDiagram\n");
        assert_eq!(g.participants.len(), 0);
        assert!(g.canvas_w > 0 && g.canvas_h > 0);
        let j = g.to_json();
        assert!(j.contains("\"kind\":\"sequence\""));
    }

    #[test]
    fn self_message_layout_and_json_flag() {
        let g = geometry("sequenceDiagram\nA->>B: m1\nB->>B: self");
        assert_eq!(g.messages.len(), 2);
        assert!(g.messages[1].self_msg);
        // Self message loops out to the right of its own lifeline.
        assert!(g.messages[1].x2 > g.messages[1].x1);
        let j = g.to_json();
        assert!(j.contains("\"self\":true"));
    }

    #[test]
    fn note_rect_spans_its_participants() {
        let g = geometry("sequenceDiagram\nparticipant A\nparticipant B\nNote over A,B: hello");
        assert_eq!(g.notes.len(), 1);
        let n = &g.notes[0];
        assert!(n.w > 0 && n.h > 0);
        assert_eq!(n.x, g.participants[0].x);
    }
}
