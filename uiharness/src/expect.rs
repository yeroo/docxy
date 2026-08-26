//! The vocabulary a test is written in, and what a failure in it reads like.
//!
//! A UI test says what it expects in its own words:
//!
//! ```text
//! border A1:C5 solid
//! border A1:D5 dashed teal
//! border A1 top absent
//! no border cell:B7
//! ```
//!
//! [`parse_border`] turns one of those into a [`BorderExpect`] — a pure
//! function over a string, so every accepted and rejected form is a unit test
//! rather than something you find out by running a window. The region names are
//! Task 4's: whatever the app's `rect` verb accepts, plus the shorthand of
//! writing `A1:C5` for `cell:A1:C5`, because a test that is about a selection
//! should read like the selection.
//!
//! [`check_border`] then reads the region's picture with the probes in
//! [`crate::probe`] and produces a [`BorderCheck`], whose `report` is the whole
//! point of this module:
//!
//! ```text
//! border cell:A1:C5 solid — FAILED
//!   expected: every edge solid
//!   observed: top     dashed (18 dashes of ~4.0px, gaps ~2.0px, 67% of 248px) #2aa79b
//!             right   dashed (…) #2aa79b
//!             bottom  dashed (…) #2aa79b
//!             left    dashed (…) #2aa79b
//!   evidence: …\runs\20260826\drag-select-does-not-fill\cell-a1-c5.png
//! ```
//!
//! A failure that only said "assertion failed" would send the reader back to
//! installing a build and looking at it, which is the thing this harness exists
//! to stop.
//!
//! ## Name the colour when you mean "no selection here"
//!
//! Found by running it against the real grid, not by reading it. An
//! expectation with no colour asks "is there **any** line here", measured
//! against the region's own background — and an ordinary cell has the sheet's
//! gridlines along two of its sides, so `no border H20` fails on a perfectly
//! ordinary cell with `solid (100% of 61px) #d9d9d9`. That is the truth about
//! the picture, and the report says which colour it found, but it is not what
//! the test meant. `no border H20 teal` is: no *selection* border here, the
//! gridlines being none of its business.
//!
//! The nameless form still earns its keep for a region that should be blank —
//! a fill preview that must not have been drawn, say.

use crate::image::Image;
use crate::probe::{
    DEFAULT_DEPTH, LineKind, LineProbe, ProbeOpts, Rgba, Side, Target, background_color, hex,
    probe_edge,
};
use std::path::Path;

/// The colours the grid draws its outlines in, by the name a test would use.
///
/// These mirror constants in `suite/docxy/src/main.rs` — `BRAND` (:1283) and
/// the chart slot colours (:2898) — and are duplicated rather than shared
/// because the two crates are in different workspaces. A test may always write
/// `#rrggbb` instead, which is what to do for a colour that is not on this
/// list; the list exists so the common cases read as English.
pub const NAMED_COLORS: &[(&str, u32)] = &[
    // The brand teal: the selection ring, and the pointed range's dashes.
    ("teal", 0x2AA79B),
    ("brand", 0x2AA79B),
    // A selected chart's source areas: Excel's own mapping.
    ("blue", 0x4472C4),
    ("purple", 0x7030A0),
    ("green", 0x00B050),
    ("white", 0xFFFFFF),
    ("black", 0x000000),
];

/// A colour by name or as `#rrggbb` / `rrggbb`.
pub fn parse_color(text: &str) -> Result<Rgba, String> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    if let Some((_, v)) = NAMED_COLORS.iter().find(|(n, _)| *n == lower) {
        return Ok(rgb(*v));
    }
    let hexpart = lower.strip_prefix('#').unwrap_or(&lower);
    if hexpart.len() == 6 && hexpart.chars().all(|c| c.is_ascii_hexdigit()) {
        let v = u32::from_str_radix(hexpart, 16).map_err(|e| e.to_string())?;
        return Ok(rgb(v));
    }
    let names: Vec<&str> = NAMED_COLORS.iter().map(|(n, _)| *n).collect();
    Err(format!(
        "'{t}' is not a colour: write #rrggbb, or one of {}",
        names.join(", ")
    ))
}

