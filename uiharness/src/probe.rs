//! Reading a picture: is there a line along this edge, is it solid or dashed,
//! and is it the colour it is supposed to be.
//!
//! Everything here is pure — an [`Image`] in, a reading out — because these are
//! the decisions a test's verdict rests on, and a decision that needs a window
//! to reproduce is one nobody can debug. The harness supplies the picture
//! (`capture`), the app supplies the rectangle (`rect`), and this module is
//! what turns the pixels inside that rectangle into a sentence.
//!
//! ## What "an edge" means here
//!
//! A region's rectangle is the region's own box, and the app draws a selection
//! border straddling it: the border element is inset `-1px` and `2px` wide, so
//! of the two rows it covers along the top, one is inside the crop and one is
//! not. The same holds on every side. Rather than have every caller guess the
//! inset, [`probe_edge`] scans a shallow **band** inward from the edge and
//! keeps the line it finds — the row (or column) that best matches what was
//! asked for. A crop one pixel off therefore still reads the right line, and a
//! genuinely absent one still reads absent, because nothing in the band matches.
//!
//! ## Why a tolerance, and why a modal colour
//!
//! Borders are anti-aliased: a 2px teal line on white has a blended pixel on
//! its outer boundary and the nominal colour in its core. Matching exactly
//! would fail a true result, so [`color_matches`] allows a per-channel
//! tolerance — and because the band scan keeps the *best* line, the one it
//! keeps is the core, whose reported colour is the nominal one rather than a
//! blend.
//!
//! Alpha is deliberately not compared. A window capture has its alpha byte
//! forced opaque (`capture::read_bgra`), since GDI leaves there whatever
//! happened to be drawn — so comparing it would make every probe depend on a
//! byte no renderer chose.

use crate::image::Image;
use std::collections::HashMap;

/// A pixel: red, green, blue, alpha.
pub type Rgba = [u8; 4];

/// Which edge of a cropped region is being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

impl Side {
    /// Every side, in the order a border expectation reports them — the same
    /// order CSS names them, so a reader following a failure message against
    /// the style that drew it is not translating as they go.
    pub const ALL: [Side; 4] = [Side::Top, Side::Right, Side::Bottom, Side::Left];

    pub fn name(self) -> &'static str {
        match self {
            Side::Top => "top",
            Side::Right => "right",
            Side::Bottom => "bottom",
            Side::Left => "left",
        }
    }

    /// A side by name, or `None`. `all` is not a side and is not accepted
    /// here; the expectation parser handles that word, because "every side" is
    /// a question about how many probes to run, not about which one.
    pub fn parse(s: &str) -> Option<Side> {
        match s.to_ascii_lowercase().as_str() {
            "top" => Some(Side::Top),
            "right" => Some(Side::Right),
            "bottom" => Some(Side::Bottom),
            "left" => Some(Side::Left),
            _ => None,
        }
    }
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// How far a pixel may be, per channel, from a colour a test NAMED and still
/// count as that colour ([`Target::Color`]).
///
/// ⚠️ This is what keeps a colour assertion honest, and its ceiling is set by
/// the palette, not by how noisy captures are. A tolerance is a radius, so two
/// colours the grid draws that are `d` apart can BOTH match one pixel unless
/// it is under `d / 2` — and then `assert border … blue` passes on a border
/// the app drew in a different blue, which is the one failure a colour
/// assertion exists to rule out. The closest distinct pair the grid draws is
/// the chart values blue `#4472c4` and the first formula-reference blue
/// `#2f6fdb`, 23 apart; the two live in different palettes in main.rs and were
/// never compared across them, which is how this once sat at 28.
/// `no_two_drawn_colours_can_both_match` in `expect.rs` fails if a palette
/// change narrows the gap again.
///
/// It can afford to be this tight because the band scan keeps the BEST line,
/// whose core is the nominal colour exactly — the tolerance only has to cover
/// compositor rounding, not a blended edge.
pub const DEFAULT_TOL: u8 = 11;

/// How far a pixel may be from the background and still count AS the
/// background ([`Target::NotColor`]).
///
/// ⚠️ Deliberately not [`DEFAULT_TOL`]: the two targets want opposite things
/// from a tolerance. "Is this pixel the blue I named" needs it tight enough to
/// separate two blues. "Is this pixel not paper" needs it WIDE, because every
/// near-paper pixel it fails to absorb — a blend against a soft shadow, the
/// compositor's rounding on a flat fill — reads as a line, and `no border …`
/// with no colour named starts failing on a blank region. Narrowing the one
/// constant to fix the first question would have quietly broken the second.
pub const BACKGROUND_TOL: u8 = 28;

/// How deep into the region to look for the edge's line. Four pixels covers a
/// 2px border straddling the boundary at any of the insets the grid uses, and
/// stops well short of a cell's text.
pub const DEFAULT_DEPTH: u32 = 4;

/// How many pixels to ignore at each end of a run. The first and last pixels of
/// the top edge belong to the left and right borders' corners, so a top edge
/// that is genuinely absent would otherwise report two stray hits.
pub const DEFAULT_MARGIN: u32 = 2;

