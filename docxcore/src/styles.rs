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

use crate::model::{Align, RunProps};
use crate::xml::{Event, XmlParser};

#[derive(Debug, Clone, Default)]
struct PartialRun {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    strike: Option<bool>,
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
}

/// A parsed `styles.xml`: document defaults plus named style definitions.
#[derive(Debug, Clone, Default)]
pub struct StyleSheet {
    default_run: PartialRun,
    styles: HashMap<String, StyleDef>,
}

impl StyleSheet {
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
            caps: direct.caps,
            small_caps: direct.small_caps,
            vanish: direct.vanish,
            vert_align: direct.vert_align,
            color: direct.color.clone().or_else(|| agg.color.clone()),
            highlight: direct.highlight.clone(),
            size_half_pts: direct.size_half_pts.or(agg.size),
            font: direct.font.clone().or_else(|| agg.font.clone()),
            style_id: direct.style_id.clone(),
        }
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

    fn fold(&self, agg: &mut PartialRun, id: &str, seen: &mut HashSet<String>) {
        if !seen.insert(id.to_string()) {
            return;
        }
        if let Some(def) = self.styles.get(id) {
            if let Some(b) = &def.based_on {
                self.fold(agg, b, seen);
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
}

fn toggle(val: &str) -> bool {
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
                    let def = parse_style(&mut p);
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
    loop {
        match p.next() {
            Event::Start => {
                let name = p.name();
                let val = p.attr("w:val");
                match name {
                    "w:b" | "w:bCs" => run.bold = Some(toggle(val)),
                    "w:i" | "w:iCs" => run.italic = Some(toggle(val)),
                    "w:u" => run.underline = Some(toggle(val)),
                    "w:strike" | "w:dstrike" => run.strike = Some(toggle(val)),
                    "w:color" => {
                        if !val.is_empty() && val != "auto" {
                            run.color = Some(val.to_ascii_uppercase());
                        }
                    }
                    "w:sz" | "w:szCs" => {
                        let v = parse_uint(val);
                        if v > 0 {
                            run.size = Some(v);
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
}

fn parse_style_ppr(p: &mut XmlParser, def: &mut StyleDef) {
    loop {
        match p.next() {
            Event::Start => {
                if p.name() == "w:jc" {
                    def.align = map_align(p.attr("w:val"));
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
    fn paragraph_style_supplies_bold_and_size() {
        let xml = r#"<w:styles><w:style w:type="paragraph" w:styleId="Heading1">
            <w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style></w:styles>"#;
        let ss = parse_styles_xml(xml);
        let eff = ss.effective_run(Some("Heading1"), None, &RunProps::default());
        assert!(eff.bold);
        assert_eq!(eff.size_half_pts, Some(32));
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
    fn unknown_style_is_noop() {
        let ss = parse_styles_xml("<w:styles/>");
        let d = RunProps {
            bold: true,
            ..RunProps::default()
        };
        assert_eq!(ss.effective_run(Some("Nope"), None, &d), d);
    }
}
