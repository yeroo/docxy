//! Comment balloons (#2232): classic Word desktop's All Markup margin.
//!
//! A document with open comments shows them as soon as it opens, in a
//! markup area to the right of the page, inside the document view's scroll.
//! Each open comment anchored in the body gets a balloon in its author's
//! colour, level with its anchor's line and pushed down below the balloon
//! before it when they would overlap. A dotted connector runs from the
//! anchor along its line to the page's (Print Layout) or text column's (Web
//! Layout) right edge, then to the balloon. The anchored range is shaded in
//! the author's colour, with brackets at its ends that take no width.
//! Resolved comments get none of it; the Comments pane lists them.
//!
//! Anchors are only known once gpui has laid the text out, so this works
//! like the suite's other probes (#1895): each frame the anchors, the
//! balloons and the page strips record where they were painted
//! ([`State`]), in window coordinates; the end of the frame turns them into
//! scroll-content coordinates ([`Inputs`], scroll-invariant), and the next
//! frame lays the balloons out from those. When a frame's measurement
//! differs from what it was laid out with, one more frame is drawn.
//!
//! The pure pieces (markers, ranges, stacking, the connector, the date and
//! the author colours) are unit tested here.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use docxcore::comments::Comment;
use docxcore::model::{Block, Inline};
use gpui::{
    AnyElement, App, Bounds, Hsla, IntoElement, ParentElement, Pixels, Styled, Window, canvas, div,
    fill, point, px, size,
};

use crate::dark_page;

/// The vertical gap (px) between two stacked balloons.
pub(crate) const BALLOON_GAP: f32 = 6.0;
/// The room (px) between the markup area's edges and its balloons.
pub(crate) const STRIP_PAD: f32 = 12.0;
/// How far (px) below a balloon's top its connector meets it.
pub(crate) const BALLOON_LEAD: f32 = 10.0;
/// Web Layout's horizontal padding (px) on each side of the text column.
pub(crate) const FLOW_PAD: f32 = 48.0;
/// Web Layout's vertical padding (px) above and below the text.
pub(crate) const FLOW_PAD_Y: f32 = 28.0;

/// The markup area's width (px): 3 inches, zoomed.
pub(crate) fn markup_w(zoom: f32) -> f32 {
    zoom * 288.0
}

/// A balloon's width (px): the markup area less its padding on both sides.
pub(crate) fn balloon_w(zoom: f32) -> f32 {
    (markup_w(zoom) - 2.0 * STRIP_PAD).max(40.0)
}

// ---- markers and ranges ----------------------------------------------------

/// A comment marker in the body's XML.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Mark {
    Start,
    End,
    Reference,
}

/// The comment markers in a piece of raw XML, in order: `w:commentRangeStart`,
/// `w:commentRangeEnd` and `w:commentReference`, each with its `w:id`.
pub(crate) fn markers(xml: &str) -> Vec<(Mark, String)> {
    const OPEN: &str = "<w:comment";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = xml[from..].find(OPEN) {
        let at = from + rel;
        let rest = &xml[at + OPEN.len()..];
        let end = xml[at..].find('>').map_or(xml.len(), |e| at + e);
        let kind = if rest.starts_with("RangeStart") {
            Some(Mark::Start)
        } else if rest.starts_with("RangeEnd") {
            Some(Mark::End)
        } else if rest.starts_with("Reference") {
            Some(Mark::Reference)
        } else {
            None
        };
        if let (Some(kind), Some(id)) = (kind, attr(&xml[at..end], "w:id")) {
            out.push((kind, id));
        }
        from = end.max(at + 1);
    }
    out
}

/// The value of attribute `name` in tag text `tag`, either quote.
fn attr(tag: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(rel) = tag[from..].find(name) {
        let at = from + rel;
        let before = tag[..at].chars().next_back();
        let rest = tag[at + name.len()..].trim_start();
        if before.is_some_and(char::is_whitespace) {
            if let Some(rest) = rest.strip_prefix('=') {
                let rest = rest.trim_start();
                let q = rest.chars().next()?;
                if q == '"' || q == '\'' {
                    let body = &rest[1..];
                    return body.find(q).map(|e| body[..e].to_string());
                }
            }
        }
        from = at + name.len();
    }
    None
}

/// The markers of a paragraph's content, hyperlinks and tracked changes
/// opened up, in order.
fn inline_markers(content: &[Inline], f: &mut impl FnMut(Mark, String)) {
    for inline in content {
        match inline {
            Inline::Raw(xml) => {
                for (k, id) in markers(xml) {
                    f(k, id);
                }
            }
            Inline::Hyperlink(h) => inline_markers(&h.content, f),
            Inline::Revision { content, .. } => inline_markers(content, f),
            _ => {}
        }
    }
}

/// Where the body's comments are, from one walk of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BodyComments {
    /// Each comment id with a marker in the body, in order of its first one.
    pub order: Vec<String>,
    /// For each paragraph (by its render path: top-level `[i]`, in a cell
    /// `[i, row, cell, k, ..]`), the ids whose range is open at its start,
    /// innermost last. Paragraphs with none are left out.
    pub open_at: HashMap<Vec<usize>, Vec<String>>,
}