/// The largest gap, in pixels, closed before a run is counted. One pixel of
/// drop-out in a solid line is anti-aliasing, not a dash; the shader's own gap
/// is `1 x border width` = 2px at the grid's border, so closing 1px cannot
/// turn a real dashed line into a solid reading.
const CLOSE_PX: usize = 1;

/// Runs shorter than this are speckle and are dropped. The shader's dash is
/// `2 x border width` = 4px, so this is well under a real dash.
const MIN_SEG: usize = 2;

/// At or below this fraction of the run, there is no line.
const ABSENT_MAX: f32 = 0.10;

/// At or above this fraction, in a single run, the line is solid.
const SOLID_MIN: f32 = 0.90;

/// The band a dashed line's coverage falls in. A `2W`/`1W` pattern covers two
/// thirds of its edge; the lower bound allows for an edge short enough to hold
/// only a dash or two, and for the anti-aliased ends of each dash falling
/// outside the colour tolerance (the grid's own dashed border measures 52-60%).
const DASH_MIN: f32 = 0.25;

/// The most of its edge a dashed reading may cover.
///
/// Not [`SOLID_MIN`], and the difference is the whole reason [`LineKind::Broken`]
/// exists. gpui's dash pattern is fixed — `2 x width` on, `1 x width` off — so a
/// duty cycle of two thirds is the *ceiling* for this shader's dashes, and 0.80
/// is that with headroom. A run covering 85% of its edge in eight pieces is a
/// line with holes in it, and calling it dashed would pass a test whose whole
/// subject is whether the app drew dashes.
const DASH_MAX: f32 = 0.80;

/// How many pieces a run must be in before it can be called dashed.
///
/// Two is a line with a hole in it, not a pattern — a pattern needs at least
/// two gaps to repeat. Nothing real is lost: the shortest edge the grid draws
/// is a 21px row, which at the shader's 4px dash and 2px gap still shows three
/// or four dashes.
const DASH_MIN_SEGS: usize = 3;

/// How far a dash (or a gap) may sit from the typical one and still count as
/// part of a pattern. Generous, because the run is trimmed at both ends by
/// [`DEFAULT_MARGIN`] and so may start or finish mid-dash; far tighter than the
/// spread of a line that is simply drawn in pieces.
const DASH_SPREAD: usize = 3;

/// How many values at each end of a sorted run of dashes (or gaps) are set
/// aside before its spread is measured: one in six, rounded down.
///
/// ⚠️ This exists because of something the grid does that no synthetic test
/// predicted. The pointed range's border is rendered **cell by cell**, so the
/// dash phase restarts at every cell boundary: the last dash before a boundary
/// is cut short and the first two after it can abut. Against the real thing
/// that reads as dashes of 2 to 7px on perfectly even 3px gaps — a plain
/// shortest-to-longest rule called the whole border `Broken`, which is the
/// harness saying "not dashed" about a border that is visibly dashed, the worst
/// answer it can give.
///
/// A fraction rather than a fixed count, and rounded DOWN, so that it is zero
/// for the small counts the rule was written for: three fragments are still
/// measured end to end and still read as fragments. The twenty pieces a real
/// cell-by-cell border produces can spare the three at its joins.
const TRIM_FRACTION: usize = 6;

/// The shortest and longest of `v` with [`TRIM_FRACTION`] set aside at each end.
fn trimmed_range(v: &[usize]) -> Option<(usize, usize)> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    let trim = s.len() / TRIM_FRACTION;
    let lo = trim;
    let hi = s.len() - trim - 1;
    Some((s[lo], s[hi]))
}

/// Whether the segments repeat evenly enough to be dashes rather than
/// fragments: no dash more than [`DASH_SPREAD`] times the shortest, and the
/// same of the gaps between them — both measured over the middle of the
/// distribution, with [`TRIM_FRACTION`] of the outliers set aside.
fn is_regular(segments: &[Segment]) -> bool {
    let even = |v: &[usize]| match trimmed_range(v) {
        Some((lo, hi)) => hi <= lo.max(1) * DASH_SPREAD,
        None => true,
    };
    let dashes: Vec<usize> = segments.iter().map(|s| s.len).collect();
    let gaps: Vec<usize> = segments
        .windows(2)
        .map(|w| w[1].start.saturating_sub(w[0].start + w[0].len))
        .collect();
    even(&dashes) && even(&gaps)
}

/// The distance between two colours: the largest per-channel difference over
/// red, green and blue. Alpha is not compared — see the module note.
pub fn color_dist(a: Rgba, b: Rgba) -> u8 {
    (0..3).map(|i| a[i].abs_diff(b[i])).max().unwrap_or(0)
}

/// Whether two colours are the same within `tol`.
pub fn color_matches(a: Rgba, b: Rgba, tol: u8) -> bool {
    color_dist(a, b) <= tol
}