/// A packed `0xRRGGBB` as an opaque pixel.
fn rgb(v: u32) -> Rgba {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8, 0xff]
}

/// What a test can expect of an edge. [`LineKind::Broken`] is missing on
/// purpose: it is a reading, never an expectation — nobody asks for a border
/// with a hole in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpectKind {
    Solid,
    Dashed,
    Absent,
}

impl ExpectKind {
    pub fn name(self) -> &'static str {
        match self {
            ExpectKind::Solid => "solid",
            ExpectKind::Dashed => "dashed",
            ExpectKind::Absent => "absent",
        }
    }

    fn parse(s: &str) -> Option<ExpectKind> {
        match s.to_ascii_lowercase().as_str() {
            "solid" => Some(ExpectKind::Solid),
            "dashed" | "dash" => Some(ExpectKind::Dashed),
            "absent" | "none" | "no" => Some(ExpectKind::Absent),
            _ => None,
        }
    }

    /// Whether a reading satisfies this expectation.
    fn satisfied_by(self, got: LineKind) -> bool {
        match self {
            ExpectKind::Solid => got == LineKind::Solid,
            ExpectKind::Dashed => got == LineKind::Dashed,
            ExpectKind::Absent => got == LineKind::Absent,
        }
    }
}

impl std::fmt::Display for ExpectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// One expectation about a region's border.
#[derive(Debug, Clone, PartialEq)]
pub struct BorderExpect {
    /// The region, in the form the app's `rect` verb takes.
    pub region: String,
    /// The sides to read — all four unless the test named one.
    pub sides: Vec<Side>,
    /// Whether the test named its sides, which is only used to phrase the
    /// message ("every edge" versus "the top edge").
    pub sides_named: bool,
    pub kind: ExpectKind,
    /// The colour the test named, if any.
    pub color: Option<Rgba>,
    /// That colour as the test wrote it, so the message quotes the test.
    pub color_label: Option<String>,
    /// The expectation as written.
    pub source: String,
}

impl BorderExpect {
    /// The expectation in words, for the `expected:` line of a report.
    pub fn describe(&self) -> String {
        let which = if self.sides_named {
            let names: Vec<&str> = self.sides.iter().map(|s| s.name()).collect();
            format!("the {} edge", names.join(" and "))
        } else {
            "every edge".to_string()
        };
        let colour = match (&self.color, &self.color_label) {
            (Some(c), Some(l)) if !l.starts_with('#') => Some(format!("{l} ({})", hex(*c))),
            (Some(c), _) => Some(hex(*c)),
            _ => None,
        };
        match (self.kind, colour) {
            // "absent teal" is a real expectation and a useful one — "no teal
            // border here", which leaves any other line on the edge alone.
            (ExpectKind::Absent, Some(c)) => format!("no {c} line along {which}"),
            (ExpectKind::Absent, None) => format!("nothing along {which}"),
            (k, Some(c)) => format!("{which} {k} in {c}"),
            (k, None) => format!("{which} {k}"),
        }
    }
}

/// The region names that are not cell references, for an error message.
const REGION_WORDS: &str = "window, grid, chart-panel, cell:B3, cell:A1:C5, chart:0";

/// `A1` and `A1:C5` are written bare in a test; anything with a `:` head the
/// app knows, or one of its bare names, is passed through untouched.
///
/// Getting this wrong in the permissive direction would be quiet and bad — a
/// typo'd region would go to the app and come back as an error, which is fine;
/// a bare word wrongly rewritten to `cell:` would ask about the wrong thing.
/// So only a real A1-style reference is rewritten.
pub fn normalize_region(tok: &str) -> String {
    let t = tok.trim();
    if is_a1(t) || t.split_once(':').is_some_and(|(a, b)| is_a1(a) && is_a1(b)) {
        return format!("cell:{t}");
    }
    t.to_string()
}