/// [`BodyComments`] of `body`: ranges run across paragraphs and cells.
pub(crate) fn body_comments(body: &[Block]) -> BodyComments {
    let mut out = BodyComments::default();
    let mut open = Vec::new();
    walk(body, &mut Vec::new(), &mut open, &mut out);
    out
}

fn walk(blocks: &[Block], path: &mut Vec<usize>, open: &mut Vec<String>, out: &mut BodyComments) {
    for (i, b) in blocks.iter().enumerate() {
        path.push(i);
        match b {
            Block::Paragraph(p) => {
                if !open.is_empty() {
                    out.open_at.insert(path.clone(), open.clone());
                }
                inline_markers(&p.content, &mut |k, id| step(k, id, open, &mut out.order));
            }
            Block::Table(t) => {
                for (ri, row) in t.rows.iter().enumerate() {
                    for (ci, cell) in row.cells.iter().enumerate() {
                        path.extend_from_slice(&[ri, ci]);
                        walk(&cell.blocks, path, open, out);
                        path.truncate(path.len() - 2);
                    }
                }
            }
            Block::Raw(xml) => {
                for (k, id) in markers(xml) {
                    step(k, id, open, &mut out.order);
                }
            }
            Block::SectionProperties(_) => {}
        }
        path.pop();
    }
}

/// One marker of the walk: a start opens its range, an end closes it.
fn step(kind: Mark, id: String, open: &mut Vec<String>, order: &mut Vec<String>) {
    if !order.contains(&id) {
        order.push(id.clone());
    }
    match kind {
        Mark::Start => {
            if !open.contains(&id) {
                open.push(id);
            }
        }
        Mark::End => open.retain(|o| *o != id),
        Mark::Reference => {}
    }
}

/// The comments that get a balloon, in body order: those in `live` (the
/// tab's listed comments) that are open and have a marker in the body.
pub(crate) fn margin_comments<'a>(live: &'a [Comment], order: &[String]) -> Vec<&'a Comment> {
    order
        .iter()
        .filter_map(|id| live.iter().find(|c| c.id == *id && !c.resolved))
        .collect()
}

// ---- colours ---------------------------------------------------------------

/// Word-like per-author colours: `(balloon fill, edge, range shade)`. Red,
/// blue, green, violet, orange, teal, pink, olive. Word's exact values have
/// no oracle in this repository; these keep its idea (a pale fill with a
/// saturated edge of one hue per reviewer).
const AUTHOR_COLORS: [(u32, u32, u32); 8] = [
    (0xFBE3E3, 0xC00000, 0xF8D2D2),
    (0xDDEBF7, 0x2E74B5, 0xCFE2F3),
    (0xE2EFD9, 0x538135, 0xD5E8C8),
    (0xEADCF4, 0x7030A0, 0xE0CDEF),
    (0xFBE5D6, 0xC55A11, 0xF9D9C3),
    (0xD9F0F0, 0x1F7A7A, 0xCAE9E9),
    (0xF8DDEE, 0xC0398B, 0xF4CFE6),
    (0xEFEFD0, 0x7F7F00, 0xE8E8BE),
];

/// Balloon text on a light page.
const BALLOON_INK: u32 = 0x1A1A1A;
/// A balloon's date on a light page.
const BALLOON_DIM: u32 = 0x595959;

/// Each author's colour index: their order of first appearance among all of
/// the document's comments (resolved ones too, so resolving one never
/// recolours another), modulo the series' length.
pub(crate) fn author_index(comments: &[Comment], author: &str) -> usize {
    let mut seen: Vec<&str> = Vec::new();
    for c in comments {
        if !seen.contains(&c.author.as_str()) {
            seen.push(&c.author);
        }
    }
    seen.iter().position(|a| *a == author).unwrap_or(0) % AUTHOR_COLORS.len()
}

/// How one author's balloons, connectors and ranges are drawn (`0xRRGGBB`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Look {
    pub fill: u32,
    pub edge: u32,
    pub shade: u32,
    /// The shade of the selected comment's range: stronger.
    pub shade_sel: u32,
    pub ink: u32,
    pub dim: u32,
}

/// The [`Look`] of colour `index`, on a dark page (`dark`, ground `ground`)
/// through [`dark_page`]'s mappings.
pub(crate) fn look(index: usize, dark: bool, ground: u32) -> Look {
    let (fill, edge, shade) = AUTHOR_COLORS[index % AUTHOR_COLORS.len()];
    let shade_sel = dark_page::blend(shade, edge, 0.25);
    if !dark {
        return Look {
            fill,
            edge,
            shade,
            shade_sel,
            ink: BALLOON_INK,
            dim: BALLOON_DIM,
        };
    }
    let fill = dark_page::fill(fill);
    let edge = dark_page::border(Some(edge), ground);
    let shade_d = under_light_ink(dark_page::fill(shade));
    // The selected shade is stronger on a dark page too: toward the edge,
    // and still a ground the page's light ink reads on.
    let shade_sel = under_light_ink(dark_page::blend(shade_d, edge, 0.3));
    Look {
        fill,
        edge,
        shade: shade_d,
        shade_sel,
        ink: dark_page::text(BALLOON_INK, fill),
        dim: dark_page::text(BALLOON_DIM, fill),
    }
}