/// A colour as `#rrggbb`, which is how a failure message names one.
pub fn hex(c: Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// What counts as "line" along the run being read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Pixels within tolerance of this colour. What an expectation that names a
    /// colour asks for.
    Color(Rgba),
    /// Pixels *further* than tolerance from this colour — the background. What
    /// an expectation that names no colour asks for: any line at all, whatever
    /// it is drawn in.
    NotColor(Rgba),
}

impl Target {
    /// `tol` is the NAMED-colour tolerance; the background question uses
    /// [`BACKGROUND_TOL`] instead, for the reason on that constant.
    fn hit(self, px: Rgba, tol: u8) -> bool {
        match self {
            Target::Color(c) => color_matches(px, c, tol),
            Target::NotColor(bg) => !color_matches(px, bg, BACKGROUND_TOL),
        }
    }
}

/// One stretch of matched pixels along a run: where it starts and how long it
/// is, both in pixels from the run's near end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    pub start: usize,
    pub len: usize,
}

/// What a run of pixels turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Nothing along this edge.
    Absent,
    /// One unbroken line.
    Solid,
    /// Repeating dashes.
    Dashed,
    /// Something is drawn, but it is neither a clean line nor a clean dash
    /// pattern — half an edge, or a line with a real hole in it.
    ///
    /// Not a category any expectation can ask for. It exists so that a reading
    /// nobody predicted is reported as itself rather than rounded to the
    /// nearest thing a test was hoping for, which is the failure mode that
    /// sends a reader back to installing a build.
    Broken,
}

impl LineKind {
    pub fn name(self) -> &'static str {
        match self {
            LineKind::Absent => "absent",
            LineKind::Solid => "solid",
            LineKind::Dashed => "dashed",
            LineKind::Broken => "broken",
        }
    }
}

impl std::fmt::Display for LineKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A run of pixels, read.
#[derive(Debug, Clone, PartialEq)]
pub struct LineReading {
    pub kind: LineKind,
    /// How many pixels were examined.
    pub len: usize,
    /// The stretches of line found, after closing anti-aliasing drop-outs and
    /// dropping speckle.
    pub segments: Vec<Segment>,
}

impl LineReading {
    /// How many of the examined pixels are line.
    pub fn covered(&self) -> usize {
        self.segments.iter().map(|s| s.len).sum()
    }

    /// The fraction of the run that is line, 0.0 to 1.0.
    pub fn coverage(&self) -> f32 {
        if self.len == 0 {
            return 0.0;
        }
        self.covered() as f32 / self.len as f32
    }

    /// The mean length of a dash, or 0.0 when there are none.
    pub fn dash_px(&self) -> f32 {
        if self.segments.is_empty() {
            return 0.0;
        }
        self.covered() as f32 / self.segments.len() as f32
    }

    /// The mean gap between dashes, or 0.0 when there are fewer than two.
    pub fn gap_px(&self) -> f32 {
        if self.segments.len() < 2 {
            return 0.0;
        }
        let gaps: usize = self.gaps().iter().sum();
        gaps as f32 / (self.segments.len() - 1) as f32
    }

    /// The shortest and longest run, and the shortest and longest gap between
    /// runs, in pixels: `(dash_min, dash_max, gap_min, gap_max)`.
    ///
    /// This is what a `broken` reading was rejected on and the one thing the
    /// count and the coverage cannot tell you — "20 runs covering 52%" reads
    /// exactly like a dashed line to anyone holding the message rather than the
    /// pixels. Reported for `Broken` so the reader can see whether they are
    /// looking at a pattern with one bad joint or at a line in pieces.
    pub fn spread(&self) -> (usize, usize, usize, usize) {
        let dashes: Vec<usize> = self.segments.iter().map(|s| s.len).collect();
        let gaps: Vec<usize> = self.gaps();
        let range = |v: &[usize]| {
            (
                v.iter().copied().min().unwrap_or(0),
                v.iter().copied().max().unwrap_or(0),
            )
        };
        let (dlo, dhi) = range(&dashes);
        let (glo, ghi) = range(&gaps);
        (dlo, dhi, glo, ghi)
    }

    /// The gaps between consecutive runs, in pixels.
    pub fn gaps(&self) -> Vec<usize> {
        self.segments
            .windows(2)
            .map(|w| w[1].start.saturating_sub(w[0].start + w[0].len))
            .collect()
    }

    /// The reading in one line, as a failure message prints it.
    pub fn describe(&self) -> String {
        match self.kind {
            LineKind::Absent => format!("absent ({}px examined)", self.len),
            LineKind::Solid => format!("solid ({:.0}% of {}px)", self.coverage() * 100.0, self.len),
            LineKind::Dashed => format!(
                "dashed ({} dashes of ~{:.1}px, gaps ~{:.1}px, {:.0}% of {}px)",
                self.segments.len(),
                self.dash_px(),
                self.gap_px(),
                self.coverage() * 100.0,
                self.len
            ),
            LineKind::Broken => {
                let (dlo, dhi, glo, ghi) = self.spread();
                format!(
                    "broken ({} runs of {dlo}-{dhi}px with gaps of {glo}-{ghi}px, \
                     covering {:.0}% of {}px)",
                    self.segments.len(),
                    self.coverage() * 100.0,
                    self.len
                )
            }
        }
    }
}