/// Whether `s` is an A1-style cell reference: letters then digits, no `$`.
fn is_a1(s: &str) -> bool {
    let letters = s.chars().take_while(|c| c.is_ascii_alphabetic()).count();
    let digits = s[letters..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .count();
    letters > 0 && digits > 0 && letters + digits == s.len()
}

/// Parse one border expectation.
///
/// ```text
/// border <region> [<side>] <solid|dashed|absent> [<colour>]
/// no border <region>
/// ```
///
/// The words may come in either order after the region (`border A1 top dashed`
/// and `border A1 dashed top` both parse), because that is the kind of thing a
/// test author gets wrong once and never thinks about again.
pub fn parse_border(text: &str) -> Result<BorderExpect, String> {
    let source = text.trim().to_string();
    let mut words: Vec<&str> = source.split_whitespace().collect();
    // `no border X` is `border X absent`, spelled the way it reads.
    let negated = words.first().is_some_and(|w| w.eq_ignore_ascii_case("no"));
    if negated {
        words.remove(0);
    }
    match words.first() {
        Some(w) if w.eq_ignore_ascii_case("border") => {
            words.remove(0);
        }
        Some(w) => {
            return Err(format!(
                "'{w}' is not an expectation; write 'border <region> <solid|dashed|absent> [colour]'"
            ));
        }
        None => {
            return Err(
                "an empty expectation; write 'border <region> <solid|dashed|absent> [colour]'"
                    .to_string(),
            );
        }
    }
    let region_tok = words
        .first()
        .copied()
        .ok_or_else(|| format!("'border' needs a region ({REGION_WORDS})"))?;
    words.remove(0);

    let mut sides: Vec<Side> = Vec::new();
    let mut sides_named = false;
    let mut kind: Option<ExpectKind> = None;
    let mut color: Option<Rgba> = None;
    let mut color_label: Option<String> = None;
    for w in words {
        if w.eq_ignore_ascii_case("all") {
            sides_named = false;
            sides.clear();
            continue;
        }
        if let Some(s) = Side::parse(w) {
            sides_named = true;
            if !sides.contains(&s) {
                sides.push(s);
            }
            continue;
        }
        if let Some(k) = ExpectKind::parse(w) {
            if let Some(prev) = kind {
                if prev != k {
                    return Err(format!("'{source}' asks for both {prev} and {k}; pick one"));
                }
            }
            kind = Some(k);
            continue;
        }
        match parse_color(w) {
            Ok(c) if color.is_none() => {
                color = Some(c);
                color_label = Some(w.to_string());
            }
            Ok(_) => {
                return Err(format!(
                    "'{source}' names two colours; an edge is drawn in one"
                ));
            }
            Err(e) => {
                return Err(format!(
                    "'{w}' is not a side, a line style or a colour in '{source}' \
                     (sides: top, right, bottom, left, all; styles: solid, dashed, absent; {e})"
                ));
            }
        }
    }
    let kind = match (kind, negated) {
        (Some(ExpectKind::Absent), _) | (None, true) => ExpectKind::Absent,
        (Some(k), false) => k,
        (Some(k), true) => {
            return Err(format!(
                "'{source}' says both 'no border' and '{k}'; pick one"
            ));
        }
        (None, false) => {
            return Err(format!(
                "'{source}' does not say what the border should be \
                 (solid, dashed or absent)"
            ));
        }
    };
    if sides.is_empty() {
        sides = Side::ALL.to_vec();
    }
    Ok(BorderExpect {
        region: normalize_region(region_tok),
        sides,
        sides_named,
        kind,
        color,
        color_label,
        source,
    })
}

/// One side, read.
#[derive(Debug, Clone, PartialEq)]
pub struct SideOutcome {
    pub probe: LineProbe,
    pub pass: bool,
    /// What is actually drawn there, read against the background rather than
    /// against the colour the test named. Filled in only when the side failed
    /// and a colour was named, so a "no teal line" that failed can say what
    /// colour the line it found really is.
    pub any_line: Option<LineProbe>,
}

/// An expectation, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct BorderCheck {
    pub expect: BorderExpect,
    pub sides: Vec<SideOutcome>,
    /// The region's background colour, which is what a nameless expectation was
    /// measured against.
    pub background: Rgba,
    /// A side that could not be read at all (too small a crop, say). A check
    /// with one of these has failed, whatever the readings say.
    pub errors: Vec<String>,
}