/// A dark page's range shade `c`, darkened toward the page until the page's
/// automatic (light) ink reads on it at the text floor: a mapped green is
/// lighter than a mapped red of the same lightness.
fn under_light_ink(c: u32) -> u32 {
    let mut out = c;
    let mut t = 0.0;
    while dark_page::contrast(dark_page::AUTO_DARK, out) < dark_page::TEXT_FLOOR && t < 1.0 {
        t += 0.1;
        out = dark_page::blend(c, dark_page::PAGE_DARK, t);
    }
    out
}

// ---- the date --------------------------------------------------------------

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A comment's `w:date` as Word shows it on a balloon, `November 17, 2026 at
/// 6:41 PM`, seen from a zone `offset_min` minutes ahead of UTC. The date is
/// read as UTC. `None` for a date that does not parse.
pub(crate) fn balloon_date(iso: &str, offset_min: i64) -> Option<String> {
    let t = docxcore::field::parse_iso(iso)?;
    if !(1..=12).contains(&t.month) || t.day == 0 || t.day > 31 || t.hour > 23 || t.min > 59 {
        return None;
    }
    let secs = days_from_civil(i64::from(t.year), t.month, t.day) * 86_400
        + i64::from(t.hour) * 3600
        + i64::from(t.min) * 60
        + i64::from(t.sec)
        + offset_min * 60;
    let l = docxcore::field::civil_from_unix(secs);
    let (h12, ampm) = match l.hour {
        0 => (12, "AM"),
        h @ 1..=11 => (h, "AM"),
        12 => (12, "PM"),
        h => (h - 12, "PM"),
    };
    Some(format!(
        "{} {}, {} at {}:{:02} {}",
        MONTHS[(l.month - 1) as usize],
        l.day,
        l.year,
        h12,
        l.min,
        ampm
    ))
}

// ---- layout ----------------------------------------------------------------

/// The balloons' tops (px, content coordinates), in order: each at its
/// anchor's line top, or below the balloon before it with `gap` between.
pub(crate) fn stack(anchors: &[f32], heights: &[f32], gap: f32) -> Vec<f32> {
    let mut out: Vec<f32> = Vec::with_capacity(anchors.len());
    for (i, &a) in anchors.iter().enumerate() {
        let y = match i {
            0 => a,
            _ => a.max(out[i - 1] + heights.get(i - 1).copied().unwrap_or(0.0) + gap),
        };
        out.push(y);
    }
    out
}

/// A connector's three points: the anchor point, the point on the right edge
/// (`edge_x`) level with it, and the point on the balloon's left edge
/// [`BALLOON_LEAD`] below its top.
pub(crate) fn connector(
    anchor: (f32, f32),
    edge_x: f32,
    balloon_left: f32,
    balloon_top: f32,
) -> [(f32, f32); 3] {
    [
        anchor,
        (edge_x, anchor.1),
        (balloon_left, balloon_top + BALLOON_LEAD),
    ]
}

// ---- the frame's probes ----------------------------------------------------

/// A rect in window coordinates (px).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn of(b: Bounds<Pixels>) -> Rect {
        Rect {
            x: f32::from(b.origin.x),
            y: f32::from(b.origin.y),
            w: f32::from(b.size.width),
            h: f32::from(b.size.height),
        }
    }

    pub(crate) fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// An anchor marker as painted this frame: a zero-width span `line_h` tall
/// in its line's middle at `mid` (window coordinates).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PaintedMark {
    pub id: String,
    pub kind: Mark,
    pub x: f32,
    pub mid: f32,
    pub line_h: f32,
}

/// An anchor in scroll-content coordinates: the point the connector starts
/// at (`x`, `mid`), its line's top and height, and its page (Print Layout).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Anchor {
    pub x: f32,
    pub mid: f32,
    pub line_top: f32,
    pub line_h: f32,
    pub page: usize,
}

/// What a frame lays the balloons out from, in scroll-content coordinates:
/// the last painted frame's anchors, balloon heights, page strip tops and the
/// content's bottom. `key` names the tab and view it was measured in.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Inputs {
    pub key: Option<(usize, bool)>,
    pub anchors: BTreeMap<String, Anchor>,
    pub heights: BTreeMap<String, f32>,
    pub page_tops: Vec<f32>,
    pub content_bottom: f32,
}

impl Inputs {
    /// Whether `other` lays out as this does: the same anchors, heights and
    /// pages, each within half a pixel.
    pub(crate) fn close_to(&self, other: &Inputs) -> bool {
        let near = |a: f32, b: f32| (a - b).abs() <= 0.5;
        self.key == other.key
            && self.anchors.len() == other.anchors.len()
            && self
                .anchors
                .iter()
                .zip(&other.anchors)
                .all(|((ia, a), (ib, b))| {
                    ia == ib
                        && near(a.x, b.x)
                        && near(a.line_top, b.line_top)
                        && near(a.mid, b.mid)
                        && a.page == b.page
                })
            && self.heights.len() == other.heights.len()
            && self
                .heights
                .iter()
                .zip(&other.heights)
                .all(|((ia, a), (ib, b))| ia == ib && near(*a, *b))
            && self.page_tops.len() == other.page_tops.len()
            && self
                .page_tops
                .iter()
                .zip(&other.page_tops)
                .all(|(a, b)| near(*a, *b))
            && near(self.content_bottom, other.content_bottom)
    }
}

