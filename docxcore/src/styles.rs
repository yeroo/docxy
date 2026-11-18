//! Parse `word/styles.xml` and resolve **effective** run/paragraph properties.
//!
//! WordprocessingML formatting is layered: document defaults → paragraph style
//! (following `basedOn`) → character style → direct run properties. The model
//! stores only *direct* properties (so save stays faithful); this module
//! resolves what a run/paragraph should actually look like for rendering/export.
//!
//! Because the model's `RunProps` can't represent "unset" toggles, inheritance
//! uses an OR / or-else merge: a style turns a property *on*; a direct property
//! wins when present. Explicit "turn off in a child" is uncommon and not modeled.

use std::collections::{HashMap, HashSet};

use crate::model::{
    Align, BorderKind, ParBorder, ParBorders, ParProps, RunProps, TabAlign, TabLeader, TabStop,
};
use crate::table_styles::RunLook;
use crate::theme::{ThemeColor, ThemeColorRef, ThemeFonts};
use crate::xml::{Event, XmlParser};

#[derive(Debug, Clone, Default)]
struct PartialRun {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
    rtl: Option<bool>,
    /// `w:vanish`: hidden text. Resolved only by
    /// [`StyleSheet::effective_vanish`]; rendering keeps direct `vanish`.
    vanish: Option<bool>,
    color: Option<String>,
    size: Option<u32>,
    font: Option<String>,
    /// `w:rFonts w:asciiTheme` (else `w:hAnsiTheme`): a theme font such as
    /// `minorHAnsi`, which [`StyleSheet::apply_theme_fonts`] turns into
    /// `font` (#1972).
    font_theme: Option<String>,
}

impl PartialRun {
    fn merge(&mut self, o: &PartialRun) {
        if o.bold.is_some() {
            self.bold = o.bold;
        }
        if o.italic.is_some() {
            self.italic = o.italic;
        }
        if o.underline.is_some() {
            self.underline = o.underline;
        }
        if o.strike.is_some() {
            self.strike = o.strike;
        }
        if o.rtl.is_some() {
            self.rtl = o.rtl;
        }
        if o.vanish.is_some() {
            self.vanish = o.vanish;
        }
        if o.color.is_some() {
            self.color = o.color.clone();
        }
        if o.size.is_some() {
            self.size = o.size;
        }
        if o.font.is_some() {
            self.font = o.font.clone();
        }
        if o.font_theme.is_some() {
            self.font_theme = o.font_theme.clone();
        }
    }

    /// Its theme font reference resolved against `theme`: a reference the
    /// theme names beats the explicit `w:ascii`, as in Word; any other
    /// leaves `font` as it is.
    fn apply_theme(&mut self, theme: &ThemeFonts) {
        if let Some(family) = self.font_theme.as_deref().and_then(|t| theme.family(t)) {
            self.font = Some(family.to_string());
        }
    }
}

#[derive(Debug, Clone, Default)]
struct StyleDef {
    based_on: Option<String>,
    run: PartialRun,
    align: Option<Align>,
    rtl: Option<bool>,
    tabs: Vec<TabStop>,
    borders: ParBorders,
    /// `w:pageBreakBefore` / `w:suppressLineNumbers` in the style's `w:pPr`.
    page_break_before: Option<bool>,
    suppress_line_numbers: Option<bool>,
    /// `w:numPr` in the style's `w:pPr`: its `w:numId` (when given) and
    /// `w:ilvl` (when given).
    num_pr: Option<(Option<i32>, Option<i32>)>,
    /// The left indent attributes of the style's `w:ind`.
    left: LeftInd,
    /// Display name (`w:name`), e.g. "heading 1".
    name: Option<String>,
    /// Whether this is a paragraph style (`w:type="paragraph"`), so the
    /// Apply-Styles picker can list only paragraph styles.
    is_paragraph: bool,
}

/// A parsed `styles.xml`: document defaults plus named style definitions.
#[derive(Debug, Clone, Default)]
pub struct StyleSheet {
    default_run: PartialRun,
    styles: HashMap<String, StyleDef>,
    /// The default paragraph style (`w:type="paragraph" w:default="1"`), which
    /// applies to a paragraph without a `w:pStyle`.
    default_paragraph: Option<String>,
    /// The left indent of `w:docDefaults/w:pPrDefault/w:pPr/w:ind`, the last
    /// fallback of [`StyleSheet::style_left_indent`].
    default_left: LeftInd,
    /// [`StyleSheet::doc_font`], kept as the sheet changes.
    doc_font: Option<String>,
    /// [`StyleSheet::para_font`] of each style id, kept as the sheet changes.
    para_fonts: HashMap<String, Option<String>>,
    /// The document defaults' own family: [`StyleSheet::para_font`] of a
    /// style id the sheet does not define.
    defaults_font: Option<String>,
}

/// The left indent a `w:ind` sets, its twips (`w:left`, else `w:start`) and
/// its character units (`w:leftChars`, else `w:startChars`, a zero
/// included: it cancels an inherited count) apart, as Word inherits each
/// from the most-derived style that sets it (#1815).
///
/// The first line rides along (#2231): its twips (`w:hanging` as a negative
/// value, else `w:firstLine`) and its character units (`w:hangingChars`,
/// else `w:firstLineChars`), each inherited the same way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LeftInd {
    twips: Option<i32>,
    chars: Option<i32>,
    first: Option<i32>,
    first_chars: Option<i32>,
}

impl LeftInd {
    /// `self`, with what it does not set taken from `base`.
    fn or(self, base: LeftInd) -> LeftInd {
        LeftInd {
            twips: self.twips.or(base.twips),
            chars: self.chars.or(base.chars),
            first: self.first.or(base.first),
            first_chars: self.first_chars.or(base.first_chars),
        }
    }
}

/// An on/off paragraph property a paragraph style can set, resolved by
/// [`StyleSheet::effective_ppr_flag`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PprFlag {
    PageBreakBefore,
    SuppressLineNumbers,
}

impl PprFlag {
    fn local_name(self) -> &'static str {
        match self {
            PprFlag::PageBreakBefore => "pageBreakBefore",
            PprFlag::SuppressLineNumbers => "suppressLineNumbers",
        }
    }

    fn of(self, def: &StyleDef) -> Option<bool> {
        match self {
            PprFlag::PageBreakBefore => def.page_break_before,
            PprFlag::SuppressLineNumbers => def.suppress_line_numbers,
        }
    }
}

impl StyleSheet {
    /// The family Word draws a run in when neither it nor its paragraph or
    /// character style names one (#1972): the default paragraph style's
    /// chain over the document defaults, theme fonts resolved once
    /// [`StyleSheet::apply_theme_fonts`] has run. `None` when none names a
    /// family (or only a theme font and no theme was applied).
    pub fn doc_font(&self) -> Option<&str> {
        self.doc_font.as_deref()
    }

    /// The family Word draws a run in, in a paragraph styled `pstyle`, when
    /// neither the run nor its character style names one: the paragraph
    /// style's own `basedOn` chain over the document defaults, as
    /// [`StyleSheet::effective_run`] folds it, so a style that never reaches
    /// the default paragraph style takes the defaults' family, not Normal's;
    /// without a `pStyle`, [`StyleSheet::doc_font`]. Precomputed: a read is
    /// one lookup.
    pub fn para_font(&self, pstyle: Option<&str>) -> Option<&str> {
        match pstyle {
            None => self.doc_font(),
            Some(id) => self
                .para_fonts
                .get(id)
                .map_or(self.defaults_font.as_deref(), Option::as_deref),
        }
    }

    /// Resolves every theme font reference (`w:asciiTheme`, `w:hAnsiTheme`)
    /// of the document defaults and of each style's own run properties
    /// against `theme` (#1972), before any `basedOn` folding, so a style's
    /// own theme reference beats its own `w:ascii` and a derived style's
    /// `w:ascii` still beats the theme reference it inherits. A reference
    /// the theme does not name changes nothing.
    pub fn apply_theme_fonts(&mut self, theme: &ThemeFonts) {
        self.default_run.apply_theme(theme);
        for def in self.styles.values_mut() {
            def.run.apply_theme(theme);
        }
        self.compute_fonts();
    }

    /// Fills [`StyleSheet::doc_font`] and [`StyleSheet::para_font`]'s table.
    fn compute_fonts(&mut self) {
        let plain = RunProps::default();
        self.doc_font = self.effective_run(None, None, &plain).font;
        self.defaults_font = self.default_run.font.clone();
        self.para_fonts = self
            .styles
            .keys()
            .map(|id| (id.clone(), self.effective_run(Some(id), None, &plain).font))
            .collect();
    }

    /// Every paragraph style as `(styleId, display name)`, sorted by name. Used
    /// by the Apply-Styles dialog to offer every style the document defines.
    pub fn paragraph_styles(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .styles
            .iter()
            .filter(|(_, d)| d.is_paragraph)
            .map(|(id, d)| (id.clone(), d.name.clone().unwrap_or_else(|| id.clone())))
            .collect();
        v.sort_by_key(|(_, name)| name.to_lowercase());
        v
    }