/// Turn a per-pixel hit mask into a reading. The pure core of every probe: one
/// pixel of drop-out is closed, speckle is dropped, and what is left is
/// classified by how much of the run it covers and in how many pieces.
pub fn classify(hits: &[bool]) -> LineReading {
    let mut segments: Vec<Segment> = Vec::new();
    let mut i = 0;
    while i < hits.len() {
        if !hits[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < hits.len() && hits[i] {
            i += 1;
        }
        segments.push(Segment {
            start,
            len: i - start,
        });
    }
    // Close drop-outs: a gap of at most CLOSE_PX joins its neighbours.
    let mut closed: Vec<Segment> = Vec::new();
    for s in segments {
        match closed.last_mut() {
            Some(prev) if s.start - (prev.start + prev.len) <= CLOSE_PX => {
                prev.len = s.start + s.len - prev.start;
            }
            _ => closed.push(s),
        }
    }
    closed.retain(|s| s.len >= MIN_SEG);

    let len = hits.len();
    let covered: usize = closed.iter().map(|s| s.len).sum();
    let coverage = if len == 0 {
        0.0
    } else {
        covered as f32 / len as f32
    };
    let kind = if coverage <= ABSENT_MAX {
        LineKind::Absent
    } else if closed.len() == 1 && coverage >= SOLID_MIN {
        LineKind::Solid
    } else if closed.len() >= DASH_MIN_SEGS
        && (DASH_MIN..DASH_MAX).contains(&coverage)
        && is_regular(&closed)
    {
        LineKind::Dashed
    } else {
        LineKind::Broken
    };
    LineReading {
        kind,
        len,
        segments: closed,
    }
}

/// How a probe is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeOpts {
    /// How many pixels inward from the edge to search for the line.
    pub depth: u32,
    /// The per-channel tolerance for a colour the test named. The background
    /// question does not use it (see [`BACKGROUND_TOL`]).
    pub tol: u8,
    /// How many pixels to skip at each end of the run.
    pub margin: u32,
}

impl Default for ProbeOpts {
    fn default() -> ProbeOpts {
        ProbeOpts {
            depth: DEFAULT_DEPTH,
            tol: DEFAULT_TOL,
            margin: DEFAULT_MARGIN,
        }
    }
}

/// One edge, read.
#[derive(Debug, Clone, PartialEq)]
pub struct LineProbe {
    pub side: Side,
    /// How many pixels inward from the edge the line was found. Reported
    /// because a border at an unexpected inset is worth seeing in a failure.
    pub offset: u32,
    pub reading: LineReading,
    /// The commonest colour among the matched pixels — the line's nominal
    /// colour, not one of its anti-aliased edges. `None` when nothing matched.
    pub color: Option<Rgba>,
}

impl LineProbe {
    /// The probe in one line, as a failure message prints it.
    pub fn describe(&self) -> String {
        match self.color {
            Some(c) => format!("{} {}", self.reading.describe(), hex(c)),
            None => self.reading.describe(),
        }
    }
}

/// The pixels along one line of the band, from the near end of the edge to the
/// far one, with `margin` skipped at each end. `offset` is measured inward.
fn run_at(img: &Image, side: Side, offset: u32, margin: u32) -> Vec<Rgba> {
    let (w, h) = (img.w, img.h);
    let span = |n: u32| -> std::ops::Range<u32> {
        let lo = margin.min(n);
        let hi = n.saturating_sub(margin).max(lo);
        lo..hi
    };
    match side {
        Side::Top => span(w).filter_map(|x| img.pixel(x, offset)).collect(),
        Side::Bottom => span(w)
            .filter_map(|x| img.pixel(x, h.saturating_sub(1 + offset)))
            .collect(),
        Side::Left => span(h).filter_map(|y| img.pixel(offset, y)).collect(),
        Side::Right => span(h)
            .filter_map(|y| img.pixel(w.saturating_sub(1 + offset), y))
            .collect(),
    }
}

/// The commonest colour among the pixels that matched — the line's nominal
/// colour. Ties go to the colour that appeared first, so the result does not
/// depend on the iteration order of a map.
fn modal(px: &[Rgba]) -> Option<Rgba> {
    let mut counts: HashMap<Rgba, (usize, usize)> = HashMap::new();
    for (i, p) in px.iter().enumerate() {
        let e = counts.entry(*p).or_insert((0, i));
        e.0 += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, (n, first))| (*n, std::cmp::Reverse(*first)))
        .map(|(c, _)| c)
}