/// A balloon as this frame drew it, for the connectors and the harness.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Drawn {
    pub id: String,
    pub author: String,
    pub date: String,
    pub date_utc: String,
    pub text: String,
    pub look: Option<Look>,
    pub selected: bool,
    pub page: usize,
}

/// The view a frame drew balloons in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum View {
    Print,
    Web,
}

/// The markup area's state, shared by the frame's probes and the next
/// render. Everything painted is in window coordinates and cleared when a
/// frame starts ([`State::begin`]).
#[derive(Debug, Default)]
pub(crate) struct State {
    /// The view the markup area is shown in this frame; `None`: not shown.
    pub view: Option<View>,
    /// The comments with a balloon this frame, in body order.
    pub drawn: Vec<Drawn>,
    /// How many of the document's listed comments are resolved (and so
    /// have no balloon).
    pub resolved_hidden: usize,
    /// The markup area's width (px).
    pub area_w: f32,
    pub marks: Vec<PaintedMark>,
    pub balloons: Vec<(String, Rect)>,
    /// Each balloon's buttons as painted: comment id, `resolve` or
    /// `delete`, rect.
    pub buttons: Vec<(String, &'static str, Rect)>,
    /// Print Layout: each page's strip (its left edge is the page's right).
    pub strips: Vec<Option<Rect>>,
    /// Web Layout: the text column's right edge.
    pub column_right: Option<f32>,
    /// The scroll content's top-left as painted.
    pub origin: Option<(f32, f32)>,
    /// The content's bottom (before the bottom room) as painted.
    pub content_bottom: Option<f32>,
    pub connectors: Vec<(String, [(f32, f32); 3])>,
    /// What this frame was laid out with.
    pub used: Inputs,
    /// What the last painted frame measured: the next frame's layout.
    pub last: Inputs,
}

/// The markup area's state, shared.
pub(crate) type Shared = Rc<RefCell<State>>;

impl State {
    /// Start a frame: drop what the last one painted, keep what it measured.
    pub(crate) fn begin(&mut self, view: Option<View>) {
        self.view = view;
        self.drawn.clear();
        self.resolved_hidden = 0;
        self.marks.clear();
        self.balloons.clear();
        self.buttons.clear();
        self.strips.clear();
        self.column_right = None;
        self.origin = None;
        self.content_bottom = None;
        self.connectors.clear();
    }

    /// The anchor of comment `id` as painted: its reference mark, else its
    /// range's end, else its start.
    pub(crate) fn painted_anchor(&self, id: &str) -> Option<&PaintedMark> {
        [Mark::Reference, Mark::End, Mark::Start]
            .into_iter()
            .find_map(|k| self.marks.iter().find(|m| m.id == id && m.kind == k))
    }

    /// The page (Print Layout) whose strip spans window `y`: the last one at
    /// or above it.
    fn page_at(&self, y: f32) -> usize {
        let mut page = 0;
        for (i, s) in self.strips.iter().enumerate() {
            if let Some(s) = s {
                if s.y <= y {
                    page = i;
                }
            }
        }
        page
    }

    /// This frame's [`Inputs`], from what it painted.
    pub(crate) fn measure(&self) -> Inputs {
        let Some((ox, oy)) = self.origin else {
            return Inputs {
                key: self.used.key,
                ..Inputs::default()
            };
        };
        let mut anchors = BTreeMap::new();
        for d in &self.drawn {
            if let Some(m) = self.painted_anchor(&d.id) {
                anchors.insert(
                    d.id.clone(),
                    Anchor {
                        x: m.x - ox,
                        mid: m.mid - oy,
                        line_top: m.mid - m.line_h / 2.0 - oy,
                        line_h: m.line_h,
                        page: self.page_at(m.mid),
                    },
                );
            }
        }
        Inputs {
            key: self.used.key,
            anchors,
            heights: self
                .balloons
                .iter()
                .map(|(id, r)| (id.clone(), r.h))
                .collect(),
            page_tops: self
                .strips
                .iter()
                .map(|s| s.map_or(0.0, |s| s.y - oy))
                .collect(),
            content_bottom: self.content_bottom.map_or(0.0, |b| b - oy),
        }
    }