    /// Effective run properties combining defaults, the paragraph style, the
    /// character style, and direct properties.
    pub fn effective_run(
        &self,
        para_style: Option<&str>,
        run_style: Option<&str>,
        direct: &RunProps,
    ) -> RunProps {
        let mut agg = self.default_run.clone();
        if let Some(s) = para_style.or(self.default_paragraph.as_deref()) {
            let mut seen = HashSet::new();
            self.fold(&mut agg, s, &mut seen);
        }
        if let Some(s) = run_style {
            let mut seen = HashSet::new();
            self.fold(&mut agg, s, &mut seen);
        }
        RunProps {
            bold: direct.bold || agg.bold.unwrap_or(false),
            italic: direct.italic || agg.italic.unwrap_or(false),
            underline: direct.underline || agg.underline.unwrap_or(false),
            strike: direct.strike || agg.strike.unwrap_or(false),
            code: direct.code,
            caps: direct.caps,
            small_caps: direct.small_caps,
            vanish: direct.vanish,
            rtl: if direct.rtl {
                true
            } else if has_raw_child(&direct.raw_props, "rtl") {
                false
            } else {
                agg.rtl.unwrap_or(false)
            },
            vert_align: direct.vert_align,
            color: direct.color.clone().or_else(|| agg.color.clone()),
            highlight: direct.highlight.clone(),
            size_half_pts: direct.size_half_pts.or(agg.size),
            font: direct.font.clone().or_else(|| agg.font.clone()),
            style_id: direct.style_id.clone(),
            ..Default::default()
        }
    }

    /// A run as the suite draws it (#1427): the table style's look below the
    /// paragraph style's `basedOn` chain, below the character style's, below
    /// the run's direct formatting. Unlike [`StyleSheet::effective_run`] it
    /// starts from nothing (no document defaults, no default paragraph
    /// style, unless a paragraph names it), so an unstyled run comes back as it is, and a direct
    /// explicit-off (`<w:b w:val="0"/>`, kept in `raw_props`) beats every
    /// style. The character style beats the paragraph style outright, the
    /// toggle-property XOR Word applies between them is not modelled. Only
    /// the drawn fields change; the rest is the direct run's.
    pub fn shown_run(
        &self,
        table: Option<&RunLook>,
        para_style: Option<&str>,
        run_style: Option<&str>,
        direct: &RunProps,
    ) -> RunProps {
        let mut agg = PartialRun::default();
        let mut table_color = None;
        if let Some(t) = table {
            agg.bold = t.bold;
            agg.italic = t.italic;
            table_color = t.color.clone();
        }
        if let Some(s) = para_style {
            self.fold_paragraph(&mut agg, s);
        }
        if let Some(s) = run_style {
            self.fold(&mut agg, s, &mut HashSet::new());
        }
        let mut out = direct.clone();
        if !direct.bold {
            out.bold = !has_raw_child(&direct.raw_props, "b") && agg.bold.unwrap_or(false);
        }
        if !direct.italic {
            out.italic = !has_raw_child(&direct.raw_props, "i") && agg.italic.unwrap_or(false);
        }
        out.underline = direct.underline || agg.underline.unwrap_or(false);
        out.strike = direct.strike || agg.strike.unwrap_or(false);
        let explicit_auto = crate::table_styles::explicit_auto_color(direct);
        if direct.color.is_none() && !explicit_auto {
            out.color = agg.color.clone().or(table_color.flatten());
        }
        out.size_half_pts = direct.size_half_pts.or(agg.size);
        out.font = direct.font.clone().or_else(|| agg.font.clone());
        out
    }

    /// Whether a run is hidden text (`w:vanish`) once its styles are
    /// applied: a direct `w:vanish` decides, on or off (an off one stays in
    /// `raw_props`); otherwise the character style, then the paragraph style
    /// (the default one without a `w:pStyle`), then the document defaults,
    /// each through its `basedOn` chain. The most specific setting wins: the
    /// toggle-property XOR between a paragraph and a character style is not
    /// applied. Rendering does not use this (see [`StyleSheet::effective_run`]).
    pub fn effective_vanish(
        &self,
        para_style: Option<&str>,
        run_style: Option<&str>,
        direct: &RunProps,
    ) -> bool {
        if direct.vanish {
            return true;
        }
        if has_raw_child(&direct.raw_props, "vanish") {
            return false;
        }
        let mut agg = self.default_run.clone();
        if let Some(s) = para_style.or(self.default_paragraph.as_deref()) {
            self.fold(&mut agg, s, &mut HashSet::new());
        }
        if let Some(s) = run_style {
            self.fold(&mut agg, s, &mut HashSet::new());
        }
        agg.vanish.unwrap_or(false)
    }

    /// Effective alignment: direct wins, an explicit direct `w:jc="left"`
    /// (kept in `raw_props`) included; otherwise inherit from the style chain
    /// (the default paragraph style's when the paragraph names none). An
    /// editor-chosen Align Left is explicit too (`ParProps::set_align_direct`,
    /// #1752).
    pub fn effective_align(&self, para_style: Option<&str>, direct: &ParProps) -> Align {
        if direct.align != Align::Left {
            return direct.align;
        }
        if has_raw_child(&direct.raw_props, "jc") {
            return Align::Left;
        }
        if let Some(s) = para_style.or(self.default_paragraph.as_deref()) {
            let mut seen = HashSet::new();
            if let Some(a) = self.fold_align(s, &mut seen) {
                return a;
            }
        }
        Align::Left
    }

    /// Explicit or inherited paragraph direction, if OOXML provides one.
    /// `None` means layout should infer the bidi base direction from content.
    pub fn paragraph_rtl_override(
        &self,
        para_style: Option<&str>,
        direct: &ParProps,
    ) -> Option<bool> {
        if direct.rtl {
            return Some(true);
        }
        if has_raw_child(&direct.raw_props, "bidi") {
            return Some(false);
        }
        if let Some(s) = para_style.or(self.default_paragraph.as_deref()) {
            let mut seen = HashSet::new();
            if let Some(rtl) = self.fold_rtl(s, &mut seen) {
                return Some(rtl);
            }
        }
        None
    }

    /// An on/off paragraph property: a direct `w:pPr` child (on, or explicitly
    /// off with `w:val="0"`) wins; otherwise the paragraph style's chain, or the
    /// default paragraph style's when the paragraph names none; else off.
    pub fn effective_ppr_flag(
        &self,
        para_style: Option<&str>,
        direct: &ParProps,
        flag: PprFlag,
    ) -> bool {
        if let Some(on) = raw_toggle(&direct.raw_props, flag.local_name()) {
            return on;
        }
        para_style
            .or(self.default_paragraph.as_deref())
            .and_then(|s| self.fold_ppr_flag(s, flag, &mut HashSet::new()))
            .unwrap_or(false)
    }

    /// The list numbering a paragraph style gives its paragraphs, through
    /// its `w:basedOn` chain (the default paragraph style's when `para_style`
    /// is `None`): `(numId, ilvl)`, the level `None` when no style in the
    /// chain names one. A style's `w:numId` of 0 is no numbering. Heading
    /// styles are numbered this way in Word's outline lists.
    pub fn style_num_pr(&self, para_style: Option<&str>) -> Option<(i32, Option<i32>)> {
        let mut id = para_style.or(self.default_paragraph.as_deref())?;
        let mut seen = HashSet::new();
        let (mut num_id, mut ilvl) = (None, None);
        while seen.insert(id) {
            let Some(def) = self.styles.get(id) else {
                break;
            };
            if let Some((n, l)) = def.num_pr {
                num_id = num_id.or(n);
                ilvl = ilvl.or(l);
            }
            if num_id.is_some() {
                break;
            }
            let Some(base) = def.based_on.as_deref() else {
                break;
            };
            id = base;
        }
        num_id.filter(|&n| n > 0).map(|n| (n, ilvl))
    }

    fn fold_ppr_flag(&self, id: &str, flag: PprFlag, seen: &mut HashSet<String>) -> Option<bool> {
        if !seen.insert(id.to_string()) {
            return None;
        }
        let def = self.styles.get(id)?;
        flag.of(def).or_else(|| {
            def.based_on
                .as_ref()
                .and_then(|b| self.fold_ppr_flag(b, flag, seen))
        })
    }

    fn fold(&self, agg: &mut PartialRun, id: &str, seen: &mut HashSet<String>) {
        self.fold_chain(agg, id, seen, false);
    }

    /// [`StyleSheet::fold`] for a paragraph style's chain, which leaves the
    /// default paragraph style out when it is reached through `basedOn`
    /// (nearly every style is based on Normal): [`StyleSheet::shown_run`]
    /// applies it only to a paragraph that names it.
    fn fold_paragraph(&self, agg: &mut PartialRun, id: &str) {
        self.fold_chain(agg, id, &mut HashSet::new(), true);
    }

    fn fold_chain(
        &self,
        agg: &mut PartialRun,
        id: &str,
        seen: &mut HashSet<String>,
        skip_default: bool,
    ) {
        if !seen.insert(id.to_string()) {
            return;
        }
        if let Some(def) = self.styles.get(id) {
            if let Some(b) = def.based_on.as_ref().filter(|b| {
                !(skip_default && self.default_paragraph.as_deref() == Some(b.as_str()))
            }) {
                self.fold_chain(agg, b, seen, skip_default);
            }
            agg.merge(&def.run);
        }
    }

    fn fold_align(&self, id: &str, seen: &mut HashSet<String>) -> Option<Align> {
        if !seen.insert(id.to_string()) {
            return None;
        }
        let def = self.styles.get(id)?;
        let base = def.based_on.as_ref().and_then(|b| self.fold_align(b, seen));
        def.align.or(base)
    }

    fn fold_left(&self, id: &str, seen: &mut HashSet<String>) -> LeftInd {
        if !seen.insert(id.to_string()) {
            return LeftInd::default();
        }
        let Some(def) = self.styles.get(id) else {
            return LeftInd::default();
        };
        let base = def
            .based_on
            .as_ref()
            .map(|b| self.fold_left(b, seen))
            .unwrap_or_default();
        def.left.or(base)
    }

    /// The left indent attributes a paragraph inherits, each from the
    /// most-derived style in its chain that sets it, else `w:pPrDefault`.
    fn inherited_left(&self, para_style: Option<&str>) -> LeftInd {
        para_style
            .or(self.default_paragraph.as_deref())
            .map(|s| self.fold_left(s, &mut HashSet::new()))
            .unwrap_or_default()
            .or(self.default_left)
    }

