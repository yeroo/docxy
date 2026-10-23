//! The state and pure logic of a `richtext` dialog control (#1557).
//!
//! The control's `Value::Text` stays the plain text, so every caret,
//! selection and paste path of a multiline works unchanged. Beside it
//! [`RichState`] keeps a style byte per character and a [`Para`] per line,
//! spliced in step by every edit. Nothing here draws or reads the dialog:
//! the host renders [`line_segments`] and offsets clicks by [`line_offset`].

use projcore::richtext::{Align, BOLD, ITALIC, Para, Rich};

/// Width in pixels of the bullet gutter of a bulleted line.
pub(crate) const BULLET_GUTTER: f32 = 14.;

/// The rich attributes of a richtext control's text.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct RichState {
    /// A style byte per character (`projcore::richtext::BOLD | ITALIC`).
    pub chars: Vec<u8>,
    /// Properties per `\n`-separated line.
    pub paras: Vec<Para>,
    /// The style the next typed text takes, set by a toggle at a bare caret
    /// and dropped by the first thing that moves the caret away or types.
    pub pending: Option<u8>,
}

/// What the toolbar shows for the caret or selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ToolState {
    pub bold: bool,
    pub italic: bool,
    pub align: Align,
    pub bullets: bool,
    pub pending_bold: bool,
    pub pending_italic: bool,
}

impl ToolState {
    /// The `richtext` object of `dialog-read`.
    pub fn to_json(self) -> ctlcore::json::Json {
        use ctlcore::json::Json;
        Json::obj(vec![
            ("bold", Json::Bool(self.bold)),
            ("italic", Json::Bool(self.italic)),
            (
                "align",
                Json::Str(
                    match self.align {
                        Align::Left => "left",
                        Align::Center => "center",
                        Align::Right => "right",
                    }
                    .into(),
                ),
            ),
            ("bullets", Json::Bool(self.bullets)),
            ("pending_bold", Json::Bool(self.pending_bold)),
            ("pending_italic", Json::Bool(self.pending_italic)),
        ])
    }
}

/// The line (index into the paragraphs) character `at` of `text` is on.
fn line_of(text: &[char], at: usize) -> usize {
    text[..at.min(text.len())]
        .iter()
        .filter(|&&c| c == '\n')
        .count()
}

impl RichState {
    pub fn from_rich(r: &Rich) -> Self {
        Self {
            chars: r.chars.clone(),
            paras: r.paras.clone(),
            pending: None,
        }
    }

    /// Clamp or pad the attributes to `text`: a plain, left aligned tail for
    /// what is missing. Called before the state is read, as a value assigned
    /// without the control (a dialog-set of the plain text, an owner's write)
    /// leaves it as it was.
    pub fn sync(&mut self, text: &str) {
        let (chars, lines) = (text.chars().count(), text.split('\n').count());
        self.chars.resize(chars, 0);
        self.paras.resize(lines, Para::default());
    }

    /// The model of `text` with these attributes.
    pub fn rich(&self, text: &str) -> Rich {
        let mut s = self.clone();
        s.sync(text);
        Rich {
            text: text.to_string(),
            chars: s.chars,
            paras: s.paras,
        }
    }

    /// The style bits typed text takes at `from`: the pending style, else
    /// the character before it, or at a line start the line's first
    /// character, plain in an empty line.
    pub fn insert_style(&self, text: &[char], from: usize) -> u8 {
        if let Some(p) = self.pending {
            return p;
        }
        let from = from.min(text.len());
        let style = |i: usize| match text.get(i) {
            Some('\n') | None => None,
            Some(_) => Some(self.chars.get(i).copied().unwrap_or(0)),
        };
        if from > 0
            && let Some(s) = style(from - 1)
        {
            return s;
        }
        style(from).unwrap_or(0)
    }