impl BorderCheck {
    pub fn passed(&self) -> bool {
        self.errors.is_empty() && self.sides.iter().all(|s| s.pass)
    }

    /// The whole verdict, ready to print. `png` is where the evidence was
    /// filed; a check run without saving one passes `None`.
    pub fn report(&self, png: Option<&Path>) -> String {
        let mut out = format!(
            "{} — {}\n",
            self.expect.source,
            if self.passed() { "ok" } else { "FAILED" }
        );
        out.push_str(&format!("  region:   {}\n", self.expect.region));
        out.push_str(&format!("  expected: {}\n", self.expect.describe()));
        let mut first = true;
        for s in &self.sides {
            let head = if first {
                "  observed: "
            } else {
                "            "
            };
            first = false;
            out.push_str(&format!(
                "{head}{:<7}{}{}\n",
                s.probe.side.name(),
                s.probe.describe(),
                if s.pass { "" } else { "   <- not as expected" }
            ));
            if let Some(any) = &s.any_line {
                out.push_str(&format!(
                    "            {:<7}what is drawn there: {}\n",
                    "",
                    any.describe()
                ));
            }
        }
        for e in &self.errors {
            let head = if first {
                "  observed: "
            } else {
                "            "
            };
            first = false;
            out.push_str(&format!("{head}{e}\n"));
        }
        out.push_str(&format!(
            "  paper:    {} (what 'any line' was measured against)\n",
            hex(self.background)
        ));
        if let Some(p) = png {
            out.push_str(&format!("  evidence: {}\n", p.display()));
        }
        out
    }
}

