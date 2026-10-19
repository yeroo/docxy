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
    Align, BorderKind, ParBorders, ParProps, RunProps, TabAlign, TabLeader, TabStop,
};
use crate::table_styles::RunLook;
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
        if let Some(s) = para_style {
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

    /// Effective alignment: direct wins; otherwise inherit from the style chain.
    pub fn effective_align(&self, para_style: Option<&str>, direct: Align) -> Align {
        if direct != Align::Left {
            return direct;
        }
        if let Some(s) = para_style {
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
        if let Some(s) = para_style {
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

    fn fold_rtl(&self, id: &str, seen: &mut HashSet<String>) -> Option<bool> {
        if !seen.insert(id.to_string()) {
            return None;
        }
        let def = self.styles.get(id)?;
        let base = def.based_on.as_ref().and_then(|b| self.fold_rtl(b, seen));
        def.rtl.or(base)
    }

    /// Tab stops for a paragraph style (the most-derived style that defines any,
    /// following `basedOn`). Empty if none.
    pub fn effective_tabs(&self, para_style: Option<&str>) -> Vec<TabStop> {
        let mut seen = HashSet::new();
        para_style
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
            .map(|s| self.fold_borders(s, &mut HashSet::new()))
            .unwrap_or_default();
        ParBorders {
            top: direct.top.or(style_b.top),
            bottom: direct.bottom.or(style_b.bottom),
        }
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
        ParBorders {
            top: def.borders.top.or(base.top),
            bottom: def.borders.bottom.or(base.bottom),
        }
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
                "w:docDefaults" => parse_doc_defaults(&mut p, &mut ss.default_run),
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

fn parse_doc_defaults(p: &mut XmlParser, run: &mut PartialRun) {
    loop {
        match p.next() {
            Event::Start => match p.name() {
                "w:rPrDefault" => parse_rpr_default(p, run),
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
                _ => p.skip_element(),
            },
            Event::End | Event::Eof => break,
            Event::Text => {}
        }
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

/// Parse a `<w:pBdr>` element's top/bottom sides into [`ParBorders`].
pub fn parse_pbdr(p: &mut XmlParser) -> ParBorders {
    let mut b = ParBorders::default();
    loop {
        match p.next() {
            Event::Start => {
                let side = p.name();
                let kind = BorderKind::from_val(p.attr("w:val"));
                match side {
                    "w:top" => b.top = kind,
                    "w:bottom" => b.bottom = kind,
                    _ => {}
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
        assert_eq!(
            ss.effective_align(Some("Centered"), Align::Left),
            Align::Center
        );
        assert_eq!(
            ss.effective_align(Some("Centered"), Align::Right),
            Align::Right
        ); // direct wins
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
        assert_eq!(b.bottom, Some(BorderKind::Single));
        assert_eq!(b.top, None);
        // a direct border wins over the style's
        let direct = ParBorders {
            bottom: Some(BorderKind::Double),
            top: None,
        };
        assert_eq!(
            ss.effective_borders(Some("Rule"), direct).bottom,
            Some(BorderKind::Double)
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
}