    /// Replace characters `from..to` of `text` (the text before the edit)
    /// with `with` typed in `style`, the attributes in step: an inserted
    /// `\n` splits the line, copying its properties; removed lines leave the
    /// first one's.
    pub fn splice(&mut self, text: &str, from: usize, to: usize, with: &str, style: u8) {
        self.sync(text);
        let chars: Vec<char> = text.chars().collect();
        let (from, to) = (
            from.min(chars.len()),
            to.min(chars.len()).max(from.min(chars.len())),
        );
        let line = line_of(&chars, from);
        let removed = chars[from..to].iter().filter(|&&c| c == '\n').count();
        let added = with.chars().filter(|&c| c == '\n').count();
        let kept = self.paras[line];
        self.paras.drain(line + 1..line + 1 + removed);
        for _ in 0..added {
            self.paras.insert(line + 1, kept);
        }
        let inserted = with.chars().map(|c| if c == '\n' { 0 } else { style });
        self.chars.splice(from..to, inserted);
        self.pending = None;
    }

    /// The lines of `text` the range `from..to` touches (a bare caret, its
    /// line); a selection ending at a line's start does not touch that line.
    fn lines(text: &[char], from: usize, to: usize) -> std::ops::RangeInclusive<usize> {
        let first = line_of(text, from);
        let last = if to > from {
            line_of(text, to - 1).max(first)
        } else {
            first
        };
        first..=last
    }

    /// The characters of `from..to` that carry text (not the line breaks).
    fn text_indices(text: &[char], from: usize, to: usize) -> impl Iterator<Item = usize> + '_ {
        (from.min(text.len())..to.min(text.len())).filter(|&i| text[i] != '\n')
    }

    /// Toggle style `bit` over the selection (every character has it: clear,
    /// else set), or at a bare caret set the pending style.
    pub fn toggle(&mut self, text: &str, from: usize, to: usize, bit: u8) {
        self.sync(text);
        let chars: Vec<char> = text.chars().collect();
        let idx: Vec<usize> = Self::text_indices(&chars, from, to).collect();
        if idx.is_empty() {
            let now = self.insert_style(&chars, from);
            self.pending = Some(now ^ bit);
            return;
        }
        let all = idx.iter().all(|&i| self.chars[i] & bit != 0);
        for i in idx {
            if all {
                self.chars[i] &= !bit;
            } else {
                self.chars[i] |= bit;
            }
        }
    }

    /// Align every line the range touches.
    pub fn set_align(&mut self, text: &str, from: usize, to: usize, align: Align) {
        self.sync(text);
        let chars: Vec<char> = text.chars().collect();
        for l in Self::lines(&chars, from, to) {
            self.paras[l].align = align;
        }
    }

    /// Bullets on every line the range touches, or off when they all have one.
    pub fn toggle_bullets(&mut self, text: &str, from: usize, to: usize) {
        self.sync(text);
        let chars: Vec<char> = text.chars().collect();
        let lines = Self::lines(&chars, from, to);
        let all = lines.clone().all(|l| self.paras[l].bullet);
        for l in lines {
            self.paras[l].bullet = !all;
        }
    }

    /// What the toolbar shows at the caret (`from == to`) or over a
    /// selection: bold and italic when every selected character has them
    /// (the style typing would take at a caret), the first touched line's
    /// alignment, bullets when every touched line has one.
    pub fn state_at(&self, text: &str, from: usize, to: usize) -> ToolState {
        let mut s = self.clone();
        s.sync(text);
        let chars: Vec<char> = text.chars().collect();
        let idx: Vec<usize> = Self::text_indices(&chars, from, to).collect();
        let (bold, italic) = if idx.is_empty() {
            let now = s.insert_style(&chars, from);
            (now & BOLD != 0, now & ITALIC != 0)
        } else {
            (
                idx.iter().all(|&i| s.chars[i] & BOLD != 0),
                idx.iter().all(|&i| s.chars[i] & ITALIC != 0),
            )
        };
        let lines = Self::lines(&chars, from, to);
        ToolState {
            bold,
            italic,
            align: s.paras[*lines.start()].align,
            bullets: lines.clone().all(|l| s.paras[l].bullet),
            pending_bold: s.pending.is_some_and(|p| p & BOLD != 0),
            pending_italic: s.pending.is_some_and(|p| p & ITALIC != 0),
        }
    }
}