/// Read one edge of a cropped region.
///
/// Scans `opts.depth` lines inward and keeps the one that best matches
/// `target` — best meaning most covered, and on a tie the one nearer the edge.
/// That is what lets a caller name a region without knowing how far the
/// renderer inset its border.
///
/// `Err` when the crop is too small to hold a run: an image narrower than the
/// margins, or shallower than the offset asked for. Reporting that is the
/// point — a zero-pixel run would otherwise classify as `Absent` and a test
/// would read "no border" when the truth is "no picture".
pub fn probe_edge(
    img: &Image,
    side: Side,
    target: Target,
    opts: ProbeOpts,
) -> Result<LineProbe, String> {
    let along = match side {
        Side::Top | Side::Bottom => img.w,
        Side::Left | Side::Right => img.h,
    };
    let across = match side {
        Side::Top | Side::Bottom => img.h,
        Side::Left | Side::Right => img.w,
    };
    if along <= opts.margin * 2 {
        return Err(format!(
            "the {}x{} region is too small to read its {side} edge: {along}px along it, \
             with {}px skipped at each end",
            img.w, img.h, opts.margin
        ));
    }
    if across == 0 {
        return Err(format!(
            "the {}x{} region has no pixels across its {side} edge",
            img.w, img.h
        ));
    }
    let depth = opts.depth.max(1).min(across);
    let mut best: Option<LineProbe> = None;
    for offset in 0..depth {
        let px = run_at(img, side, offset, opts.margin);
        let hits: Vec<bool> = px.iter().map(|p| target.hit(*p, opts.tol)).collect();
        let reading = classify(&hits);
        let matched: Vec<Rgba> = px
            .iter()
            .zip(&hits)
            .filter(|(_, h)| **h)
            .map(|(p, _)| *p)
            .collect();
        let probe = LineProbe {
            side,
            offset,
            color: modal(&matched),
            reading,
        };
        // Strictly greater, so a tie keeps the line nearer the edge.
        let better = match &best {
            None => true,
            Some(b) => probe.reading.covered() > b.reading.covered(),
        };
        if better {
            best = Some(probe);
        }
    }
    best.ok_or_else(|| format!("nothing to read along the {side} edge"))
}