/// Check an expectation against the region's pixels.
///
/// Pure: the picture has already been taken and cropped. `img` must be the crop
/// of the region the expectation names — [`crate::Driver::shot`] produces
/// exactly that, and pairing the two is the caller's job (see
/// `main.rs`'s `assert`).
pub fn check_border(exp: &BorderExpect, img: &Image, opts: ProbeOpts) -> BorderCheck {
    let background = background_color(img, DEFAULT_DEPTH);
    let target = match exp.color {
        Some(c) => Target::Color(c),
        None => Target::NotColor(background),
    };
    let mut sides = Vec::new();
    let mut errors = Vec::new();
    for side in &exp.sides {
        match probe_edge(img, *side, target, opts) {
            Ok(probe) => {
                let pass = exp.kind.satisfied_by(probe.reading.kind);
                // Only worth a second look when the first one disappointed and
                // the answer might be "a line, but not that colour".
                let any_line = (!pass && exp.color.is_some())
                    .then(|| probe_edge(img, *side, Target::NotColor(background), opts).ok())
                    .flatten();
                sides.push(SideOutcome {
                    probe,
                    pass,
                    any_line,
                });
            }
            Err(e) => errors.push(e),
        }
    }
    BorderCheck {
        expect: exp.clone(),
        sides,
        background,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEAL: Rgba = [0x2a, 0xa7, 0x9b, 0xff];
    const BLUE: Rgba = [0x44, 0x72, 0xc4, 0xff];
    const WHITE: Rgba = [0xff, 0xff, 0xff, 0xff];

    fn blank(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.set_pixel(x, y, WHITE);
            }
        }
        img
    }

    /// A `w` x `h` region with a 2px border on every side, `on`/`off` px along
    /// it — `off == 0` for solid.
    fn boxed(w: u32, h: u32, c: Rgba, on: u32, off: u32) -> Image {
        let mut img = blank(w, h);
        let lit = |i: u32| off == 0 || i % (on + off) < on;
        for x in 0..w {
            if lit(x) {
                for d in 0..2 {
                    img.set_pixel(x, d, c);
                    img.set_pixel(x, h - 1 - d, c);
                }
            }
        }
        for y in 0..h {
            if lit(y) {
                for d in 0..2 {
                    img.set_pixel(d, y, c);
                    img.set_pixel(w - 1 - d, y, c);
                }
            }
        }
        img
    }

    // ---- the parser ------------------------------------------------------

    #[test]
    fn the_plans_own_two_expectations_parse() {
        let e = parse_border("border A1:C5 solid").unwrap();
        assert_eq!(e.region, "cell:A1:C5");
        assert_eq!(e.kind, ExpectKind::Solid);
        assert_eq!(e.sides, Side::ALL.to_vec());
        assert_eq!(e.color, None);

        let e = parse_border("border A1:D5 dashed teal").unwrap();
        assert_eq!(e.region, "cell:A1:D5");
        assert_eq!(e.kind, ExpectKind::Dashed);
        assert_eq!(e.color, Some(TEAL));
        assert_eq!(e.color_label.as_deref(), Some("teal"));
    }

    #[test]
    fn a_region_is_written_however_the_app_or_the_test_prefers() {
        for (written, resolved) in [
            ("A1", "cell:A1"),
            ("A1:C5", "cell:A1:C5"),
            ("cell:A1:C5", "cell:A1:C5"),
            ("cells:B3", "cells:B3"),
            ("grid", "grid"),
            ("window", "window"),
            ("chart-panel", "chart-panel"),
            ("chart:0", "chart:0"),
        ] {
            let e = parse_border(&format!("border {written} solid")).unwrap();
            assert_eq!(e.region, resolved, "{written}");
        }
    }

    /// The rewrite must not reach a word that only looks like a reference.
    #[test]
    fn a_bare_word_that_is_not_a_reference_is_left_for_the_app_to_refuse() {
        assert_eq!(normalize_region("grid"), "grid");
        assert_eq!(normalize_region("A"), "A");
        assert_eq!(normalize_region("1"), "1");
        assert_eq!(normalize_region("A1B"), "A1B");
        assert_eq!(normalize_region("$A$1"), "$A$1");
        assert_eq!(normalize_region("chart:0"), "chart:0");
        assert_eq!(normalize_region("A1:C5"), "cell:A1:C5");
        assert_eq!(normalize_region("aa10:bb20"), "cell:aa10:bb20");
    }

    #[test]
    fn a_side_may_be_named_and_the_words_may_come_in_either_order() {
        let a = parse_border("border A1 top dashed").unwrap();
        let b = parse_border("border A1 dashed top").unwrap();
        assert_eq!(a.sides, vec![Side::Top]);
        assert_eq!(a.sides, b.sides);
        assert_eq!(a.kind, b.kind);
        assert!(a.sides_named);
        let e = parse_border("border A1 left right solid").unwrap();
        assert_eq!(e.sides, vec![Side::Left, Side::Right]);
        // 'all' is the default, spelled out.
        let e = parse_border("border A1 all solid").unwrap();
        assert_eq!(e.sides, Side::ALL.to_vec());
        assert!(!e.sides_named);
    }

    #[test]
    fn absence_is_spelled_three_ways_and_they_agree() {
        for text in ["border B7 absent", "border B7 none", "no border B7"] {
            let e = parse_border(text).unwrap();
            assert_eq!(e.kind, ExpectKind::Absent, "{text}");
            assert_eq!(e.region, "cell:B7");
        }
        // With a colour, which is the useful form: no TEAL line here.
        let e = parse_border("no border B7 teal").unwrap();
        assert_eq!(e.kind, ExpectKind::Absent);
        assert_eq!(e.color, Some(TEAL));
    }

    #[test]
    fn a_colour_is_a_name_or_a_hex_triple() {
        assert_eq!(parse_color("teal").unwrap(), TEAL);
        assert_eq!(parse_color("TEAL").unwrap(), TEAL);
        assert_eq!(parse_color("brand").unwrap(), TEAL);
        assert_eq!(parse_color("#2aa79b").unwrap(), TEAL);
        assert_eq!(parse_color("2AA79B").unwrap(), TEAL);
        assert_eq!(parse_color("blue").unwrap(), BLUE);
        assert_eq!(parse_color("white").unwrap(), WHITE);
        let e = parse_color("turquoise").unwrap_err();
        assert!(e.contains("turquoise"), "{e}");
        assert!(e.contains("#rrggbb"), "{e}");
        assert!(e.contains("teal"), "{e}");
        assert!(parse_color("#2aa79").is_err(), "five digits");
        assert!(parse_color("#gggggg").is_err());
    }

    /// The named colours must be the ones the grid actually draws, or a test
    /// that passes is testing the wrong thing.
    #[test]
    fn the_named_colours_are_the_grids_own() {
        let by = |n: &str| NAMED_COLORS.iter().find(|(k, _)| *k == n).unwrap().1;
        assert_eq!(by("teal"), 0x2AA79B, "main.rs BRAND");
        assert_eq!(by("brand"), by("teal"));
        assert_eq!(by("blue"), 0x4472C4, "main.rs CHART_VALUES_COLOR");
        assert_eq!(by("purple"), 0x7030A0, "main.rs CHART_CATEGORIES_COLOR");
        assert_eq!(by("green"), 0x00B050, "main.rs CHART_NAME_COLOR");
    }

    #[test]
    fn an_expectation_that_says_nothing_useful_is_refused_by_name() {
        let e = parse_border("").unwrap_err();
        assert!(e.contains("empty"), "{e}");
        let e = parse_border("assert A1 solid").unwrap_err();
        assert!(e.contains("assert"), "{e}");
        let e = parse_border("border").unwrap_err();
        assert!(e.contains("needs a region"), "{e}");
        let e = parse_border("border A1").unwrap_err();
        assert!(e.contains("does not say what"), "{e}");
        let e = parse_border("border A1 wobbly").unwrap_err();
        assert!(e.contains("wobbly"), "{e}");
        assert!(e.contains("solid"), "{e}");
        let e = parse_border("border A1 solid dashed").unwrap_err();
        assert!(e.contains("pick one"), "{e}");
        let e = parse_border("border A1 solid teal blue").unwrap_err();
        assert!(e.contains("two colours"), "{e}");
        let e = parse_border("no border A1 solid").unwrap_err();
        assert!(e.contains("pick one"), "{e}");
    }

    // ---- checking against pixels ----------------------------------------

    #[test]
    fn a_solid_teal_box_satisfies_the_solid_expectation() {
        let img = boxed(60, 40, TEAL, 1, 0);
        let exp = parse_border("border A1:C5 solid teal").unwrap();
        let c = check_border(&exp, &img, ProbeOpts::default());
        assert!(c.passed(), "{}", c.report(None));
        assert_eq!(c.sides.len(), 4);
        assert_eq!(c.background, WHITE);
        assert!(c.sides.iter().all(|s| s.probe.color == Some(TEAL)));
    }

    #[test]
    fn a_dashed_box_satisfies_dashed_and_fails_solid() {
        let img = boxed(60, 40, TEAL, 4, 2);
        let ok = check_border(
            &parse_border("border A1:C5 dashed teal").unwrap(),
            &img,
            ProbeOpts::default(),
        );
        assert!(ok.passed(), "{}", ok.report(None));
        let bad = check_border(
            &parse_border("border A1:C5 solid").unwrap(),
            &img,
            ProbeOpts::default(),
        );
        assert!(!bad.passed());
        // This is the regression in the plan's Overview, stated in pixels: the
        // report has to say which way round it went.
        let r = bad.report(None);
        assert!(r.contains("FAILED"), "{r}");
        assert!(r.contains("dashed"), "{r}");
        assert!(r.contains("not as expected"), "{r}");
    }

    #[test]
    fn an_empty_region_satisfies_absent_and_fails_the_others() {
        let img = blank(60, 40);
        let c = check_border(
            &parse_border("no border A1").unwrap(),
            &img,
            ProbeOpts::default(),
        );
        assert!(c.passed(), "{}", c.report(None));
        let c = check_border(
            &parse_border("border A1 solid").unwrap(),
            &img,
            ProbeOpts::default(),
        );
        assert!(!c.passed());
        assert!(c.report(None).contains("absent"));
    }

    /// A border in the wrong colour must fail a coloured expectation, and the
    /// report must say what colour is actually there — otherwise the reader
    /// learns only that something is wrong.
    #[test]
    fn a_border_of_the_wrong_colour_fails_and_the_report_names_the_real_one() {
        let img = boxed(60, 40, BLUE, 1, 0);
        let c = check_border(
            &parse_border("border A1 solid teal").unwrap(),
            &img,
            ProbeOpts::default(),
        );
        assert!(!c.passed());
        let r = c.report(None);
        assert!(r.contains("#4472c4"), "{r}");
        assert!(r.contains("what is drawn there"), "{r}");
        assert!(r.contains("#2aa79b"), "the expectation is quoted too: {r}");
    }

    #[test]
    fn only_the_named_side_is_read_when_the_test_names_one() {
        let mut img = blank(60, 40);
        // Solid teal along the top only.
        for x in 0..60 {
            for y in 0..2 {
                img.set_pixel(x, y, TEAL);
            }
        }
        let ok = check_border(
            &parse_border("border A1 top solid teal").unwrap(),
            &img,
            ProbeOpts::default(),
        );
        assert!(ok.passed(), "{}", ok.report(None));
        assert_eq!(ok.sides.len(), 1);
        let bad = check_border(
            &parse_border("border A1 solid teal").unwrap(),
            &img,
            ProbeOpts::default(),
        );
        assert!(!bad.passed(), "the other three sides are bare");
        assert_eq!(bad.sides.iter().filter(|s| s.pass).count(), 1);
    }

    /// An unreadable crop is an error, not a quiet "no border" — the difference
    /// between "the app drew nothing" and "the harness photographed nothing".
    #[test]
    fn a_crop_too_small_to_read_is_reported_as_an_error() {
        let img = blank(3, 3);
        let c = check_border(
            &parse_border("no border A1").unwrap(),
            &img,
            ProbeOpts::default(),
        );
        assert!(!c.passed(), "an unreadable region cannot pass");
        assert_eq!(c.errors.len(), 4);
        assert!(c.report(None).contains("too small"));
    }

    #[test]
    fn the_report_names_the_evidence_it_was_read_from() {
        let img = boxed(60, 40, TEAL, 4, 2);
        let c = check_border(
            &parse_border("border A1:C5 solid").unwrap(),
            &img,
            ProbeOpts::default(),
        );
        let png = Path::new("runs").join("drag-select").join("cell-a1-c5.png");
        let r = c.report(Some(&png));
        assert!(r.contains("evidence:"), "{r}");
        assert!(r.contains("cell-a1-c5.png"), "{r}");
        // Everything a reader needs without opening the file: what was asked,
        // what was seen, and where to look.
        assert!(r.contains("expected:"), "{r}");
        assert!(r.contains("observed:"), "{r}");
        assert!(r.contains("region:   cell:A1:C5"), "{r}");
    }

    #[test]
    fn an_expectation_describes_itself_in_the_words_it_was_written_in() {
        let d = parse_border("border A1:D5 dashed teal").unwrap().describe();
        assert_eq!(d, "every edge dashed in teal (#2aa79b)");
        let d = parse_border("border A1 top solid").unwrap().describe();
        assert_eq!(d, "the top edge solid");
        let d = parse_border("border A1 left right solid")
            .unwrap()
            .describe();
        assert_eq!(d, "the left and right edge solid");
        let d = parse_border("no border B7").unwrap().describe();
        assert_eq!(d, "nothing along every edge");
        let d = parse_border("no border B7 teal").unwrap().describe();
        assert_eq!(d, "no teal (#2aa79b) line along every edge");
        let d = parse_border("border A1 solid #123456").unwrap().describe();
        assert_eq!(d, "every edge solid in #123456");
    }
}