/// A piece of a line drawn in one look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Seg {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub selected: bool,
    /// The caret sits before this piece (an empty last piece holds one at the
    /// line's end).
    pub caret_before: bool,
}

/// Cut one line into pieces: at a style change, at the selection's edges and
/// at the caret. `line` starts at character `start` of the text; `styles` is
/// the line's own styles (one per character); `sel` is the selected range and
/// `caret` the caret, both in the text's offsets (the caret `None` when this
/// control is not focused).
pub(crate) fn line_segments(
    line: &[char],
    styles: &[u8],
    start: usize,
    sel: (usize, usize),
    caret: Option<usize>,
) -> Vec<Seg> {
    let end = start + line.len();
    let caret = caret.filter(|&c| c >= start && c <= end);
    let style = |k: usize| styles.get(k).copied().unwrap_or(0);
    let selected = |k: usize| start + k >= sel.0 && start + k < sel.1;
    let mut out: Vec<Seg> = Vec::new();
    let mut caret_pending = false;
    for (k, &ch) in line.iter().enumerate() {
        if caret == Some(start + k) {
            caret_pending = true;
        }
        let (bold, italic, sel_now) = (style(k) & BOLD != 0, style(k) & ITALIC != 0, selected(k));
        match out.last_mut() {
            Some(last)
                if !caret_pending
                    && last.bold == bold
                    && last.italic == italic
                    && last.selected == sel_now =>
            {
                last.text.push(ch);
            }
            _ => {
                out.push(Seg {
                    text: ch.to_string(),
                    bold,
                    italic,
                    selected: sel_now,
                    caret_before: caret_pending,
                });
                caret_pending = false;
            }
        }
    }
    if caret == Some(end) {
        out.push(Seg {
            text: String::new(),
            bold: false,
            italic: false,
            selected: false,
            caret_before: true,
        });
    }
    out
}