    /// The connectors of the balloons painted this frame (window
    /// coordinates).
    fn connectors_now(&self) -> Vec<(String, [(f32, f32); 3])> {
        let mut out = Vec::new();
        for (id, r) in &self.balloons {
            let Some(m) = self.painted_anchor(id) else {
                continue;
            };
            let edge = match self.view {
                Some(View::Print) => self
                    .strips
                    .get(self.page_at(m.mid))
                    .copied()
                    .flatten()
                    .map(|s| s.x),
                Some(View::Web) => self.column_right,
                None => None,
            };
            if let Some(edge) = edge {
                out.push((id.clone(), connector((m.x, m.mid), edge, r.x, r.y)));
            }
        }
        out
    }
}

/// A balloon placed by the layout: its comment, its top in content
/// coordinates and the page it belongs to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Placed {
    pub id: String,
    pub top: f32,
    pub page: usize,
}

/// Place the balloons of `ids` from `inputs`: those with a measured anchor,
/// sorted by their anchor's line (then x), stacked; a balloon not measured
/// yet is `estimate(id)` tall. Also the room (px) the content needs below
/// its bottom for the last balloon to be reached.
pub(crate) fn place(
    ids: &[String],
    inputs: &Inputs,
    estimate: impl Fn(&str) -> f32,
) -> (Vec<Placed>, f32) {
    let mut have: Vec<(&String, Anchor)> = ids
        .iter()
        .filter_map(|id| inputs.anchors.get(id).map(|a| (id, *a)))
        .collect();
    have.sort_by(|a, b| {
        (a.1.line_top, a.1.x)
            .partial_cmp(&(b.1.line_top, b.1.x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let tops: Vec<f32> = have.iter().map(|(_, a)| a.line_top).collect();
    let heights: Vec<f32> = have
        .iter()
        .map(|(id, _)| {
            inputs
                .heights
                .get(*id)
                .copied()
                .unwrap_or_else(|| estimate(id))
        })
        .collect();
    let ys = stack(&tops, &heights, BALLOON_GAP);
    let room = match (ys.last(), heights.last()) {
        (Some(y), Some(h)) if inputs.content_bottom > 0.0 => {
            (y + h - inputs.content_bottom).max(0.0)
        }
        _ => 0.0,
    };
    let placed = have
        .iter()
        .zip(ys)
        .map(|((id, a), top)| Placed {
            id: (*id).clone(),
            top,
            page: a.page,
        })
        .collect();
    (placed, room)
}

/// A balloon's height (px) before it was measured: its header and date
/// lines and its text's lines at about 7 px a character, zoomed.
pub(crate) fn estimate_height(text: &str, zoom: f32) -> f32 {
    let per_line = (balloon_w(zoom) / (7.0 * zoom)).max(8.0) as usize;
    let lines: usize = text
        .lines()
        .map(|l| l.chars().count().div_ceil(per_line).max(1))
        .sum::<usize>()
        .max(1);
    zoom * (35.0 + 17.0 * lines as f32) + 12.0
}

// ---- elements --------------------------------------------------------------

/// The per-frame comment context the body's paragraphs draw with: which
/// ranges are open where, each margin comment's look, the selected one, and
/// where anchors go.
pub(crate) struct CommentCtx {
    pub open_at: HashMap<Vec<usize>, Vec<String>>,
    pub looks: HashMap<String, Look>,
    pub selected: Option<String>,
    pub state: Shared,
}

impl CommentCtx {
    /// The shade a run draws on with the ranges `open` open (innermost last):
    /// the innermost margin comment's, stronger when it is the selected one.
    pub(crate) fn shade(&self, open: &[String]) -> Option<u32> {
        open.iter().rev().find_map(|id| {
            self.looks.get(id).map(|l| {
                if self.selected.as_deref() == Some(id.as_str()) {
                    l.shade_sel
                } else {
                    l.shade
                }
            })
        })
    }

    /// The zero-width span a marker of a margin comment draws: a probe that
    /// records where it is painted, and the range's bracket at a start or an
    /// end. `None` for a comment without a balloon. It takes no width, and
    /// is `caret_h` tall (the caret bar's height, which never inflates a
    /// line) in a row that centres its items.
    pub(crate) fn marker_el(
        &self,
        kind: Mark,
        id: &str,
        caret_h: f32,
        line_h: f32,
    ) -> Option<AnyElement> {
        let look = self.looks.get(id)?;
        let state = self.state.clone();
        let rec_id = id.to_string();
        let probe = canvas(
            move |b: Bounds<Pixels>, _w: &mut Window, _a: &mut App| {
                let r = Rect::of(b);
                state.borrow_mut().marks.push(PaintedMark {
                    id: rec_id.clone(),
                    kind,
                    x: r.x,
                    mid: r.y + r.h / 2.0,
                    line_h,
                });
            },
            |_b, _s, _w, _a| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let edge = hsla(look.edge);
        let bracket = match kind {
            Mark::Start => Some(
                div()
                    .absolute()
                    .top_0()
                    .left(px(-1.))
                    .w(px(3.))
                    .h_full()
                    .border_l_1()
                    .border_t_1()
                    .border_b_1()
                    .border_color(edge),
            ),
            Mark::End => Some(
                div()
                    .absolute()
                    .top_0()
                    .right(px(-1.))
                    .w(px(3.))
                    .h_full()
                    .border_r_1()
                    .border_t_1()
                    .border_b_1()
                    .border_color(edge),
            ),
            Mark::Reference => None,
        };
        Some(
            div()
                .relative()
                .flex_none()
                .w(px(0.))
                .h(px(caret_h))
                .child(probe)
                .children(bracket)
                .into_any_element(),
        )
    }
}

/// `0xRRGGBB` as a colour.
fn hsla(c: u32) -> Hsla {
    gpui::rgb(c).into()
}

/// A canvas that records `rect` into the state at prepaint, through `put`.
/// Its parent must be `relative`.
pub(crate) fn rect_probe(state: &Shared, put: impl Fn(&mut State, Rect) + 'static) -> AnyElement {
    let state = state.clone();
    canvas(
        move |b: Bounds<Pixels>, _w: &mut Window, _a: &mut App| {
            put(&mut state.borrow_mut(), Rect::of(b))
        },
        |_b, _s, _w, _a| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

/// Print Layout's markup area beside page `pi` (#2232): a strip `w` px wide
/// that only reserves the room (no height, no background), recording where
/// it is painted, with the page's balloons absolutely placed in it.
pub(crate) fn strip(state: &Shared, pi: usize, w: f32, balloons: Vec<AnyElement>) -> AnyElement {
    div()
        .relative()
        .flex_none()
        .w(px(w))
        .h(px(0.))
        .child(rect_probe(state, move |s, r| {
            if s.strips.len() <= pi {
                s.strips.resize(pi + 1, None);
            }
            s.strips[pi] = Some(r);
        }))
        .children(balloons)
        .into_any_element()
}

/// The content's bottom: an in-flow spacer `room` px tall after the content,
/// whose top is recorded.
pub(crate) fn bottom_room(state: &Shared, room: f32) -> AnyElement {
    div()
        .relative()
        .flex_none()
        .w_full()
        .h(px(room))
        .child(rect_probe(state, |s, r| s.content_bottom = Some(r.y)))
        .into_any_element()
}

/// A zero-size absolute layer at `(left, top)` px in its parent (the
/// content box's origin): a scroll area sizes its content from its
/// children's bounds, so a layer as wide as the view would add a sideways
/// scroll (#2232).
pub(crate) fn layer_at(left: f32, top: f32, children: Vec<AnyElement>) -> AnyElement {
    div()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(0.))
        .h(px(0.))
        .children(children)
        .into_any_element()
}

/// The canvas laid last in the scroll content, at the content's origin (in
/// a [`layer_at`] there): at prepaint it takes that origin (and in Web
/// Layout the text column's right edge, `column_w` px right of it),
/// measures the frame's [`Inputs`] and asks for another frame when they
/// differ from what this one was laid out with; at paint it draws the
/// connectors, dotted, or solid and wider for the selected comment.
pub(crate) fn end_layer(state: &Shared, column_w: Option<f32>) -> AnyElement {
    let st = state.clone();
    let paint_st = state.clone();
    canvas(
        move |b: Bounds<Pixels>, window: &mut Window, _a: &mut App| {
            let mut s = st.borrow_mut();
            s.origin = Some((f32::from(b.origin.x), f32::from(b.origin.y)));
            if let Some(w) = column_w {
                s.column_right = Some(f32::from(b.origin.x) + w);
            }
            let now = s.measure();
            if !now.close_to(&s.used) {
                window.request_animation_frame();
            }
            s.last = now;
            s.connectors = s.connectors_now();
        },
        move |_b, _s, window: &mut Window, _a: &mut App| {
            let s = paint_st.borrow();
            for (id, pts) in &s.connectors {
                let Some(d) = s.drawn.iter().find(|d| d.id == *id) else {
                    continue;
                };
                let Some(look) = d.look else {
                    continue;
                };
                let color = hsla(look.edge);
                let (step, dot) = if d.selected { (1.0, 2.0) } else { (3.0, 1.0) };
                for seg in pts.windows(2) {
                    paint_segment(window, seg[0], seg[1], step, dot, color);
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

/// Paint the segment `a`→`b` as `dot`-px squares every `step` px.
fn paint_segment(window: &mut Window, a: (f32, f32), b: (f32, f32), step: f32, dot: f32, c: Hsla) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    let n = (len / step).floor() as usize;
    for i in 0..=n {
        let t = if len > 0.0 {
            i as f32 * step / len
        } else {
            0.0
        };
        let (x, y) = (a.0 + dx * t, a.1 + dy * t);
        window.paint_quad(fill(
            Bounds::new(
                point(px(x - dot / 2.0), px(y - dot / 2.0)),
                size(px(dot), px(dot)),
            ),
            c,
        ));
    }
}

/// Whether the painted balloons follow the stacking rule, from the painted
/// rects alone (±1 px): in order of their tops, each is at
/// `max(its anchor's line top, the previous bottom + gap)`, and none
/// overlaps the one before. True with no balloons.
pub(crate) fn stacked_ok(painted: &[(Rect, f32)]) -> bool {
    let mut sorted: Vec<&(Rect, f32)> = painted.iter().collect();
    sorted.sort_by(|a, b| {
        a.0.y
            .partial_cmp(&b.0.y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut prev: Option<Rect> = None;
    for (r, line_top) in sorted {
        let want = match prev {
            Some(p) => line_top.max(p.bottom() + BALLOON_GAP),
            None => *line_top,
        };
        if (r.y - want).abs() > 1.0 || prev.is_some_and(|p| r.y < p.bottom()) {
            return false;
        }
        prev = Some(*r);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::model::{Paragraph, Run};

    fn para(content: Vec<Inline>) -> Block {
        Block::Paragraph(Paragraph {
            content,
            ..Paragraph::default()
        })
    }

    fn text(t: &str) -> Inline {
        Inline::Run(Run {
            text: t.into(),
            ..Run::default()
        })
    }

    fn raw(x: &str) -> Inline {
        Inline::Raw(x.into())
    }

    fn comment(id: &str, author: &str, resolved: bool) -> Comment {
        Comment {
            id: id.into(),
            author: author.into(),
            resolved,
            ..Comment::default()
        }
    }

    #[test]
    fn markers_read_every_kind_in_order_2232() {
        let xml = "<w:commentRangeEnd w:id=\"3\"/><w:r><w:rPr><w:rStyle \
                   w:val=\"CommentReference\"/></w:rPr><w:commentReference w:id='3'/></w:r>\
                   <w:commentRangeStart w:id=\"4\"/>";
        assert_eq!(
            markers(xml),
            vec![
                (Mark::End, "3".to_string()),
                (Mark::Reference, "3".to_string()),
                (Mark::Start, "4".to_string()),
            ]
        );
        assert!(markers("<w:comments/><w:t>x</w:t>").is_empty());
    }

    #[test]
    fn stack_follows_anchor_or_pushes_below_2232() {
        // Far apart: each at its anchor.
        assert_eq!(stack(&[10.0, 200.0], &[50.0, 50.0], 6.0), vec![10.0, 200.0]);
        // The same line: the second is pushed below the first.
        assert_eq!(stack(&[10.0, 10.0], &[50.0, 30.0], 6.0), vec![10.0, 66.0]);
        // A push carries on to the next, which is below its own anchor.
        assert_eq!(
            stack(&[10.0, 12.0, 80.0], &[50.0, 30.0, 20.0], 6.0),
            vec![10.0, 66.0, 102.0]
        );
        assert!(stack(&[], &[], 6.0).is_empty());
    }

    #[test]
    fn connector_runs_along_the_line_to_the_edge_then_the_balloon_2232() {
        let pts = connector((100.0, 40.0), 700.0, 712.0, 90.0);
        assert_eq!(pts[0], (100.0, 40.0), "starts at the anchor");
        assert_eq!(pts[1], (700.0, 40.0), "along the anchor's line to the edge");
        assert_eq!(
            pts[2],
            (712.0, 90.0 + BALLOON_LEAD),
            "on the balloon's left edge"
        );
    }

    #[test]
    fn balloon_date_is_words_format_in_local_time_2232() {
        let d = "2026-11-17T18:41:00Z";
        assert_eq!(
            balloon_date(d, 0).as_deref(),
            Some("November 17, 2026 at 6:41 PM")
        );
        // Four hours ahead: past midnight, the next day.
        assert_eq!(
            balloon_date(d, 240).as_deref(),
            Some("November 17, 2026 at 10:41 PM")
        );
        assert_eq!(
            balloon_date(d, 360).as_deref(),
            Some("November 18, 2026 at 12:41 AM")
        );
        // Behind UTC, and noon.
        assert_eq!(
            balloon_date("2026-11-17T12:00:00Z", -300).as_deref(),
            Some("November 17, 2026 at 7:00 AM")
        );
        assert_eq!(
            balloon_date("2026-11-17T12:05:00Z", 0).as_deref(),
            Some("November 17, 2026 at 12:05 PM")
        );
        // Across a year's end.
        assert_eq!(
            balloon_date("2026-01-01T02:00:00Z", -180).as_deref(),
            Some("December 31, 2025 at 11:00 PM")
        );
        assert_eq!(balloon_date("", 0), None);
        assert_eq!(balloon_date("not a date", 0), None);
    }

    #[test]
    fn author_colours_follow_first_appearance_2232() {
        let all = vec![
            comment("1", "Avery Lane", true),
            comment("2", "Robin Hale", false),
            comment("3", "Avery Lane", false),
        ];
        // A resolved comment still counts: Avery is first.
        assert_eq!(author_index(&all, "Avery Lane"), 0);
        assert_eq!(author_index(&all, "Robin Hale"), 1);
        // Two authors draw in two colours.
        assert_ne!(look(0, false, 0xFFFFFF), look(1, false, 0xFFFFFF));
        // Nine authors wrap around the series.
        let many: Vec<Comment> = (0..9)
            .map(|i| comment(&i.to_string(), &format!("A{i}"), false))
            .collect();
        assert_eq!(author_index(&many, "A8"), 0);
    }

    #[test]
    fn dark_looks_read_on_their_fill_2232() {
        for i in 0..AUTHOR_COLORS.len() {
            let light = look(i, false, 0xFFFFFF);
            assert!(dark_page::contrast(light.ink, light.fill) >= 4.5);
            // Black text reads on a light shade, selected or not.
            assert!(
                dark_page::contrast(0x000000, light.shade_sel) >= 4.5,
                "author {i}"
            );
            assert_ne!(light.shade_sel, light.shade);
            let dark = look(i, true, dark_page::PAGE_DARK);
            assert_ne!(dark.fill, light.fill, "a dark page maps the fill");
            assert!(
                dark_page::contrast(dark.ink, dark.fill) >= 4.5,
                "author {i}"
            );
            // The page's light ink reads on a dark shade, selected or not.
            let ink = dark_page::AUTO_DARK;
            assert!(dark_page::contrast(ink, dark.shade) >= 4.5, "author {i}");
            assert!(
                dark_page::contrast(ink, dark.shade_sel) >= 4.5,
                "author {i}"
            );
            assert_ne!(dark.shade_sel, dark.shade, "author {i}");
        }
    }

    #[test]
    fn open_at_tracks_ranges_across_paragraphs_and_cells_2232() {
        use docxcore::model::{Cell, Row, Table};
        let cell = Cell {
            blocks: vec![para(vec![
                text("in cell"),
                raw("<w:commentRangeEnd w:id=\"1\"/>"),
            ])],
            ..Cell::default()
        };
        let table = Block::Table(Table {
            rows: vec![Row {
                cells: vec![Cell::default(), cell],
                ..Row::default()
            }],
            ..Table::default()
        });
        let body = vec![
            para(vec![raw("<w:commentRangeStart w:id=\"1\"/>"), text("a")]),
            para(vec![
                raw("<w:commentRangeStart w:id=\"2\"/>"),
                text("b"),
                raw("<w:commentRangeEnd w:id=\"2\"/>"),
                raw("<w:r><w:commentReference w:id=\"2\"/></w:r>"),
            ]),
            table,
            para(vec![text("after")]),
        ];
        let bc = body_comments(&body);
        assert_eq!(bc.order, vec!["1".to_string(), "2".to_string()]);
        assert_eq!(bc.open_at.get(&vec![0]), None, "opens inside the paragraph");
        assert_eq!(bc.open_at.get(&vec![1]), Some(&vec!["1".to_string()]));
        // Through the table's first cell into the second's paragraph.
        assert_eq!(
            bc.open_at.get(&vec![2, 0, 1, 0]),
            Some(&vec!["1".to_string()])
        );
        assert_eq!(bc.open_at.get(&vec![3]), None, "closed in the cell");
    }

    #[test]
    fn margin_comments_skip_resolved_and_unanchored_2232() {
        let live = vec![
            comment("5", "A", false),
            comment("1", "A", true),
            comment("2", "B", false),
            comment("9", "B", false),
        ];
        // Body order, not the list's; 9 has no marker in the body.
        let order = vec!["2".to_string(), "1".to_string(), "5".to_string()];
        let ids: Vec<&str> = margin_comments(&live, &order)
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(ids, vec!["2", "5"]);
    }

    #[test]
    fn place_sorts_by_line_and_leaves_room_below_2232() {
        let anchor = |top: f32, x: f32| Anchor {
            x,
            mid: top + 8.0,
            line_top: top,
            line_h: 16.0,
            page: 0,
        };
        let inputs = Inputs {
            anchors: [
                ("a".to_string(), anchor(100.0, 50.0)),
                ("b".to_string(), anchor(100.0, 20.0)),
            ]
            .into_iter()
            .collect(),
            heights: [("a".to_string(), 40.0)].into_iter().collect(),
            content_bottom: 120.0,
            ..Inputs::default()
        };
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let (placed, room) = place(&ids, &inputs, |_| 30.0);
        // b is left of a on the same line; c has no anchor yet.
        assert_eq!(
            placed.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        assert_eq!(placed[0].top, 100.0);
        assert_eq!(placed[1].top, 100.0 + 30.0 + BALLOON_GAP);
        assert_eq!(room, 136.0 + 40.0 - 120.0);
    }

    #[test]
    fn stacked_ok_reads_the_painted_rects_2232() {
        let r = |y: f32, h: f32| Rect {
            x: 0.0,
            y,
            w: 10.0,
            h,
        };
        assert!(stacked_ok(&[(r(10.0, 40.0), 10.0), (r(56.0, 20.0), 10.0)]));
        // At the previous bottom + gap although its own anchor is lower.
        assert!(!stacked_ok(&[(r(10.0, 40.0), 10.0), (r(56.0, 20.0), 90.0)]));
        // Overlapping.
        assert!(!stacked_ok(&[(r(10.0, 40.0), 10.0), (r(30.0, 20.0), 10.0)]));
        assert!(stacked_ok(&[]));
    }

    #[test]
    fn inputs_close_to_ignores_half_pixels_2232() {
        let mut a = Inputs::default();
        a.heights.insert("1".into(), 40.0);
        let mut b = a.clone();
        b.heights.insert("1".into(), 40.4);
        assert!(a.close_to(&b));
        b.heights.insert("1".into(), 41.0);
        assert!(!a.close_to(&b));
    }
}