    fn fold_rtl(&self, id: &str, seen: &mut HashSet<String>) -> Option<bool> {
        if !seen.insert(id.to_string()) {
            return None;
        }
        let def = self.styles.get(id)?;
        let base = def.based_on.as_ref().and_then(|b| self.fold_rtl(b, seen));
        def.rtl.or(base)
    }

    /// The left indent a paragraph with no direct one takes from its styles.
    /// The twips (`w:left`, else `w:start`) and the character units
    /// (`w:leftChars`, else `w:startChars`) are each inherited from the
    /// most-derived style in its chain (from `para_style`, else the default
    /// paragraph style, which is reached through `basedOn` too) that sets
    /// them, else from `w:docDefaults`' `w:pPrDefault`. A nonzero character
    /// count wins over the twips (as in Word) and is `Some(1)`: callers ask
    /// only whether it is nonzero ([`Self::style_left_in_chars`] says it is
    /// one). `None` when nothing sets one (#1815).
    pub fn style_left_indent(&self, para_style: Option<&str>) -> Option<i32> {
        let left = self.inherited_left(para_style);
        if left.chars.is_some_and(|c| c != 0) {
            return Some(1);
        }
        left.twips
    }

    /// The left indent, in twips, a paragraph with no direct one draws at
    /// from its styles (#2231): [`Self::style_left_indent`]'s chain, but
    /// `None` where a nonzero character count wins, as the page view does
    /// not draw character units.
    pub fn style_left_twips(&self, para_style: Option<&str>) -> Option<i32> {
        let left = self.inherited_left(para_style);
        if left.chars.is_some_and(|c| c != 0) {
            return None;
        }
        left.twips
    }

    /// The first-line indent, in twips (a hanging indent negative), a
    /// paragraph with no direct one takes from its styles, over the same
    /// chain as [`Self::style_left_twips`]; `None` where nothing sets one or
    /// a nonzero character count wins (#2231).
    pub fn style_first_line(&self, para_style: Option<&str>) -> Option<i32> {
        let ind = self.inherited_left(para_style);
        if ind.first_chars.is_some_and(|c| c != 0) {
            return None;
        }
        ind.first
    }

    /// Whether the left indent a paragraph inherits is a nonzero character
    /// count, which a direct `w:left` beats only beside a `w:leftChars="0"`.
    pub fn style_left_in_chars(&self, para_style: Option<&str>) -> bool {
        self.inherited_left(para_style)
            .chars
            .is_some_and(|c| c != 0)
    }

    /// Tab stops for a paragraph style (the most-derived style that defines any,
    /// following `basedOn`). Empty if none.
    pub fn effective_tabs(&self, para_style: Option<&str>) -> Vec<TabStop> {
        let mut seen = HashSet::new();
        para_style
            .or(self.default_paragraph.as_deref())
            .map(|s| self.fold_tabs(s, &mut seen))
            .unwrap_or_default()
    }

    fn fold_tabs(&self, id: &str, seen: &mut HashSet<String>) -> Vec<TabStop> {
        if !seen.insert(id.to_string()) {
            return Vec::new();
        }
        let Some(def) = self.styles.get(id) else {
            return Vec::new();
        };
        if !def.tabs.is_empty() {
            return def.tabs.clone();
        }
        def.based_on
            .as_ref()
            .map(|b| self.fold_tabs(b, seen))
            .unwrap_or_default()
    }

    /// Effective paragraph borders: a direct side wins; otherwise inherit the
    /// most-derived side from the style chain.
    pub fn effective_borders(&self, para_style: Option<&str>, direct: ParBorders) -> ParBorders {
        let style_b = para_style
            .or(self.default_paragraph.as_deref())
            .map(|s| self.fold_borders(s, &mut HashSet::new()))
            .unwrap_or_default();
        direct.over(style_b)
    }

    fn fold_borders(&self, id: &str, seen: &mut HashSet<String>) -> ParBorders {
        if !seen.insert(id.to_string()) {
            return ParBorders::default();
        }
        let Some(def) = self.styles.get(id) else {
            return ParBorders::default();
        };
        let base = def
            .based_on
            .as_ref()
            .map(|b| self.fold_borders(b, seen))
            .unwrap_or_default();
        def.borders.over(base)
    }
}

/// An on/off attribute that is off when absent (`w:default`).
fn toggle_attr(val: &str) -> bool {
    !val.is_empty() && toggle(val)
}

pub(crate) fn toggle(val: &str) -> bool {
    !(val == "0" || val == "false" || val == "off" || val == "none")
}

fn parse_uint(s: &str) -> u32 {
    s.bytes()
        .take_while(|b| b.is_ascii_digit())
        .fold(0u32, |a, b| a * 10 + (b - b'0') as u32)
}

fn map_align(jc: &str) -> Option<Align> {
    match jc {
        "center" => Some(Align::Center),
        "right" | "end" => Some(Align::Right),
        "both" | "distribute" => Some(Align::Justify),
        "left" | "start" => Some(Align::Left),
        _ => None,
    }
}

/// The on/off value of the first `raw_props` element named `local` (any
/// prefix): `w:val` absent is on.
fn raw_toggle(raw_props: &[String], local: &str) -> Option<bool> {
    raw_props.iter().find_map(|raw| {
        let mut parser = XmlParser::new(raw);
        (parser.next() == Event::Start && parser.name().rsplit(':').next() == Some(local))
            .then(|| toggle(parser.attr("w:val")))
    })
}

pub(crate) fn has_raw_child(raw_props: &[String], local: &str) -> bool {
    raw_props.iter().any(|raw| {
        let mut parser = XmlParser::new(raw);
        parser.next() == Event::Start
            && parser
                .name()
                .rsplit(':')
                .next()
                .is_some_and(|name| name == local)
    })
}

/// Parse `styles.xml` into a [`StyleSheet`].
pub fn parse_styles_xml(xml: &str) -> StyleSheet {
    let mut ss = StyleSheet::default();
    let mut p = XmlParser::new(xml);
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:docDefaults" => {
                    parse_doc_defaults(&mut p, &mut ss.default_run, &mut ss.default_left)
                }
                "w:style" => {
                    let id = p.attr("w:styleId").to_string();
                    let ty = p.attr("w:type");
                    // w:type defaults to "paragraph" when omitted.
                    let is_paragraph = ty.is_empty() || ty == "paragraph";
                    let is_default = toggle_attr(p.attr("w:default"));
                    let mut def = parse_style(&mut p);
                    def.is_paragraph = is_paragraph;
                    if is_paragraph
                        && is_default
                        && !id.is_empty()
                        && ss.default_paragraph.is_none()
                    {
                        ss.default_paragraph = Some(id.clone());
                    }
                    if !id.is_empty() {
                        ss.styles.insert(id, def);
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    ss.compute_fonts();
    ss
}

fn parse_style(p: &mut XmlParser) -> StyleDef {
    let mut def = StyleDef::default();
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:basedOn" => {
                    let v = p.attr("w:val");
                    if !v.is_empty() {
                        def.based_on = Some(v.to_string());
                    }
                    p.skip_element();
                }
                "w:name" => {
                    let v = p.attr("w:val");
                    if !v.is_empty() {
                        def.name = Some(v.to_string());
                    }
                    p.skip_element();
                }
                "w:rPr" => parse_partial_rpr(p, &mut def.run),
                "w:pPr" => parse_style_ppr(p, &mut def),
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    def
}