/// The region's background: the commonest colour in its interior, with the
/// band the borders live in excluded on every side.
///
/// This is what an expectation that names no colour is measured against — "is
/// there a line here at all" only means anything relative to what the region is
/// mostly made of. Falls back to the whole image when the crop is too small to
/// have an interior, which is better than refusing: a 6px sliver still has a
/// commonest colour.
pub fn background_color(img: &Image, inset: u32) -> Rgba {
    let inner: Vec<Rgba> = if img.w > inset * 2 && img.h > inset * 2 {
        (inset..img.h - inset)
            .flat_map(|y| (inset..img.w - inset).filter_map(move |x| img.pixel(x, y)))
            .collect()
    } else {
        (0..img.h)
            .flat_map(|y| (0..img.w).filter_map(move |x| img.pixel(x, y)))
            .collect()
    };
    modal(&inner).unwrap_or([0, 0, 0, 255])
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEAL: Rgba = [0x2a, 0xa7, 0x9b, 0xff];
    const WHITE: Rgba = [0xff, 0xff, 0xff, 0xff];

    /// A white image with the given size.
    fn blank(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set_pixel(x, y, WHITE);
            }
        }
        img
    }

    /// Blend `c` over white at `a` (0.0..1.0) — one anti-aliased pixel.
    fn over_white(c: Rgba, a: f32) -> Rgba {
        let mix = |v: u8| (v as f32 * a + 255.0 * (1.0 - a)).round() as u8;
        [mix(c[0]), mix(c[1]), mix(c[2]), 0xff]
    }

    /// Draw a horizontal line `thick` px deep starting at row `y0`, `on` px of
    /// colour followed by `off` px of nothing, repeating. `off == 0` is solid.
    fn hline(img: &mut Image, y0: u32, thick: u32, c: Rgba, on: u32, off: u32) {
        let pitch = on + off;
        for x in 0..img.w {
            if off > 0 && x % pitch >= on {
                continue;
            }
            for y in y0..y0 + thick {
                img.set_pixel(x, y, c);
            }
        }
    }

    // ---- classify: the pure core ----------------------------------------

    #[test]
    fn an_unbroken_run_is_solid() {
        let r = classify(&[true; 40]);
        assert_eq!(r.kind, LineKind::Solid);
        assert_eq!(r.segments.len(), 1);
        assert_eq!(r.coverage(), 1.0);
        assert_eq!(r.covered(), 40);
    }

    #[test]
    fn an_empty_run_is_absent() {
        let r = classify(&[false; 40]);
        assert_eq!(r.kind, LineKind::Absent);
        assert!(r.segments.is_empty());
        assert_eq!(r.coverage(), 0.0);
        // And so is a run with nothing but speckle in it.
        let mut hits = [false; 40];
        hits[7] = true;
        hits[21] = true;
        let r = classify(&hits);
        assert_eq!(r.kind, LineKind::Absent, "single pixels are not a line");
    }

    /// The shader's own pattern: a dash of `2 x border width` and a gap of
    /// `1 x width`, which at the grid's 2px border is 4 on, 2 off.
    #[test]
    fn the_shaders_dash_pitch_reads_as_dashed() {
        let hits: Vec<bool> = (0..60).map(|i| i % 6 < 4).collect();
        let r = classify(&hits);
        assert_eq!(r.kind, LineKind::Dashed);
        assert_eq!(r.segments.len(), 10);
        assert!((r.dash_px() - 4.0).abs() < 0.01, "{}", r.dash_px());
        assert!((r.gap_px() - 2.0).abs() < 0.01, "{}", r.gap_px());
        assert!((r.coverage() - 2.0 / 3.0).abs() < 0.01);
    }

    /// One pixel of anti-aliased drop-out in the middle of a solid line must
    /// not turn it into two dashes.
    #[test]
    fn a_one_pixel_dropout_does_not_make_a_solid_line_dashed() {
        let mut hits = vec![true; 40];
        hits[19] = false;
        let r = classify(&hits);
        assert_eq!(r.kind, LineKind::Solid);
        assert_eq!(r.segments.len(), 1, "the gap is closed");
    }

    /// A real hole is not closed, and does not get rounded to either answer.
    #[test]
    fn a_line_with_a_real_hole_reads_as_broken() {
        let hits: Vec<bool> = (0..40).map(|i| !(15..25).contains(&i)).collect();
        let r = classify(&hits);
        assert_eq!(r.kind, LineKind::Broken);
        assert_eq!(r.segments.len(), 2);
        // Half an edge is broken too, not "solid enough".
        let hits: Vec<bool> = (0..40).map(|i| i < 20).collect();
        assert_eq!(classify(&hits).kind, LineKind::Broken);
    }

    /// Three pieces of wildly different lengths cover as much of the run as a
    /// dash pattern would, and are not one. A rule that only counted pieces
    /// would call this dashed and pass a test that ought to fail.
    #[test]
    fn irregular_pieces_are_broken_however_many_there_are() {
        let hits: Vec<bool> = (0..60)
            .map(|i| (0..3).contains(&i) || (10..30).contains(&i) || (50..57).contains(&i))
            .collect();
        let r = classify(&hits);
        assert_eq!(r.segments.len(), 3);
        assert_eq!(r.kind, LineKind::Broken, "{}", r.describe());
    }

    /// The grid draws a pointed range's border cell by cell, so the dash phase
    /// restarts at every cell boundary: a truncated dash before the join and
    /// two abutting ones after it. This is a transcription of what the real
    /// thing measures — 20 runs of 2 to 7px on even 3px gaps, 52% of 126px —
    /// and it must read as dashed. It did not before [`TRIM_FRACTION`].
    #[test]
    fn a_border_drawn_cell_by_cell_reads_as_dashed_despite_its_joins() {
        // Three dashes per stretch, then a join: a 2px stub and a 7px pair.
        let mut hits = Vec::new();
        for cell in 0..2 {
            for _ in 0..8 {
                hits.extend([true, true, true, false, false, false]);
            }
            if cell == 0 {
                hits.extend([true, true, false, false, false]); // the truncated dash
                hits.extend([true; 7]); // the two that abut across the join
                hits.extend([false, false, false]);
            }
        }
        let r = classify(&hits);
        assert_eq!(r.kind, LineKind::Dashed, "{}", r.describe());
        let (dlo, dhi, glo, ghi) = r.spread();
        assert!(
            dhi >= 7 && dlo <= 2,
            "the joins are in there: {dlo}-{dhi}px"
        );
        assert_eq!((glo, ghi), (3, 3), "the shader's own gaps are even");
    }

    /// A line with a handful of holes in it covers far more of its edge than
    /// gpui's `2W`/`1W` shader ever can, and must not be rounded to "dashed"
    /// just because it is in several pieces on even gaps. Found by running the
    /// harness: the diagnostic probe of a real dashed border against the
    /// BACKGROUND picks up the cell text too and reads 87% in 8 runs.
    #[test]
    fn a_line_with_holes_covers_too_much_of_its_edge_to_be_dashed() {
        // Eight runs of 13px on 2px gaps: 87% of the run, evenly spaced.
        let mut hits = Vec::new();
        for _ in 0..8 {
            hits.extend(std::iter::repeat_n(true, 13));
            hits.extend([false, false]);
        }
        let r = classify(&hits);
        assert!(r.coverage() > DASH_MAX, "{}", r.describe());
        assert!(r.coverage() < SOLID_MIN, "{}", r.describe());
        assert_eq!(r.kind, LineKind::Broken, "{}", r.describe());
        // And the report says what it was rejected on, not just "broken".
        assert!(r.describe().contains("runs of 13-13px"), "{}", r.describe());
        assert!(r.describe().contains("gaps of 2-2px"), "{}", r.describe());
    }

    #[test]
    fn a_run_of_no_pixels_is_absent_rather_than_a_division_by_zero() {
        let r = classify(&[]);
        assert_eq!(r.kind, LineKind::Absent);
        assert_eq!(r.coverage(), 0.0);
        assert_eq!(r.dash_px(), 0.0);
        assert_eq!(r.gap_px(), 0.0);
    }

    // ---- colour ----------------------------------------------------------

    #[test]
    fn colours_match_within_the_tolerance_and_not_beyond_it() {
        assert_eq!(color_dist(TEAL, TEAL), 0);
        assert!(color_matches(TEAL, [0x2a, 0xa7, 0x9b, 0xff], 0));
        // A few units of compositor rounding.
        assert!(color_matches(TEAL, [0x2c, 0xa5, 0x9d, 0xff], DEFAULT_TOL));
        // Another palette colour is nowhere near.
        let blue: Rgba = [0x44, 0x72, 0xc4, 0xff];
        assert!(
            !color_matches(TEAL, blue, DEFAULT_TOL),
            "the tolerance must not blur two colours the grid draws"
        );
        assert_eq!(hex(TEAL), "#2aa79b");
    }

    /// The two tolerances answer opposite questions, so the named-colour one
    /// must stay under half the closest gap in the palette while the
    /// background one stays wide. Pinned here because a future "unify these"
    /// would re-open exactly the hole this pair was split to close.
    #[test]
    fn the_named_colour_tolerance_separates_the_two_blues() {
        let chart_blue: Rgba = [0x44, 0x72, 0xc4, 0xff];
        let ref_blue: Rgba = [0x2f, 0x6f, 0xdb, 0xff];
        assert_eq!(color_dist(chart_blue, ref_blue), 23);
        assert!(
            !color_matches(chart_blue, ref_blue, DEFAULT_TOL),
            "a pixel cannot be allowed to match both blues"
        );
        // The property `2 * tol < dist` buys, which `tol < dist` does not: NO
        // pixel matches both. `tol = 23` would also pass the assertion above
        // and still let every pixel between the two blues answer to either
        // name — which is the failure mode, not the distance itself.
        for r in 0..=0xffu16 {
            let px: Rgba = [r as u8, 0x70, 0xd0, 0xff];
            assert!(
                !(color_matches(px, chart_blue, DEFAULT_TOL)
                    && color_matches(px, ref_blue, DEFAULT_TOL)),
                "{} answers to both blues",
                hex(px)
            );
        }

        // A compile-time check: the background question must stay the wide one.
        const _: () = assert!(BACKGROUND_TOL > DEFAULT_TOL);
    }

    #[test]
    fn alpha_is_not_compared() {
        assert!(color_matches(TEAL, [0x2a, 0xa7, 0x9b, 0x00], 0));
    }

    // ---- probe_edge over synthetic images --------------------------------

    #[test]
    fn a_solid_edge_reads_solid_in_its_own_colour() {
        let mut img = blank(60, 30);
        hline(&mut img, 0, 2, TEAL, 1, 0);
        let p = probe_edge(&img, Side::Top, Target::Color(TEAL), ProbeOpts::default()).unwrap();
        assert_eq!(p.reading.kind, LineKind::Solid);
        assert_eq!(p.color, Some(TEAL));
        assert_eq!(p.offset, 0, "the line nearer the edge wins the tie");
    }

    #[test]
    fn a_dashed_edge_reads_dashed_at_the_shaders_pitch() {
        let mut img = blank(60, 30);
        hline(&mut img, 0, 2, TEAL, 4, 2);
        let p = probe_edge(&img, Side::Top, Target::Color(TEAL), ProbeOpts::default()).unwrap();
        assert_eq!(p.reading.kind, LineKind::Dashed, "{}", p.describe());
        assert!((p.reading.dash_px() - 4.0).abs() <= 1.0, "{}", p.describe());
        assert_eq!(p.color, Some(TEAL));
    }

    #[test]
    fn an_empty_edge_reads_absent() {
        let img = blank(60, 30);
        let p = probe_edge(&img, Side::Top, Target::Color(TEAL), ProbeOpts::default()).unwrap();
        assert_eq!(p.reading.kind, LineKind::Absent);
        assert_eq!(p.color, None);
        // And against "any line at all", measured from the background.
        let bg = background_color(&img, DEFAULT_DEPTH);
        assert_eq!(bg, WHITE);
        let p = probe_edge(&img, Side::Top, Target::NotColor(bg), ProbeOpts::default()).unwrap();
        assert_eq!(p.reading.kind, LineKind::Absent);
    }

    /// The case the plan calls out: a border with anti-aliased boundaries must
    /// still read as its NOMINAL colour, not as a blend of it with the paper.
    #[test]
    fn an_anti_aliased_edge_still_reads_as_its_nominal_colour() {
        let mut img = blank(60, 30);
        // A half-covered row, the two solid rows, and another half-covered one
        // — what a 2px line lands as when it does not sit on a pixel boundary.
        hline(&mut img, 0, 1, over_white(TEAL, 0.5), 1, 0);
        hline(&mut img, 1, 2, TEAL, 1, 0);
        hline(&mut img, 3, 1, over_white(TEAL, 0.5), 1, 0);
        let p = probe_edge(&img, Side::Top, Target::Color(TEAL), ProbeOpts::default()).unwrap();
        assert_eq!(p.reading.kind, LineKind::Solid);
        assert_eq!(
            p.color,
            Some(TEAL),
            "the band scan must keep the core of the line, not its blend"
        );
        assert_eq!(p.offset, 1, "the first fully covered row");
        // The blends are more than a tolerance away, which is why the core had
        // to be found rather than assumed.
        assert!(!color_matches(over_white(TEAL, 0.5), TEAL, DEFAULT_TOL));
        // Read against the background instead and the blends count too, so the
        // line is still there — a nameless expectation is not fooled either.
        let p = probe_edge(
            &img,
            Side::Top,
            Target::NotColor(WHITE),
            ProbeOpts::default(),
        )
        .unwrap();
        assert_eq!(p.reading.kind, LineKind::Solid);
    }

    /// The reason for the band: the app insets its border by 1px, so the line
    /// inside the crop is not always at offset 0.
    #[test]
    fn a_border_inset_from_the_edge_is_still_found() {
        let mut img = blank(60, 30);
        hline(&mut img, 2, 2, TEAL, 1, 0);
        let p = probe_edge(&img, Side::Top, Target::Color(TEAL), ProbeOpts::default()).unwrap();
        assert_eq!(p.reading.kind, LineKind::Solid);
        assert_eq!(p.offset, 2);
        // Past the band it is not found, and that is reported as absent rather
        // than as an error — nothing is wrong with the picture.
        let p = probe_edge(
            &img,
            Side::Top,
            Target::Color(TEAL),
            ProbeOpts {
                depth: 1,
                ..ProbeOpts::default()
            },
        )
        .unwrap();
        assert_eq!(p.reading.kind, LineKind::Absent);
    }

    #[test]
    fn every_side_is_read_from_its_own_edge() {
        let mut img = blank(40, 40);
        // A box: solid top, dashed left, nothing on the other two.
        hline(&mut img, 0, 2, TEAL, 1, 0);
        for y in 0..40 {
            if y % 6 < 4 {
                for x in 0..2 {
                    img.set_pixel(x, y, TEAL);
                }
            }
        }
        let read = |s: Side| {
            probe_edge(&img, s, Target::Color(TEAL), ProbeOpts::default())
                .unwrap()
                .reading
                .kind
        };
        assert_eq!(read(Side::Top), LineKind::Solid);
        assert_eq!(read(Side::Left), LineKind::Dashed);
        assert_eq!(read(Side::Bottom), LineKind::Absent);
        assert_eq!(read(Side::Right), LineKind::Absent);
    }

    /// The corner pixels of the perpendicular borders must not make an absent
    /// edge look like it has something on it.
    #[test]
    fn the_corners_of_the_other_borders_do_not_count_as_this_edge() {
        let mut img = blank(40, 40);
        for y in 0..40 {
            for x in [0u32, 1, 38, 39] {
                img.set_pixel(x, y, TEAL);
            }
        }
        let p = probe_edge(&img, Side::Top, Target::Color(TEAL), ProbeOpts::default()).unwrap();
        assert_eq!(p.reading.kind, LineKind::Absent, "{}", p.describe());
    }

    #[test]
    fn a_region_too_small_to_read_is_an_error_not_an_absent_border() {
        let img = blank(3, 3);
        let e = probe_edge(&img, Side::Top, Target::Color(TEAL), ProbeOpts::default()).unwrap_err();
        assert!(e.contains("3x3"), "{e}");
        assert!(e.contains("top"), "{e}");
        let img = Image::new(0, 0);
        assert!(probe_edge(&img, Side::Left, Target::Color(TEAL), ProbeOpts::default()).is_err());
    }

    #[test]
    fn the_background_is_the_interior_not_the_border() {
        let mut img = blank(40, 40);
        // A thick teal frame — but the middle is still white.
        for y in 0..40 {
            for x in 0..40 {
                if x < 3 || y < 3 || x >= 37 || y >= 37 {
                    img.set_pixel(x, y, TEAL);
                }
            }
        }
        assert_eq!(background_color(&img, DEFAULT_DEPTH), WHITE);
        // A crop with no interior left still answers, rather than refusing.
        let tiny = img.crop(crate::image::RectPx::new(0, 0, 2, 2)).unwrap();
        assert_eq!(background_color(&tiny, DEFAULT_DEPTH), TEAL);
    }

    #[test]
    fn a_reading_describes_itself_in_the_words_a_failure_prints() {
        let hits: Vec<bool> = (0..60).map(|i| i % 6 < 4).collect();
        let d = classify(&hits).describe();
        assert!(d.starts_with("dashed"), "{d}");
        assert!(d.contains("10 dashes"), "{d}");
        assert!(classify(&[true; 20]).describe().starts_with("solid"));
        assert!(classify(&[false; 20]).describe().starts_with("absent"));
    }

    #[test]
    fn sides_round_trip_through_their_names() {
        for s in Side::ALL {
            assert_eq!(Side::parse(s.name()), Some(s));
            assert_eq!(Side::parse(&s.name().to_uppercase()), Some(s));
        }
        assert_eq!(Side::parse("all"), None, "'all' is not one side");
        assert_eq!(Side::parse("middle"), None);
    }
}