/// How far in from the left of the box's inner area a line's text starts: the
/// bullet gutter, then the free width shared by its alignment.
pub(crate) fn line_offset(para: Para, box_w: f32, line_w: f32) -> f32 {
    let gutter = if para.bullet { BULLET_GUTTER } else { 0. };
    let free = (box_w - gutter - line_w).max(0.);
    gutter
        + match para.align {
            Align::Left => 0.,
            Align::Center => free / 2.,
            Align::Right => free,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(text: &str) -> RichState {
        let mut s = RichState::default();
        s.sync(text);
        s
    }

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    fn invariant(s: &RichState, text: &str) {
        assert_eq!(s.chars.len(), text.chars().count());
        assert_eq!(s.paras.len(), text.split('\n').count());
    }

    #[test]
    fn toggling_a_selection_sets_then_clears() {
        let text = "hello world";
        let mut s = state(text);
        s.toggle(text, 0, 5, BOLD);
        assert_eq!(s.chars[..6], [BOLD, BOLD, BOLD, BOLD, BOLD, 0]);
        // A mixed selection sets; an all-bold one clears.
        s.toggle(text, 3, 8, BOLD);
        assert!(s.chars[3..8].iter().all(|&c| c & BOLD != 0));
        s.toggle(text, 3, 8, BOLD);
        assert!(s.chars[3..8].iter().all(|&c| c & BOLD == 0));
        assert_eq!(s.chars[0], BOLD);
        s.toggle(text, 0, 2, ITALIC);
        assert_eq!(s.chars[0], BOLD | ITALIC);
        assert_eq!(s.pending, None);
    }

    #[test]
    fn a_bare_caret_sets_the_pending_style() {
        let text = "ab";
        let mut s = state(text);
        s.toggle(text, 1, 1, BOLD);
        assert_eq!(s.pending, Some(BOLD));
        assert!(s.state_at(text, 1, 1).pending_bold);
        assert!(s.state_at(text, 1, 1).bold);
        // Toggling again at the caret turns the pending bold off.
        s.toggle(text, 1, 1, BOLD);
        assert_eq!(s.pending, Some(0));
        // Typing takes it and drops it.
        s.pending = Some(BOLD);
        assert_eq!(s.insert_style(&chars(text), 1), BOLD);
        s.splice(text, 1, 1, "X", BOLD);
        assert_eq!(s.chars, [0, BOLD, 0]);
        assert_eq!(s.pending, None);
    }

    #[test]
    fn typed_text_takes_the_style_before_the_caret() {
        let text = "ab\n\ncd";
        let mut s = state(text);
        s.chars[0] = BOLD;
        s.chars[1] = ITALIC;
        let t = chars(text);
        assert_eq!(s.insert_style(&t, 2), ITALIC, "after b");
        // At a line start: the line's first character.
        assert_eq!(s.insert_style(&t, 0), BOLD);
        assert_eq!(s.insert_style(&t, 3), 0, "empty line is plain");
        assert_eq!(s.insert_style(&t, 4), 0);
        assert_eq!(s.insert_style(&chars(""), 0), 0);
    }

    #[test]
    fn splice_keeps_the_attributes_in_step() {
        let mut text = "abc".to_string();
        let mut s = state(&text);
        s.paras[0] = Para {
            align: Align::Center,
            bullet: true,
        };
        s.chars = vec![BOLD, 0, ITALIC];
        // Type in the middle.
        s.splice(&text, 1, 1, "XY", BOLD);
        text = "aXYbc".into();
        invariant(&s, &text);
        assert_eq!(s.chars, [BOLD, BOLD, BOLD, 0, ITALIC]);
        // Enter splits the line and copies its properties.
        s.splice(&text, 2, 2, "\n", 0);
        text = "aX\nYbc".into();
        invariant(&s, &text);
        assert_eq!(s.paras.len(), 2);
        assert_eq!(s.paras[0], s.paras[1]);
        assert_eq!(s.paras[1].align, Align::Center);
        // Align the second line only; joining keeps the first line's.
        s.set_align(&text, 3, 3, Align::Right);
        assert_eq!(s.paras[0].align, Align::Center);
        s.splice(&text, 2, 3, "", 0);
        text = "aXYbc".into();
        invariant(&s, &text);
        assert_eq!(
            s.paras,
            [Para {
                align: Align::Center,
                bullet: true
            }]
        );
        assert_eq!(s.chars, [BOLD, BOLD, BOLD, 0, ITALIC]);
        // A selection across lines, replaced by a paste of two lines.
        let mut s = state("one\ntwo\nthree");
        s.paras[2].bullet = true;
        s.splice("one\ntwo\nthree", 2, 9, "A\nB\nC", 0);
        let text = "onA\nB\nChree";
        invariant(&s, text);
        assert_eq!(s.paras.len(), 3);
        assert!(!s.paras[2].bullet, "the first line's props win");
        // Delete everything.
        s.splice(text, 0, text.chars().count(), "", 0);
        invariant(&s, "");
    }

    #[test]
    fn sync_pads_and_clamps() {
        let mut s = RichState {
            chars: vec![BOLD; 10],
            paras: vec![
                Para {
                    align: Align::Right,
                    bullet: false
                };
                4
            ],
            pending: None,
        };
        s.sync("ab\ncd");
        invariant(&s, "ab\ncd");
        assert_eq!(s.paras[0].align, Align::Right);
        let mut short = RichState::default();
        short.sync("ab\ncd");
        invariant(&short, "ab\ncd");
        let rich = short.rich("ab\ncd");
        assert_eq!(rich.to_markup(), "<p>ab</p>\n<p>cd</p>");
    }

    #[test]
    fn align_and_bullets_act_on_every_touched_line() {
        let text = "a\nb\nc";
        let mut s = state(text);
        // Selection from line 0 into line 1; one ending at a line start
        // does not touch it.
        s.set_align(text, 0, 3, Align::Center);
        assert_eq!(
            s.paras.iter().map(|p| p.align).collect::<Vec<_>>(),
            [Align::Center, Align::Center, Align::Left]
        );
        s.set_align(text, 0, 2, Align::Right);
        assert_eq!(s.paras[1].align, Align::Center);
        s.toggle_bullets(text, 0, 5);
        assert!(s.paras.iter().all(|p| p.bullet));
        assert!(s.state_at(text, 0, 5).bullets);
        s.toggle_bullets(text, 2, 2);
        assert!(!s.paras[1].bullet && s.paras[0].bullet);
        // Mixed: turning on first.
        s.toggle_bullets(text, 0, 5);
        assert!(s.paras.iter().all(|p| p.bullet));
        s.toggle_bullets(text, 0, 5);
        assert!(s.paras.iter().all(|p| !p.bullet));
    }

    #[test]
    fn the_tool_state_follows_the_caret_and_selection() {
        let text = "ab cd";
        let mut s = state(text);
        s.toggle(text, 0, 2, BOLD);
        s.paras[0].align = Align::Right;
        let at = |s: &RichState, a, b| s.state_at(text, a, b);
        assert!(at(&s, 0, 2).bold && !at(&s, 0, 2).italic);
        assert!(!at(&s, 0, 5).bold, "not every character");
        // A bare caret reads the character before it.
        assert!(at(&s, 2, 2).bold);
        assert!(!at(&s, 4, 4).bold);
        assert_eq!(at(&s, 0, 0).align, Align::Right);
    }

    #[test]
    fn segments_split_at_styles_selection_and_caret() {
        let line = chars("abcd");
        let styles = [0, BOLD, BOLD, 0];
        let segs = line_segments(&line, &styles, 10, (0, 0), None);
        assert_eq!(
            segs.iter()
                .map(|s| (s.text.as_str(), s.bold))
                .collect::<Vec<_>>(),
            [("a", false), ("bc", true), ("d", false)]
        );
        // The caret inside a run splits it and marks the second half.
        let segs = line_segments(&line, &styles, 10, (0, 0), Some(12));
        assert_eq!(
            segs.iter()
                .map(|s| (s.text.as_str(), s.caret_before))
                .collect::<Vec<_>>(),
            [("a", false), ("b", false), ("c", true), ("d", false)]
        );
        // A selection across runs.
        let segs = line_segments(&line, &styles, 10, (11, 13), Some(13));
        assert_eq!(
            segs.iter()
                .map(|s| (s.text.as_str(), s.selected, s.caret_before))
                .collect::<Vec<_>>(),
            [("a", false, false), ("bc", true, false), ("d", false, true)]
        );
        // Caret at the line's end and on an empty line.
        let segs = line_segments(&line, &styles, 10, (0, 0), Some(14));
        assert_eq!(
            segs.last().map(|s| (s.text.as_str(), s.caret_before)),
            Some(("", true))
        );
        let segs = line_segments(&[], &[], 5, (0, 0), Some(5));
        assert_eq!(segs.len(), 1);
        assert!(segs[0].caret_before && segs[0].text.is_empty());
        // A caret on another line draws none.
        let segs = line_segments(&line, &styles, 10, (0, 0), Some(3));
        assert!(segs.iter().all(|s| !s.caret_before));
        let joined: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "abcd");
    }

    #[test]
    fn a_line_offset_is_its_gutter_and_alignment() {
        let left = Para::default();
        let centre = Para {
            align: Align::Center,
            bullet: false,
        };
        let right = Para {
            align: Align::Right,
            bullet: false,
        };
        let bullet = Para {
            align: Align::Left,
            bullet: true,
        };
        assert_eq!(line_offset(left, 100., 40.), 0.);
        assert_eq!(line_offset(centre, 100., 40.), 30.);
        assert_eq!(line_offset(right, 100., 40.), 60.);
        assert_eq!(line_offset(bullet, 100., 40.), BULLET_GUTTER);
        assert_eq!(
            line_offset(
                Para {
                    align: Align::Right,
                    bullet: true
                },
                100.,
                40.
            ),
            BULLET_GUTTER + 100. - BULLET_GUTTER - 40.
        );
        // A line wider than the box starts at its edge.
        assert_eq!(line_offset(right, 30., 40.), 0.);
    }
}