fn parse_doc_defaults(p: &mut XmlParser, run: &mut PartialRun, left: &mut LeftInd) {
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:rPrDefault" => parse_rpr_default(p, run),
                "w:pPrDefault" => parse_ppr_default(p, left),
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

/// `w:pPrDefault`: only its `w:pPr/w:ind` left indent is read.
fn parse_ppr_default(p: &mut XmlParser, left: &mut LeftInd) {
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:pPr" => {
                    let mut def = StyleDef::default();
                    parse_style_ppr(p, &mut def);
                    *left = def.left;
                }
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

fn parse_rpr_default(p: &mut XmlParser, run: &mut PartialRun) {
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:rPr" => parse_partial_rpr(p, run),
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

fn parse_partial_rpr(p: &mut XmlParser, run: &mut PartialRun) {
    // The same precedence as direct formatting (`RprPairs`).
    let mut pairs = crate::load::RprPairs::default();
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name();
                let val = p.attr("w:val");
                if pairs.read(name, val) {
                    p.skip_element();
                    continue;
                }
                match name {
                    "w:u" => run.underline = Some(toggle(val)),
                    "w:rtl" => run.rtl = Some(toggle(val)),
                    "w:vanish" => run.vanish = Some(toggle(val)),
                    "w:color" => {
                        if !val.is_empty() && val != "auto" {
                            run.color = Some(val.to_ascii_uppercase());
                        }
                    }
                    "w:rFonts" => {
                        let a = p.attr("w:ascii");
                        if !a.is_empty() {
                            run.font = Some(a.to_string());
                        }
                        let t = [p.attr("w:asciiTheme"), p.attr("w:hAnsiTheme")]
                            .into_iter()
                            .find(|t| !t.is_empty());
                        if let Some(t) = t {
                            run.font_theme = Some(t.to_string());
                        }
                    }
                    _ => {}
                }
                p.skip_element();
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    if let Some(on) = pairs.bold() {
        run.bold = Some(on);
    }
    if let Some(on) = pairs.italic() {
        run.italic = Some(on);
    }
    if let Some(on) = pairs.strike() {
        run.strike = Some(on);
    }
    if let Some(v) = pairs.size() {
        run.size = Some(v);
    }
}

fn parse_style_ppr(p: &mut XmlParser, def: &mut StyleDef) {
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:jc" => {
                    def.align = map_align(p.attr("w:val"));
                    p.skip_element();
                }
                "w:bidi" => {
                    def.rtl = Some(toggle(p.attr("w:val")));
                    p.skip_element();
                }
                "w:tabs" => parse_tabs(p, &mut def.tabs),
                "w:pBdr" => def.borders = parse_pbdr(p),
                "w:pageBreakBefore" => {
                    def.page_break_before = Some(toggle(p.attr("w:val")));
                    p.skip_element();
                }
                "w:suppressLineNumbers" => {
                    def.suppress_line_numbers = Some(toggle(p.attr("w:val")));
                    p.skip_element();
                }
                "w:numPr" => def.num_pr = Some(parse_style_numpr(p)),
                "w:ind" => {
                    def.left = parse_ind_left(p);
                    p.skip_element();
                }
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

/// A `w:ind`'s (at its start tag) left and first line in twips: `w:left`,
/// else `w:start`; `w:hanging` as a negative first line, else
/// `w:firstLine`. An attribute left out, or not a number, is `None`. Styles
/// and list levels read it alike (#2231).
pub(crate) fn ind_twips(p: &XmlParser) -> (Option<i32>, Option<i32>) {
    let num = |name: &str| p.attr(name).trim().parse::<i32>().ok();
    (
        num("w:left").or_else(|| num("w:start")),
        num("w:hanging").map(|h| -h).or_else(|| num("w:firstLine")),
    )
}

/// The left indent attributes of a `w:ind` (at its start tag).
fn parse_ind_left(p: &XmlParser) -> LeftInd {
    let num = |name: &str| p.attr(name).trim().parse::<i32>().ok();
    let (twips, first) = ind_twips(p);
    LeftInd {
        twips,
        chars: num("w:leftChars").or_else(|| num("w:startChars")),
        first,
        first_chars: num("w:hangingChars")
            .map(|h| -h)
            .or_else(|| num("w:firstLineChars")),
    }
}

/// A style's `<w:numPr>`: `(numId, ilvl)`, each when present.
fn parse_style_numpr(p: &mut XmlParser) -> (Option<i32>, Option<i32>) {
    let mut out = (None, None);
    loop {
        match p.next() {
            Event::Start => {
                let val = p.attr("w:val").trim().parse::<i32>().ok();
                match p.name() {
                    "w:numId" => out.0 = val,
                    "w:ilvl" => out.1 = val,
                    _ => {}
                }
                p.skip_element();
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    out
}

/// Parse a `<w:pBdr>` element's five sides into [`ParBorders`]. Lenient: a
/// bad number takes Word's default (`w:sz` 4, `w:space` 0) and a bad colour
/// is automatic. A `nil`/`none` side draws no line and is recorded as a
/// cancel ([`ParBorders::nil`]), which hides a style's side below it; a
/// value with no line in this model (an art border) is left unsaid.
pub fn parse_pbdr(p: &mut XmlParser) -> ParBorders {
    let mut b = ParBorders::default();
    loop {
        match p.next() {
            Event::Start => {
                let side = BorderKind::from_val(p.attr("w:val")).map(|kind| {
                    let num = |a: &str| p.attr(a).trim().parse::<u16>().ok();
                    let color = crate::theme::hex_rgb(p.attr("w:color"));
                    let byte = |a: &str| u8::from_str_radix(p.attr(a).trim(), 16).ok();
                    ParBorder {
                        kind,
                        sz: num("w:sz").unwrap_or(4),
                        space: num("w:space").unwrap_or(0),
                        color,
                        theme: ThemeColor::from_val(p.attr("w:themeColor")).map(|name| {
                            ThemeColorRef {
                                name,
                                tint: byte("w:themeTint"),
                                shade: byte("w:themeShade"),
                            }
                        }),
                    }
                });
                let i = match p.name() {
                    "w:top" => Some(0),
                    "w:left" | "w:start" => Some(1),
                    "w:bottom" => Some(2),
                    "w:right" | "w:end" => Some(3),
                    "w:between" => Some(4),
                    _ => None,
                };
                // A `nil`/`none` side is no line, which cancels a style's
                // (#2231 r1); another value this model has no line for is
                // left unsaid. `w:left` and `w:start` are one side: the first
                // that draws wins.
                let cancels = matches!(p.attr("w:val"), "nil" | "none");
                if let Some(i) = i {
                    let start_end = i == 1 || i == 3;
                    let taken = start_end && b.sides()[i].1.is_some();
                    if (side.is_some() || cancels) && !taken {
                        b.set_side(i, side);
                    }
                }
                p.skip_element();
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
    b
}

fn parse_tabs(p: &mut XmlParser, tabs: &mut Vec<TabStop>) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "w:tab" {
                    let val = p.attr("w:val");
                    if val != "clear" {
                        let align = match val {
                            "right" | "end" => TabAlign::Right,
                            "center" => TabAlign::Center,
                            _ => TabAlign::Left,
                        };
                        let leader = match p.attr("w:leader") {
                            "dot" | "middleDot" => TabLeader::Dot,
                            "hyphen" => TabLeader::Hyphen,
                            "underscore" => TabLeader::Underscore,
                            _ => TabLeader::None,
                        };
                        tabs.push(TabStop {
                            pos: parse_uint(p.attr("w:pos")) as i32,
                            align,
                            leader,
                        });
                    }
                }
                p.skip_element();
            }
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paragraph_styles_lists_names_sorted_excluding_char_styles() {
        let xml = r#"<w:styles>
            <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/></w:style>
            <w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
            <w:style w:type="character" w:styleId="Strong"><w:name w:val="Strong"/></w:style>
        </w:styles>"#;
        let ps = parse_styles_xml(xml).paragraph_styles();
        // The character style is excluded; the rest sort by display name.
        assert_eq!(
            ps,
            vec![
                ("Heading1".to_string(), "heading 1".to_string()),
                ("Normal".to_string(), "Normal".to_string()),
            ]
        );
    }

    fn shown_sheet() -> StyleSheet {
        parse_styles_xml(
            r#"<w:styles>
            <w:style w:type="paragraph" w:styleId="Heading1"><w:rPr><w:b/>
              <w:rFonts w:ascii="Georgia"/><w:color w:val="1F3864"/><w:sz w:val="32"/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="Plain"><w:rPr><w:b w:val="0"/></w:rPr></w:style>
            <w:style w:type="character" w:styleId="Strong"><w:rPr><w:b/></w:rPr></w:style>
            <w:style w:type="character" w:styleId="Red"><w:rPr><w:color w:val="FF0000"/></w:rPr></w:style>
            <w:style w:type="character" w:styleId="Lite"><w:basedOn w:val="Strong"/>
              <w:rPr><w:i/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="A"><w:basedOn w:val="B"/><w:rPr><w:i/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="B"><w:basedOn w:val="A"/></w:style>
            </w:styles>"#,
        )
    }

    fn off(local: &str) -> RunProps {
        RunProps {
            raw_props: vec![format!("<w:{local} w:val=\"0\"/>")],
            ..Default::default()
        }
    }

    #[test]
    fn shown_run_paragraph_style_bold_size_color() {
        let r = shown_sheet().shown_run(None, Some("Heading1"), None, &RunProps::default());
        assert!(r.bold);
        assert_eq!(r.size_half_pts, Some(32));
        assert_eq!(r.color.as_deref(), Some("1F3864"));
        assert_eq!(r.font.as_deref(), Some("Georgia"));
        let direct = RunProps {
            size_half_pts: Some(20),
            color: Some("00FF00".into()),
            font: Some("Arial".into()),
            ..Default::default()
        };
        let r = shown_sheet().shown_run(None, Some("Heading1"), None, &direct);
        assert_eq!(r.size_half_pts, Some(20));
        assert_eq!(r.color.as_deref(), Some("00FF00"));
        assert_eq!(r.font.as_deref(), Some("Arial"));
    }

    #[test]
    fn shown_run_char_style_beats_paragraph_style() {
        let ss = shown_sheet();
        let r = ss.shown_run(None, Some("Plain"), Some("Strong"), &RunProps::default());
        assert!(
            r.bold,
            "the character style's bold beats the paragraph's off"
        );
        let r = ss.shown_run(None, Some("Heading1"), Some("Red"), &RunProps::default());
        assert!(r.bold, "layers merge");
        assert_eq!(r.color.as_deref(), Some("FF0000"));
        let r = ss.shown_run(None, None, Some("Lite"), &RunProps::default());
        assert!(r.bold && r.italic, "basedOn is followed");
    }

    #[test]
    fn shown_run_direct_explicit_off_beats_style() {
        let ss = shown_sheet();
        let r = ss.shown_run(None, Some("Heading1"), Some("Strong"), &off("b"));
        assert!(!r.bold);
        let look = RunLook {
            italic: Some(true),
            ..Default::default()
        };
        let r = ss.shown_run(Some(&look), None, None, &off("i"));
        assert!(!r.italic);
    }

    #[test]
    fn shown_run_table_below_paragraph() {
        let ss = shown_sheet();
        let look = RunLook {
            bold: Some(true),
            ..Default::default()
        };
        let r = ss.shown_run(Some(&look), None, None, &RunProps::default());
        assert!(r.bold);
        let r = ss.shown_run(Some(&look), Some("Plain"), None, &RunProps::default());
        assert!(!r.bold, "the paragraph style's off beats the table's bold");
        let coloured = RunLook {
            color: Some(Some("0000FF".into())),
            ..Default::default()
        };
        let r = ss.shown_run(
            Some(&coloured),
            Some("Heading1"),
            None,
            &RunProps::default(),
        );
        assert_eq!(r.color.as_deref(), Some("1F3864"));
    }

    #[test]
    fn shown_run_no_styles_leaves_run_unchanged_and_ignores_doc_defaults() {
        let ss = parse_styles_xml(
            r#"<w:styles><w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="40"/><w:b/>
            </w:rPr></w:rPrDefault></w:docDefaults>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal">
            <w:rPr><w:i/></w:rPr></w:style></w:styles>"#,
        );
        let run = RunProps {
            underline: true,
            color: Some("123456".into()),
            ..Default::default()
        };
        assert_eq!(ss.shown_run(None, None, None, &run), run);
        assert_eq!(
            ss.shown_run(None, None, None, &RunProps::default()),
            RunProps::default()
        );
    }

    const NORMAL: &str = r#"<w:styles>
        <w:style w:type="paragraph" w:default="1" w:styleId="Normal">
          <w:rPr><w:rFonts w:ascii="Calibri"/><w:sz w:val="24"/></w:rPr></w:style>
        <w:style w:type="paragraph" w:styleId="Heading2"><w:basedOn w:val="Normal"/>
          <w:rPr><w:b/></w:rPr></w:style></w:styles>"#;

    #[test]
    fn explicit_normal_pstyle_run_props_now_draw() {
        let ss = parse_styles_xml(NORMAL);
        let r = ss.shown_run(None, Some("Normal"), None, &RunProps::default());
        assert_eq!(r.size_half_pts, Some(24));
        assert_eq!(r.font.as_deref(), Some("Calibri"));
    }

    #[test]
    fn normal_does_not_reach_a_style_through_basedon() {
        let ss = parse_styles_xml(NORMAL);
        let r = ss.shown_run(None, Some("Heading2"), None, &RunProps::default());
        assert!(r.bold);
        assert_eq!(r.size_half_pts, None, "the heading keeps the host's size");
        assert_eq!(r.font, None);
    }

    #[test]
    fn shown_run_basedon_cycle_terminates() {
        let r = shown_sheet().shown_run(None, Some("A"), None, &RunProps::default());
        assert!(r.italic);
    }

    #[test]
    fn paragraph_style_supplies_bold_and_size() {
        let xml = r#"<w:styles><w:style w:type="paragraph" w:styleId="Heading1">
            <w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style></w:styles>"#;
        let ss = parse_styles_xml(xml);
        let eff = ss.effective_run(Some("Heading1"), None, &RunProps::default());
        assert!(eff.bold);
        assert_eq!(eff.size_half_pts, Some(32));
    }

    /// Review r2: a style's twins resolve like direct formatting's: the
    /// primary wins over its complex-script twin in either order, and
    /// `w:dstrike` adds to `w:strike` (#1063).
    #[test]
    fn style_twins_resolve_like_direct_formatting() {
        let rpr = [
            "<w:sz w:val=\"22\"/><w:szCs w:val=\"28\"/><w:b w:val=\"0\"/><w:bCs/>\
             <w:strike/><w:dstrike w:val=\"0\"/>",
            "<w:szCs w:val=\"28\"/><w:sz w:val=\"22\"/><w:bCs/><w:b w:val=\"0\"/>\
             <w:dstrike w:val=\"0\"/><w:strike/>",
        ];
        for rpr in rpr {
            let xml = format!(
                "<w:styles><w:style w:type=\"paragraph\" w:styleId=\"S\"><w:rPr>{rpr}</w:rPr>\
                 </w:style></w:styles>"
            );
            let eff = parse_styles_xml(&xml).effective_run(Some("S"), None, &RunProps::default());
            assert_eq!(eff.size_half_pts, Some(22), "{rpr}");
            assert!(!eff.bold && eff.strike, "{rpr}");
            let doc = crate::load::parse_document_xml(
                &format!(
                    "<w:document><w:body><w:p><w:r><w:rPr>{rpr}</w:rPr><w:t>x</w:t></w:r></w:p>\
                     </w:body></w:document>"
                ),
                &Default::default(),
            );
            let crate::model::Block::Paragraph(p) = &doc.body[0] else {
                panic!("paragraph");
            };
            let crate::model::Inline::Run(r) = &p.content[0] else {
                panic!("run");
            };
            assert_eq!(r.props.size_half_pts, Some(22), "{rpr}");
            assert!(!r.props.bold && r.props.strike, "{rpr}");
        }
    }

    #[test]
    fn based_on_chain_accumulates() {
        let xml = r#"<w:styles>
            <w:style w:styleId="Base"><w:rPr><w:b/></w:rPr></w:style>
            <w:style w:styleId="Derived"><w:basedOn w:val="Base"/><w:rPr><w:i/></w:rPr></w:style>
            </w:styles>"#;
        let ss = parse_styles_xml(xml);
        let eff = ss.effective_run(Some("Derived"), None, &RunProps::default());
        assert!(eff.bold && eff.italic);
    }

    #[test]
    fn direct_wins_but_inherits_when_unset() {
        let xml = r#"<w:styles><w:style w:styleId="S"><w:rPr><w:color w:val="ff0000"/></w:rPr></w:style></w:styles>"#;
        let ss = parse_styles_xml(xml);
        let direct = RunProps {
            color: Some("00FF00".to_string()),
            ..RunProps::default()
        };
        assert_eq!(
            ss.effective_run(Some("S"), None, &direct).color.as_deref(),
            Some("00FF00")
        );
        assert_eq!(
            ss.effective_run(Some("S"), None, &RunProps::default())
                .color
                .as_deref(),
            Some("FF0000")
        );
    }

    #[test]
    fn doc_defaults_apply() {
        let xml = r#"<w:styles><w:docDefaults><w:rPrDefault><w:rPr>
            <w:rFonts w:ascii="Calibri"/></w:rPr></w:rPrDefault></w:docDefaults></w:styles>"#;
        let ss = parse_styles_xml(xml);
        assert_eq!(
            ss.effective_run(None, None, &RunProps::default())
                .font
                .as_deref(),
            Some("Calibri")
        );
    }

    #[test]
    fn character_style_resolves() {
        let xml = r#"<w:styles><w:style w:type="character" w:styleId="Strong"><w:rPr><w:b/></w:rPr></w:style></w:styles>"#;
        let ss = parse_styles_xml(xml);
        assert!(
            ss.effective_run(None, Some("Strong"), &RunProps::default())
                .bold
        );
    }

    #[test]
    fn run_rtl_resolves_from_styles() {
        let xml = r#"<w:styles>
            <w:docDefaults><w:rPrDefault><w:rPr><w:rtl/></w:rPr></w:rPrDefault></w:docDefaults>
            <w:style w:type="character" w:styleId="Latin"><w:rPr><w:rtl w:val="0"/></w:rPr></w:style>
            <w:style w:type="character" w:styleId="Hebrew"><w:rPr><w:rtl/></w:rPr></w:style>
            </w:styles>"#;
        let ss = parse_styles_xml(xml);
        assert!(ss.effective_run(None, None, &RunProps::default()).rtl);
        assert!(
            !ss.effective_run(None, Some("Latin"), &RunProps::default())
                .rtl
        );
        assert!(
            ss.effective_run(None, Some("Hebrew"), &RunProps::default())
                .rtl
        );
        let direct_off = RunProps {
            raw_props: vec!["<w:rtl w:val=\"0\"/>".to_string()],
            ..RunProps::default()
        };
        assert!(!ss.effective_run(None, Some("Hebrew"), &direct_off).rtl);
    }

    #[test]
    fn alignment_inherited_from_style() {
        let xml = r#"<w:styles><w:style w:styleId="Centered"><w:pPr><w:jc w:val="center"/></w:pPr></w:style></w:styles>"#;
        let ss = parse_styles_xml(xml);
        let with = |align| ParProps {
            align,
            ..Default::default()
        };
        assert_eq!(
            ss.effective_align(Some("Centered"), &with(Align::Left)),
            Align::Center
        );
        assert_eq!(
            ss.effective_align(Some("Centered"), &with(Align::Right)),
            Align::Right
        ); // direct wins
    }

    /// #797: a direct `w:jc="left"` (kept in `raw_props`) beats a centred
    /// style, named or default.
    #[test]
    fn explicit_direct_left_beats_a_centred_style_797() {
        let ss = parse_styles_xml(
            r#"<w:styles>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:pPr><w:jc w:val="center"/></w:pPr></w:style>
            <w:style w:type="paragraph" w:styleId="Centered"><w:pPr><w:jc w:val="center"/></w:pPr></w:style>
        </w:styles>"#,
        );
        let (_, left) = par(None, &[r#"<w:jc w:val="left"/>"#]);
        assert_eq!(ss.effective_align(None, &left), Align::Left);
        assert_eq!(ss.effective_align(Some("Centered"), &left), Align::Left);
        let plain = ParProps::default();
        assert_eq!(ss.effective_align(None, &plain), Align::Center);
        assert_eq!(ss.effective_align(Some("Centered"), &plain), Align::Center);
    }

    #[test]
    fn paragraph_rtl_inherited_from_style() {
        let xml = r#"<w:styles>
            <w:style w:styleId="Base"><w:pPr><w:bidi/></w:pPr></w:style>
            <w:style w:styleId="Derived"><w:basedOn w:val="Base"/><w:pPr><w:bidi w:val="0"/></w:pPr></w:style>
        </w:styles>"#;
        let ss = parse_styles_xml(xml);
        let mut direct = ParProps::default();
        assert_eq!(ss.paragraph_rtl_override(Some("Base"), &direct), Some(true));
        assert_eq!(
            ss.paragraph_rtl_override(Some("Derived"), &direct),
            Some(false)
        );
        direct.rtl = true;
        assert_eq!(
            ss.paragraph_rtl_override(Some("Derived"), &direct),
            Some(true)
        );
        direct.rtl = false;
        direct.raw_props = vec!["<w:bidi w:val=\"0\"/>".to_string()];
        assert_eq!(
            ss.paragraph_rtl_override(Some("Base"), &direct),
            Some(false)
        );
    }

    #[test]
    fn paragraph_border_resolved_from_style() {
        let xml = r#"<w:styles><w:style w:styleId="Rule"><w:pPr>
            <w:pBdr><w:bottom w:val="single" w:sz="6"/></w:pBdr></w:pPr></w:style></w:styles>"#;
        let ss = parse_styles_xml(xml);
        let b = ss.effective_borders(Some("Rule"), ParBorders::default());
        assert_eq!(
            b.bottom.map(|b| (b.kind, b.sz)),
            Some((BorderKind::Single, 6))
        );
        assert_eq!(b.top, None);
        // a direct border wins over the style's
        let direct = ParBorders::bottom_rule(BorderKind::Double);
        assert_eq!(
            ss.effective_borders(Some("Rule"), direct)
                .bottom
                .map(|b| b.kind),
            Some(BorderKind::Double)
        );
    }

    /// #2231: every side with its width, space, colour and theme colour; a
    /// bad number takes Word's default, `nil` is no side.
    #[test]
    fn pbdr_reads_all_five_sides_and_their_attributes() {
        let xml = r#"<w:styles><w:style w:styleId="Title"><w:pPr><w:pBdr>
            <w:top w:val="double" w:sz="12" w:space="2" w:color="FF0000"/>
            <w:left w:val="single" w:sz="x" w:space="-1" w:color="zz"/>
            <w:bottom w:val="single" w:sz="8" w:space="4" w:color="4F81BD" w:themeColor="accent1" w:themeShade="BF"/>
            <w:right w:val="nil"/>
            <w:between w:val="dashed" w:sz="4" w:space="1" w:color="auto"/>
            </w:pBdr></w:pPr></w:style></w:styles>"#;
        let b = parse_styles_xml(xml).effective_borders(Some("Title"), ParBorders::default());
        let top = b.top.unwrap();
        assert_eq!(
            (top.kind, top.sz, top.space, top.color, top.theme),
            (BorderKind::Double, 12, 2, Some(0xFF0000), None)
        );
        let left = b.left.unwrap();
        assert_eq!((left.sz, left.space, left.color), (4, 0, None));
        let bottom = b.bottom.unwrap();
        assert_eq!(
            (bottom.sz, bottom.space, bottom.color),
            (8, 4, Some(0x4F81BD))
        );
        assert_eq!(
            bottom.theme,
            Some(ThemeColorRef {
                name: ThemeColor::Accent1,
                tint: None,
                shade: Some(0xBF),
            })
        );
        assert_eq!(b.right, None);
        assert_eq!(
            b.between.map(|b| (b.kind, b.color)),
            Some((BorderKind::Dashed, None))
        );
    }

    /// #2231 r1: a side given as `none` (Word's removed rule) or `nil`
    /// cancels the style's below it, direct or in a derived style; another
    /// side still comes through.
    #[test]
    fn a_none_side_cancels_the_styles_below() {
        let xml = r#"<w:styles>
            <w:style w:styleId="Title"><w:pPr><w:pBdr><w:top w:val="single"/><w:bottom w:val="single" w:sz="8" w:color="4F81BD"/></w:pBdr></w:pPr></w:style>
            <w:style w:styleId="Plain"><w:basedOn w:val="Title"/><w:pPr><w:pBdr><w:bottom w:val="nil"/></w:pBdr></w:pPr></w:style>
            <w:style w:styleId="Again"><w:basedOn w:val="Plain"/><w:pPr><w:pBdr><w:bottom w:val="double"/></w:pBdr></w:pPr></w:style>
            </w:styles>"#;
        let ss = parse_styles_xml(xml);
        let direct = parse_document_borders(
            r#"<w:pBdr><w:bottom w:val="none" w:sz="0" w:space="0" w:color="auto"/></w:pBdr>"#,
        );
        assert_eq!(direct.bottom, None);
        assert!(direct.is_nil(2) && !direct.is_empty() && !direct.any());
        let shown = ss.effective_borders(Some("Title"), direct);
        assert_eq!(shown.bottom, None, "the direct none cancels Title's rule");
        assert_eq!(shown.top.map(|b| b.kind), Some(BorderKind::Single));
        let derived = ss.effective_borders(Some("Plain"), ParBorders::default());
        assert_eq!(
            derived.bottom, None,
            "a derived style's nil cancels its base's"
        );
        assert!(derived.top.is_some());
        // A line further up the chain draws again.
        let again = ss.effective_borders(Some("Again"), ParBorders::default());
        assert_eq!(again.bottom.map(|b| b.kind), Some(BorderKind::Double));
        // With only cancels left, nothing draws.
        let none_at_all = ParBorders::default().over(direct);
        assert!(!none_at_all.any());
    }

    fn parse_document_borders(pbdr: &str) -> ParBorders {
        let mut p = XmlParser::new(pbdr);
        assert_eq!(p.next(), Event::Start);
        parse_pbdr(&mut p)
    }

    /// #2231: each side folds through the `basedOn` chain on its own, and
    /// a direct side wins only for that side.
    #[test]
    fn pbdr_sides_fold_through_the_chain_apart() {
        let xml = r#"<w:styles>
            <w:style w:styleId="Base"><w:pPr><w:pBdr><w:top w:val="single"/><w:left w:val="single"/></w:pBdr></w:pPr></w:style>
            <w:style w:styleId="Mid"><w:basedOn w:val="Base"/><w:pPr><w:pBdr><w:bottom w:val="double"/></w:pBdr></w:pPr></w:style>
            <w:style w:styleId="Top"><w:basedOn w:val="Mid"/><w:pPr><w:pBdr><w:top w:val="dotted"/></w:pBdr></w:pPr></w:style>
            </w:styles>"#;
        let ss = parse_styles_xml(xml);
        let kinds = |b: ParBorders| b.sides().map(|(_, s)| s.map(|s| s.kind));
        use BorderKind::*;
        assert_eq!(
            kinds(ss.effective_borders(Some("Top"), ParBorders::default())),
            [Some(Dotted), Some(Single), Some(Double), None, None]
        );
        let direct = ParBorders {
            right: Some(ParBorder::rule(Wavy)),
            ..ParBorders::default()
        };
        assert_eq!(
            kinds(ss.effective_borders(Some("Top"), direct)),
            [Some(Dotted), Some(Single), Some(Double), Some(Wavy), None]
        );
    }

    #[test]
    fn unknown_style_is_noop() {
        let ss = parse_styles_xml("<w:styles/>");
        let d = RunProps {
            bold: true,
            ..RunProps::default()
        };
        assert_eq!(ss.effective_run(Some("Nope"), None, &d), d);
    }
    fn par(style: Option<&str>, raw: &[&str]) -> (Option<String>, ParProps) {
        let props = ParProps {
            raw_props: raw.iter().map(|r| r.to_string()).collect(),
            ..Default::default()
        };
        (style.map(str::to_string), props)
    }

    #[test]
    fn page_break_before_inherits_through_based_on() {
        let ss = parse_styles_xml(
            r#"<w:styles>
            <w:style w:type="paragraph" w:styleId="Base"><w:pPr><w:pageBreakBefore/></w:pPr></w:style>
            <w:style w:type="paragraph" w:styleId="Child"><w:basedOn w:val="Base"/></w:style>
            <w:style w:type="paragraph" w:styleId="Off"><w:basedOn w:val="Base"/><w:pPr><w:pageBreakBefore w:val="0"/></w:pPr></w:style>
            <w:style w:type="paragraph" w:styleId="Plain"/>
        </w:styles>"#,
        );
        let flag = |style: Option<&str>, raw: &[&str]| {
            let (style, props) = par(style, raw);
            ss.effective_ppr_flag(style.as_deref(), &props, PprFlag::PageBreakBefore)
        };
        assert!(flag(Some("Base"), &[]));
        assert!(flag(Some("Child"), &[]), "inherited through basedOn");
        assert!(!flag(Some("Off"), &[]), "a derived style turns it off");
        assert!(!flag(Some("Plain"), &[]));
        assert!(!flag(None, &[]));
        // Direct properties win either way.
        assert!(!flag(
            Some("Child"),
            &["<w:pageBreakBefore w:val=\"false\"/>"]
        ));
        assert!(flag(Some("Plain"), &["<w:pageBreakBefore/>"]));
        // The other flag is independent.
        let (style, props) = par(Some("Child"), &[]);
        assert!(!ss.effective_ppr_flag(style.as_deref(), &props, PprFlag::SuppressLineNumbers));
    }

    /// #1815: the left indent a paragraph takes from its styles.
    #[test]
    fn style_left_indent_follows_the_chain() {
        let ss = parse_styles_xml(
            r#"<w:styles>
            <w:style w:styleId="Base"><w:pPr><w:ind w:left="720"/></w:pPr></w:style>
            <w:style w:styleId="Child"><w:basedOn w:val="Base"/></w:style>
            <w:style w:styleId="Flat"><w:basedOn w:val="Base"/><w:pPr><w:ind w:left="0"/></w:pPr></w:style>
            <w:style w:styleId="Start"><w:pPr><w:ind w:start="360"/></w:pPr></w:style>
            <w:style w:styleId="Both"><w:pPr><w:ind w:left="200" w:start="360"/></w:pPr></w:style>
            <w:style w:styleId="Neg"><w:pPr><w:ind w:left="-360"/></w:pPr></w:style>
            <w:style w:styleId="Chars"><w:pPr><w:ind w:left="0" w:leftChars="200"/></w:pPr></w:style>
            <w:style w:styleId="StartChars"><w:pPr><w:ind w:startChars="100"/></w:pPr></w:style>
            <w:style w:styleId="ZeroChars"><w:pPr><w:ind w:leftChars="0" w:left="720"/></w:pPr></w:style>
            <w:style w:styleId="Hanging"><w:pPr><w:ind w:hanging="360"/></w:pPr></w:style>
            <w:style w:styleId="A"><w:basedOn w:val="B"/></w:style>
            <w:style w:styleId="B"><w:basedOn w:val="A"/></w:style>
            </w:styles>"#,
        );
        let left = |s: &str| ss.style_left_indent(Some(s));
        assert_eq!(left("Base"), Some(720));
        assert_eq!(left("Child"), Some(720), "basedOn");
        assert_eq!(left("Flat"), Some(0), "most-derived wins");
        assert_eq!(left("Start"), Some(360));
        assert_eq!(left("Both"), Some(200), "w:left before w:start");
        assert_eq!(left("Neg"), Some(-360));
        assert_eq!(left("Chars"), Some(1), "characters win over twips");
        assert_eq!(left("StartChars"), Some(1));
        assert_eq!(left("ZeroChars"), Some(720), "a zero count is absent");
        assert_eq!(left("Hanging"), None);
        assert_eq!(left("A"), None, "a cycle ends");
        assert!(ss.style_left_in_chars(Some("Chars")));
        assert!(!ss.style_left_in_chars(Some("ZeroChars")));
        assert!(!ss.style_left_in_chars(Some("Base")));
        assert_eq!(left("Missing"), None);
        assert_eq!(ss.style_left_indent(None), None);
    }

    /// #2231: the twips the page view draws a style's indent at, the first
    /// line too, each from the most-derived style that sets it; a nonzero
    /// character count is not a width.
    #[test]
    fn style_left_twips_and_first_line_follow_the_chain() {
        let ss = parse_styles_xml(
            r#"<w:styles>
            <w:docDefaults><w:pPrDefault><w:pPr><w:ind w:firstLine="100"/></w:pPr></w:pPrDefault></w:docDefaults>
            <w:style w:type="paragraph" w:styleId="ListParagraph"><w:pPr><w:ind w:left="720"/></w:pPr></w:style>
            <w:style w:type="paragraph" w:styleId="Hang"><w:basedOn w:val="ListParagraph"/><w:pPr><w:ind w:hanging="360"/></w:pPr></w:style>
            <w:style w:type="paragraph" w:styleId="Chars"><w:basedOn w:val="Hang"/><w:pPr><w:ind w:leftChars="200" w:hangingChars="100"/></w:pPr></w:style>
            </w:styles>"#,
        );
        let both = |s: Option<&str>| (ss.style_left_twips(s), ss.style_first_line(s));
        assert_eq!(both(Some("ListParagraph")), (Some(720), Some(100)));
        assert_eq!(both(Some("Hang")), (Some(720), Some(-360)));
        assert_eq!(both(Some("Chars")), (None, None));
        assert_eq!(ss.style_left_indent(Some("Chars")), Some(1));
        assert_eq!(both(None), (None, Some(100)));
    }

    /// #1815 r1: the twips and the character units are inherited apart, so
    /// a derived `w:left` does not hide a base's character count, and a
    /// derived zero count cancels it.
    #[test]
    fn style_left_indent_inherits_twips_and_chars_apart() {
        let ss = parse_styles_xml(
            r#"<w:styles>
            <w:docDefaults><w:pPrDefault><w:pPr><w:ind w:leftChars="300"/></w:pPr></w:pPrDefault></w:docDefaults>
            <w:style w:styleId="Base"><w:pPr><w:ind w:leftChars="200"/></w:pPr></w:style>
            <w:style w:styleId="Twips"><w:basedOn w:val="Base"/><w:pPr><w:ind w:left="0"/></w:pPr></w:style>
            <w:style w:styleId="Cancel"><w:basedOn w:val="Base"/><w:pPr><w:ind w:leftChars="0" w:left="360"/></w:pPr></w:style>
            <w:style w:styleId="Own"><w:pPr><w:ind w:left="720"/></w:pPr></w:style>
            </w:styles>"#,
        );
        assert_eq!(ss.style_left_indent(Some("Twips")), Some(1));
        assert!(ss.style_left_in_chars(Some("Twips")));
        assert_eq!(ss.style_left_indent(Some("Cancel")), Some(360));
        assert!(!ss.style_left_in_chars(Some("Cancel")));
        // The pPrDefault's count is inherited under a style's twips too.
        assert_eq!(ss.style_left_indent(Some("Own")), Some(1));
    }

    /// #1815: the default paragraph style applies without a `w:pStyle`, and
    /// `w:pPrDefault` is the last fallback.
    #[test]
    fn style_left_indent_falls_back_to_defaults() {
        let ss = parse_styles_xml(
            r#"<w:styles>
            <w:docDefaults><w:pPrDefault><w:pPr><w:ind w:left="100"/></w:pPr></w:pPrDefault></w:docDefaults>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:pPr><w:ind w:left="720"/></w:pPr></w:style>
            <w:style w:styleId="Plain"/>
            <w:style w:styleId="OnNormal"><w:basedOn w:val="Normal"/></w:style>
            </w:styles>"#,
        );
        assert_eq!(ss.style_left_indent(None), Some(720));
        assert_eq!(ss.style_left_indent(Some("OnNormal")), Some(720));
        assert_eq!(ss.style_left_indent(Some("Plain")), Some(100));
        let ss = parse_styles_xml(
            r#"<w:styles><w:docDefaults><w:pPrDefault><w:pPr><w:ind w:left="100"/></w:pPr></w:pPrDefault></w:docDefaults></w:styles>"#,
        );
        assert_eq!(ss.style_left_indent(None), Some(100));
    }

    #[test]
    fn default_paragraph_style_applies_without_pstyle() {
        let ss = parse_styles_xml(
            r#"<w:styles>
            <w:style w:type="character" w:default="1" w:styleId="DefaultParagraphFont"><w:pPr><w:pageBreakBefore/></w:pPr></w:style>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:pPr><w:suppressLineNumbers/><w:pageBreakBefore/></w:pPr></w:style>
            <w:style w:type="paragraph" w:styleId="Other"/>
        </w:styles>"#,
        );
        let (_, props) = par(None, &[]);
        // Normal, the default paragraph style (not the default character
        // style), applies to a paragraph that names no style.
        assert!(ss.effective_ppr_flag(None, &props, PprFlag::PageBreakBefore));
        assert!(ss.effective_ppr_flag(None, &props, PprFlag::SuppressLineNumbers));
        // A named style replaces it rather than inheriting from it.
        assert!(!ss.effective_ppr_flag(Some("Other"), &props, PprFlag::PageBreakBefore));
        assert!(!ss.effective_ppr_flag(Some("Other"), &props, PprFlag::SuppressLineNumbers));
        // No default paragraph style: off.
        let none = parse_styles_xml(
            r#"<w:styles><w:style w:type="paragraph" w:styleId="Normal"><w:pPr><w:pageBreakBefore/></w:pPr></w:style></w:styles>"#,
        );
        assert!(!none.effective_ppr_flag(None, &props, PprFlag::PageBreakBefore));
    }

    /// #797: every resolver falls back to the default paragraph style when a
    /// paragraph names none; a named style replaces it, direct wins, and with
    /// no default style nothing changes.
    #[test]
    fn every_resolver_falls_back_to_the_default_paragraph_style_797() {
        let ss = parse_styles_xml(
            r#"<w:styles>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal">
              <w:pPr><w:jc w:val="center"/><w:bidi/><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs>
                <w:pBdr><w:top w:val="single" w:sz="4"/></w:pBdr></w:pPr>
              <w:rPr><w:b/><w:color w:val="FF0000"/></w:rPr>
            </w:style>
            <w:style w:type="paragraph" w:styleId="Other"/>
        </w:styles>"#,
        );
        let none = parse_styles_xml(
            r#"<w:styles><w:style w:type="paragraph" w:styleId="Normal"><w:pPr><w:jc w:val="center"/></w:pPr></w:style></w:styles>"#,
        );
        let direct = RunProps::default();
        let run = ss.effective_run(None, None, &direct);
        assert!(run.bold);
        assert_eq!(run.color.as_deref(), Some("FF0000"));
        assert!(!ss.effective_run(Some("Other"), None, &direct).bold);
        assert!(!none.effective_run(None, None, &direct).bold);
        let own = RunProps {
            color: Some("00FF00".into()),
            ..Default::default()
        };
        assert_eq!(
            ss.effective_run(None, None, &own).color.as_deref(),
            Some("00FF00")
        );

        let with = |align| ParProps {
            align,
            ..Default::default()
        };
        assert_eq!(ss.effective_align(None, &with(Align::Left)), Align::Center);
        assert_eq!(ss.effective_align(None, &with(Align::Right)), Align::Right);
        assert_eq!(
            ss.effective_align(Some("Other"), &with(Align::Left)),
            Align::Left
        );
        assert_eq!(none.effective_align(None, &with(Align::Left)), Align::Left);

        let props = ParProps::default();
        assert_eq!(ss.paragraph_rtl_override(None, &props), Some(true));
        assert_eq!(ss.paragraph_rtl_override(Some("Other"), &props), None);

        assert_eq!(ss.effective_tabs(None).len(), 1);
        assert!(ss.effective_tabs(Some("Other")).is_empty());
        assert!(none.effective_tabs(None).is_empty());

        assert!(
            ss.effective_borders(None, ParBorders::default())
                .top
                .is_some()
        );
        assert!(
            ss.effective_borders(Some("Other"), ParBorders::default())
                .top
                .is_none()
        );
    }

    /// #918: hidden text through a character style, a paragraph style (or
    /// the default one), a `basedOn` chain or the document defaults; a direct
    /// `w:vanish`, on or off, decides.
    #[test]
    fn effective_vanish_resolves_styles_and_direct_off_wins_918() {
        let ss = parse_styles_xml(
            r#"<w:styles>
            <w:style w:type="character" w:styleId="Secret"><w:rPr><w:vanish/></w:rPr></w:style>
            <w:style w:type="character" w:styleId="Shown"><w:basedOn w:val="Secret"/><w:rPr><w:vanish w:val="0"/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="HiddenPara"><w:rPr><w:vanish/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="Child"><w:basedOn w:val="HiddenPara"/></w:style>
            <w:style w:type="paragraph" w:styleId="MarkOnly"><w:pPr><w:rPr><w:vanish/></w:rPr></w:pPr></w:style>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal"/>
        </w:styles>"#,
        );
        let plain = RunProps::default();
        assert!(ss.effective_vanish(None, Some("Secret"), &plain));
        assert!(
            !ss.effective_vanish(None, Some("Shown"), &plain),
            "a derived style turns it off"
        );
        assert!(ss.effective_vanish(Some("HiddenPara"), None, &plain));
        assert!(
            ss.effective_vanish(Some("Child"), None, &plain),
            "through basedOn"
        );
        assert!(
            !ss.effective_vanish(Some("Child"), Some("Shown"), &plain),
            "the character style wins"
        );
        assert!(
            !ss.effective_vanish(Some("MarkOnly"), None, &plain),
            "the paragraph mark's only"
        );
        assert!(!ss.effective_vanish(None, None, &plain));
        let off = RunProps {
            raw_props: vec!["<w:vanish w:val=\"0\"/>".to_string()],
            ..RunProps::default()
        };
        assert!(
            !ss.effective_vanish(Some("HiddenPara"), Some("Secret"), &off),
            "direct off wins"
        );
        let on = RunProps {
            vanish: true,
            ..RunProps::default()
        };
        assert!(
            ss.effective_vanish(None, Some("Shown"), &on),
            "direct on wins"
        );
        // The default paragraph style applies without a pStyle; docDefaults too.
        let defaults = parse_styles_xml(
            r#"<w:styles><w:docDefaults><w:rPrDefault><w:rPr><w:vanish/></w:rPr></w:rPrDefault></w:docDefaults>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal"/>
            <w:style w:type="paragraph" w:styleId="Visible"><w:rPr><w:vanish w:val="false"/></w:rPr></w:style></w:styles>"#,
        );
        assert!(defaults.effective_vanish(None, None, &plain));
        assert!(!defaults.effective_vanish(Some("Visible"), None, &plain));
        let normal = parse_styles_xml(
            r#"<w:styles><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:rPr><w:vanish/></w:rPr></w:style></w:styles>"#,
        );
        assert!(normal.effective_vanish(None, None, &plain));
        // Rendering still reads direct vanish only.
        assert!(!ss.effective_run(None, Some("Secret"), &plain).vanish);
    }

    /// The owner's file (#1972): `docDefaults` naming only the theme's minor
    /// font, a default Normal with no run properties, a heading naming the
    /// major font.
    const THEMED_STYLES: &str = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        <w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:asciiTheme="minorHAnsi" w:eastAsiaTheme="minorHAnsi"
        w:hAnsiTheme="minorHAnsi" w:cstheme="minorBidi"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>
        <w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
        <w:style w:type="paragraph" w:styleId="Heading1"><w:basedOn w:val="Normal"/>
        <w:rPr><w:rFonts w:asciiTheme="majorHAnsi" w:hAnsiTheme="majorHAnsi"/></w:rPr></w:style>
        <w:style w:type="paragraph" w:styleId="Both"><w:rPr><w:rFonts w:ascii="Georgia" w:asciiTheme="majorHAnsi"/></w:rPr></w:style>
        <w:style w:type="paragraph" w:styleId="Explicit"><w:basedOn w:val="Heading1"/>
        <w:rPr><w:rFonts w:ascii="Georgia"/></w:rPr></w:style>
        <w:style w:type="paragraph" w:styleId="Unknown"><w:rPr><w:rFonts w:ascii="Georgia" w:asciiTheme="nothing"/></w:rPr></w:style>
        </w:styles>"#;

    fn office_theme() -> crate::theme::ThemeFonts {
        crate::theme::parse_theme_fonts(
            r#"<a:theme xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"><a:themeElements>
            <a:fontScheme name="Office"><a:majorFont><a:latin typeface="Calibri Light"/></a:majorFont>
            <a:minorFont><a:latin typeface="Calibri"/></a:minorFont></a:fontScheme></a:themeElements></a:theme>"#,
        )
    }

    #[test]
    fn theme_fonts_give_the_document_font_and_style_fonts_1972() {
        let mut ss = parse_styles_xml(THEMED_STYLES);
        assert_eq!(ss.doc_font(), None, "a theme reference with no theme");
        ss.apply_theme_fonts(&office_theme());
        // The owner's run: only `w:b`, in a paragraph with no pStyle.
        assert_eq!(ss.doc_font(), Some("Calibri"));
        let bold = RunProps {
            bold: true,
            ..RunProps::default()
        };
        let shown = ss.effective_run(None, None, &bold);
        assert!(shown.bold);
        assert_eq!(shown.font.as_deref(), Some("Calibri"));
        // A style's majorHAnsi is the major latin font.
        assert_eq!(
            ss.effective_run(Some("Heading1"), None, &RunProps::default())
                .font
                .as_deref(),
            Some("Calibri Light")
        );
        // A style's own theme reference beats its own w:ascii.
        assert_eq!(
            ss.effective_run(Some("Both"), None, &RunProps::default())
                .font
                .as_deref(),
            Some("Calibri Light")
        );
        // A derived style's w:ascii beats the theme reference it inherits.
        assert_eq!(
            ss.effective_run(Some("Explicit"), None, &RunProps::default())
                .font
                .as_deref(),
            Some("Georgia")
        );
        // A reference the theme does not name leaves w:ascii.
        assert_eq!(
            ss.effective_run(Some("Unknown"), None, &RunProps::default())
                .font
                .as_deref(),
            Some("Georgia")
        );
        // A run's own family still wins.
        let named = RunProps {
            font: Some("Arial".into()),
            ..RunProps::default()
        };
        assert_eq!(
            ss.effective_run(None, None, &named).font.as_deref(),
            Some("Arial")
        );
    }

    #[test]
    fn doc_font_is_normal_over_doc_defaults_1972() {
        let ss = parse_styles_xml(
            r#"<w:styles><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri"/></w:rPr>
            </w:rPrDefault></w:docDefaults>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:rPr><w:rFonts w:ascii="Cambria"/></w:rPr></w:style></w:styles>"#,
        );
        assert_eq!(ss.doc_font(), Some("Cambria"));
        let defaults_only = parse_styles_xml(
            r#"<w:styles><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri"/></w:rPr>
            </w:rPrDefault></w:docDefaults></w:styles>"#,
        );
        assert_eq!(defaults_only.doc_font(), Some("Calibri"));
        assert_eq!(parse_styles_xml("<w:styles/>").doc_font(), None);
        // Applying a theme to a sheet with no references changes nothing.
        let mut plain = defaults_only.clone();
        plain.apply_theme_fonts(&office_theme());
        assert_eq!(plain.doc_font(), Some("Calibri"));
    }

    /// A paragraph style's own chain over the defaults (#1972 r1): one that
    /// never reaches Normal takes the defaults' family, not Normal's.
    #[test]
    fn para_font_follows_the_paragraph_styles_own_chain_1972() {
        let mut ss = parse_styles_xml(
            r#"<w:styles><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:asciiTheme="minorHAnsi"/></w:rPr>
            </w:rPrDefault></w:docDefaults>
            <w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:rPr><w:rFonts w:ascii="Times New Roman"/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="Code"><w:name w:val="Code"/></w:style>
            <w:style w:type="paragraph" w:styleId="Quote"><w:basedOn w:val="Normal"/></w:style>
            <w:style w:type="paragraph" w:styleId="Heading1"><w:basedOn w:val="Normal"/>
            <w:rPr><w:rFonts w:asciiTheme="majorHAnsi"/></w:rPr></w:style></w:styles>"#,
        );
        ss.apply_theme_fonts(&office_theme());
        assert_eq!(
            ss.para_font(None),
            Some("Times New Roman"),
            "no pStyle: Normal's"
        );
        assert_eq!(ss.para_font(Some("Normal")), Some("Times New Roman"));
        assert_eq!(
            ss.para_font(Some("Code")),
            Some("Calibri"),
            "standalone: defaults'"
        );
        assert_eq!(
            ss.para_font(Some("Quote")),
            Some("Times New Roman"),
            "basedOn Normal"
        );
        assert_eq!(ss.para_font(Some("Heading1")), Some("Calibri Light"));
        assert_eq!(
            ss.para_font(Some("Missing")),
            Some("Calibri"),
            "as effective_run"
        );
        for id in ["Normal", "Code", "Quote", "Heading1", "Missing"] {
            assert_eq!(
                ss.para_font(Some(id)),
                ss.effective_run(Some(id), None, &RunProps::default())
                    .font
                    .as_deref(),
                "{id}"
            );
        }
    }

    #[test]
    fn shown_run_sees_resolved_theme_fonts_but_still_starts_from_nothing_1972() {
        let mut ss = parse_styles_xml(THEMED_STYLES);
        ss.apply_theme_fonts(&office_theme());
        let plain = RunProps::default();
        assert_eq!(ss.shown_run(None, None, None, &plain).font, None);
        assert_eq!(
            ss.shown_run(None, Some("Heading1"), None, &plain)
                .font
                .as_deref(),
            Some("Calibri Light")
        );
    }
}
