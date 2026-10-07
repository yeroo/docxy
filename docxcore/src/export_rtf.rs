//! Document → Rich Text Format (#635): what Save As Rich Text Format writes,
//! read back by [`crate::import::rtf::import_rtf`].
//!
//! Written: paragraphs, bold / italic / underline / strikethrough /
//! superscript / subscript / caps / small caps / hidden, fonts, sizes and
//! colours, paragraph alignment and direction, headings (`heading N` styles
//! in the stylesheet), lists (a list table Word numbers from, plus the
//! marker in `\listtext` that plain readers show), tables (`\trowd … \cell
//! … \row`, vertical merges), hyperlinks as `HYPERLINK` fields, tabs and
//! line / page / column breaks. Every character past ASCII is `\uN?`, so the
//! file is 7-bit and needs no code page.
//!
//! Like the plain-text writer, tracked changes are their final view. Not
//! written: pictures, objects, headers and footers, notes (a reference is
//! its number, superscript), comments and content the model keeps only as
//! raw XML. A table inside a table cell is written as that cell's
//! paragraphs.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::model::{
    Align, Block, BreakKind, Cell, Document, Inline, Paragraph, RevisionKind, Row, RunProps, Table,
    VMerge, VertAlign,
};

/// Twips per table column when the table has no grid for it.
const DEFAULT_COLUMN: u32 = 2160;

/// The RTF for `doc`. `markers` are its list paragraphs' markers by tree path
/// ([`crate::numbering::compute_markers`]).
pub fn to_rtf(doc: &Document, markers: &HashMap<Vec<usize>, String>) -> String {
    let mut w = Writer {
        markers,
        body: String::new(),
        fonts: vec!["Calibri".to_string()],
        colors: Vec::new(),
        lists: Vec::new(),
    };
    let mut path = Vec::new();
    w.blocks(&doc.body, &mut path, false);
    w.finish()
}

/// A list definition written to the list table: one per `numId` used, its
/// levels numbered or bulleted as the first marker seen at that level says.
struct ListDef {
    num_id: i32,
    levels: [Option<LevelKind>; 9],
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LevelKind {
    Bullet,
    /// `\levelnfcN` and the character after the number (`.`, `)`).
    Number(u8, char),
}

struct Writer<'a> {
    markers: &'a HashMap<Vec<usize>, String>,
    body: String,
    fonts: Vec<String>,
    /// `RRGGBB`, `\cfN` being the index plus one (0 is "auto").
    colors: Vec<String>,
    lists: Vec<ListDef>,
}

impl Writer<'_> {
    fn finish(self) -> String {
        let mut out = String::from("{\\rtf1\\ansi\\ansicpg1252\\uc1\\deff0\n{\\fonttbl");
        for (i, f) in self.fonts.iter().enumerate() {
            let _ = write!(out, "{{\\f{i}\\fnil\\fcharset0 ");
            escape(f, &mut out);
            out.push_str(";}");
        }
        out.push_str("}\n");
        if !self.colors.is_empty() {
            out.push_str("{\\colortbl;");
            for c in &self.colors {
                let v = |i: usize| u8::from_str_radix(&c[i..i + 2], 16).unwrap_or(0);
                let _ = write!(out, "\\red{}\\green{}\\blue{};", v(0), v(2), v(4));
            }
            out.push_str("}\n");
        }
        out.push_str("{\\stylesheet{\\s0 Normal;}");
        for level in 1..=9 {
            let _ = write!(
                out,
                "{{\\s{level}\\outlinelevel{} heading {level};}}",
                level - 1
            );
        }
        out.push_str("}\n");
        if !self.lists.is_empty() {
            out.push_str("{\\*\\listtable");
            for (i, list) in self.lists.iter().enumerate() {
                let id = i + 1;
                let _ = write!(out, "{{\\list\\listtemplateid{id}");
                for (l, kind) in list.levels.iter().enumerate() {
                    list_level(l, kind.unwrap_or(LevelKind::Bullet), &mut out);
                }
                let _ = write!(out, "{{\\listname ;}}\\listid{id}}}");
            }
            out.push_str("}\n{\\*\\listoverridetable");
            for i in 1..=self.lists.len() {
                let _ = write!(
                    out,
                    "{{\\listoverride\\listid{i}\\listoverridecount0\\ls{i}}}"
                );
            }
            out.push_str("}\n");
        }
        out.push_str(&self.body);
        out.push('}');
        out
    }

    fn blocks(&mut self, blocks: &[Block], path: &mut Vec<usize>, in_table: bool) {
        for (i, block) in blocks.iter().enumerate() {
            path.push(i);
            match block {
                Block::Paragraph(p) => self.paragraph(p, path, in_table, "\\par"),
                Block::Table(t) if in_table => self.nested_table(t, path),
                Block::Table(t) => self.table(t, path),
                Block::Raw(_) | Block::SectionProperties(_) => {}
            }
            path.pop();
        }
    }

    fn paragraph(&mut self, p: &Paragraph, path: &[usize], in_table: bool, end: &str) {
        self.body.push_str("\\pard\\plain");
        if let Some(level) = p.props.heading_level.filter(|l| (1..=9).contains(l)) {
            let _ = write!(self.body, "\\s{level}\\outlinelevel{}", level - 1);
        }
        self.body.push_str(match p.props.align {
            Align::Left => "\\ql",
            Align::Center => "\\qc",
            Align::Right => "\\qr",
            Align::Justify => "\\qj",
        });
        if p.props.rtl {
            self.body.push_str("\\rtlpar");
        }
        if in_table {
            self.body.push_str("\\intbl");
        }
        let marker = self.markers.get(path).cloned();
        let listed = p.props.num_id.filter(|_| p.props.heading_level.is_none());
        if let Some(num_id) = listed {
            let ilvl = p.props.ilvl.clamp(0, 8) as usize;
            let ls = self.list(num_id, ilvl, marker.as_deref());
            let _ = write!(
                self.body,
                "\\ls{ls}\\ilvl{ilvl}\\fi-360\\li{}",
                720 * (ilvl + 1)
            );
        }
        // The control words' delimiter: a space after a group is text.
        self.body.push(' ');
        if let Some(m) = marker.filter(|_| listed.is_some()) {
            self.body.push_str("{\\listtext ");
            escape(&m, &mut self.body);
            self.body.push_str("\\tab}");
        }
        self.inlines(&p.content);
        self.body.push_str(end);
        self.body.push('\n');
    }

    /// The `\lsN` for `num_id`, recording what level `ilvl` is.
    fn list(&mut self, num_id: i32, ilvl: usize, marker: Option<&str>) -> usize {
        let i = match self.lists.iter().position(|l| l.num_id == num_id) {
            Some(i) => i,
            None => {
                self.lists.push(ListDef {
                    num_id,
                    levels: [None; 9],
                });
                self.lists.len() - 1
            }
        };
        let level = &mut self.lists[i].levels[ilvl];
        if level.is_none() {
            *level = Some(marker.map_or(LevelKind::Bullet, level_kind));
        }
        i + 1
    }

    fn table(&mut self, t: &Table, path: &mut Vec<usize>) {
        for (ri, row) in t.rows.iter().enumerate() {
            let def = row_definition(t, row);
            self.body.push_str(&def);
            for (ci, cell) in row.cells.iter().enumerate() {
                path.push(ri);
                path.push(ci);
                self.cell(cell, path);
                path.pop();
                path.pop();
            }
            // Word repeats the row's definition before `\row`.
            let _ = writeln!(self.body, "\\pard\\plain\\intbl{def}\\row");
        }
        self.body.push_str("\\pard\\plain\n");
    }

    /// A cell's paragraphs, the last ended by `\cell` rather than `\par`.
    fn cell(&mut self, cell: &Cell, path: &mut Vec<usize>) {
        let paras: Vec<(usize, &Block)> = cell
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b, Block::Paragraph(_) | Block::Table(_)))
            .collect();
        if cell.v_merge == VMerge::Continue || paras.is_empty() {
            self.body.push_str("\\pard\\plain\\intbl\\cell\n");
            return;
        }
        let last = paras.len() - 1;
        for (n, (i, block)) in paras.into_iter().enumerate() {
            path.push(i);
            match block {
                Block::Paragraph(p) => {
                    self.paragraph(p, path, true, if n == last { "\\cell" } else { "\\par" })
                }
                Block::Table(t) => {
                    self.nested_table(t, path);
                    if n == last {
                        self.body.push_str("\\pard\\plain\\intbl\\cell\n");
                    }
                }
                _ => {}
            }
            path.pop();
        }
    }

    /// A table inside a cell: its paragraphs, in order, as the cell's own.
    fn nested_table(&mut self, t: &Table, path: &mut Vec<usize>) {
        for (ri, row) in t.rows.iter().enumerate() {
            for (ci, cell) in row.cells.iter().enumerate() {
                if cell.v_merge == VMerge::Continue {
                    continue;
                }
                path.push(ri);
                path.push(ci);
                self.blocks(&cell.blocks, path, true);
                path.pop();
                path.pop();
            }
        }
    }

    fn inlines(&mut self, content: &[Inline]) {
        for inline in content {
            match inline {
                Inline::Run(r) => self.run(&r.text, &r.props),
                Inline::Hyperlink(h) => {
                    let mut inst = String::from("HYPERLINK ");
                    match (&h.target, &h.anchor) {
                        (Some(t), _) => {
                            let _ = write!(inst, "\"{}\"", t.replace('"', "%22"));
                        }
                        (None, Some(a)) => {
                            let _ = write!(inst, "\\l \"{}\"", a.replace('"', ""));
                        }
                        (None, None) => inst.clear(),
                    }
                    if inst.is_empty() {
                        for r in &h.runs {
                            self.run(&r.text, &r.props);
                        }
                        self.inlines(&h.content);
                        continue;
                    }
                    self.body.push_str("{\\field{\\*\\fldinst {");
                    escape(&inst, &mut self.body);
                    self.body.push_str("}}{\\fldrslt {");
                    for r in &h.runs {
                        self.run(&r.text, &r.props);
                    }
                    self.inlines(&h.content);
                    self.body.push_str("}}}");
                }
                Inline::Break(kind, props) => {
                    let word = match kind {
                        BreakKind::Line | BreakKind::Clear(_) => "\\line",
                        BreakKind::Page => "\\page",
                        BreakKind::Column => "\\column",
                    };
                    self.control_run(word, props);
                }
                Inline::Tab(props) => self.control_run("\\tab", props),
                Inline::SmartArt { text, .. } => self.run(&text.join("\n"), &RunProps::default()),
                Inline::Chart { chart, .. } => {
                    self.run(chart.title.as_deref().unwrap_or(""), &RunProps::default())
                }
                Inline::Equation { text, .. } => self.run(text, &RunProps::default()),
                Inline::Field { raw, text } => {
                    self.run(text, &crate::load::field_result_props(raw))
                }
                Inline::TextBox { blocks, .. } => {
                    let mut first = true;
                    for p in text_box_paragraphs(blocks) {
                        if !std::mem::take(&mut first) {
                            self.body.push_str("\\line ");
                        }
                        self.inlines(&p.content);
                    }
                }
                Inline::Revision {
                    kind: RevisionKind::Insert,
                    content,
                    ..
                } => self.inlines(content),
                Inline::Revision {
                    kind: RevisionKind::Delete,
                    ..
                } => {}
                Inline::UnsupportedRevision { .. } => {}
                Inline::FootnoteRef { id, .. } => {
                    let _ = write!(self.body, "{{\\super {id}}}");
                }
                Inline::Raw(_) => {}
            }
        }
    }

    fn run(&mut self, text: &str, props: &RunProps) {
        if text.is_empty() {
            return;
        }
        self.body.push('{');
        let start = self.body.len();
        self.run_props(props);
        if self.body.len() > start {
            self.body.push(' ');
        }
        escape(text, &mut self.body);
        self.body.push('}');
    }

    /// A tab or break, in its run's formatting.
    fn control_run(&mut self, word: &str, props: &RunProps) {
        self.body.push('{');
        self.run_props(props);
        self.body.push_str(word);
        self.body.push('}');
    }

    fn run_props(&mut self, p: &RunProps) {
        let flags = [
            (p.bold, "\\b"),
            (p.italic, "\\i"),
            (p.underline, "\\ul"),
            (p.strike, "\\strike"),
            (p.caps, "\\caps"),
            (p.small_caps, "\\scaps"),
            (p.vanish, "\\v"),
            (p.vert_align == VertAlign::Superscript, "\\super"),
            (p.vert_align == VertAlign::Subscript, "\\sub"),
        ];
        for (on, word) in flags {
            if on {
                self.body.push_str(word);
            }
        }
        let font = p
            .font
            .clone()
            .or_else(|| p.code.then(|| "Courier New".to_string()));
        if let Some(font) = font.filter(|f| !f.is_empty()) {
            let i = match self.fonts.iter().position(|f| *f == font) {
                Some(i) => i,
                None => {
                    self.fonts.push(font);
                    self.fonts.len() - 1
                }
            };
            let _ = write!(self.body, "\\f{i}");
        }
        if let Some(size) = p.size_half_pts {
            let _ = write!(self.body, "\\fs{size}");
        }
        if let Some(color) = p
            .color
            .as_deref()
            .filter(|c| c.len() == 6 && c.chars().all(|ch| ch.is_ascii_hexdigit()))
        {
            let color = color.to_ascii_uppercase();
            let i = match self.colors.iter().position(|c| *c == color) {
                Some(i) => i,
                None => {
                    self.colors.push(color);
                    self.colors.len() - 1
                }
            };
            let _ = write!(self.body, "\\cf{}", i + 1);
        }
    }
}

/// `\trowd` and the cell definitions for `row`: each cell's right edge from
/// the table grid (the columns it spans), its vertical merge.
fn row_definition(t: &Table, row: &Row) -> String {
    let mut def = String::from("\\trowd\\trgaph108\\trleft0");
    let mut col = 0usize;
    let mut right = 0u32;
    for cell in &row.cells {
        let span = cell.grid_span.max(1) as usize;
        for c in col..col + span {
            right += t
                .grid
                .get(c)
                .copied()
                .filter(|&w| w > 0)
                .unwrap_or(DEFAULT_COLUMN);
        }
        col += span;
        match cell.v_merge {
            VMerge::Restart => def.push_str("\\clvmgf"),
            VMerge::Continue => def.push_str("\\clvmrg"),
            VMerge::None => {}
        }
        let _ = write!(def, "\\cellx{right}");
    }
    def
}

/// A list level's `\listlevel` group: Word's default indents, a tab after
/// the number, and a decimal / letter / roman number or a bullet.
fn list_level(level: usize, kind: LevelKind, out: &mut String) {
    let indent = 720 * (level + 1);
    match kind {
        LevelKind::Bullet => {
            let _ = write!(
                out,
                "{{\\listlevel\\levelnfc23\\levelnfcn23\\leveljc0\\levelstartat1\\levelfollow0\
                 {{\\leveltext\\'01\\u8226 ?;}}{{\\levelnumbers;}}\\fi-360\\li{indent}}}"
            );
        }
        LevelKind::Number(nfc, suffix) => {
            let _ = write!(
                out,
                "{{\\listlevel\\levelnfc{nfc}\\levelnfcn{nfc}\\leveljc0\\levelstartat1\\levelfollow0\
                 {{\\leveltext\\'02\\'{level:02x}"
            );
            escape(&suffix.to_string(), out);
            let _ = write!(out, ";}}{{\\levelnumbers\\'01;}}\\fi-360\\li{indent}}}");
        }
    }
}

/// What a marker says its level is: a number (`1.`, `a)`, `iv.`) and its
/// format, or a bullet.
fn level_kind(marker: &str) -> LevelKind {
    let m = marker.trim();
    if !crate::import::marker_is_numbered(m) {
        return LevelKind::Bullet;
    }
    let suffix = m.chars().last().unwrap_or('.');
    let body: String = m
        .trim_end_matches(['.', ')'])
        .trim_start_matches('(')
        .to_string();
    let roman = |s: &str, set: &str| s.len() > 1 && s.chars().all(|c| set.contains(c));
    let nfc = if body.chars().all(|c| c.is_ascii_digit()) {
        0
    } else if roman(&body, "ivxlcdm") {
        2
    } else if roman(&body, "IVXLCDM") {
        1
    } else if body.chars().all(|c| c.is_lowercase()) {
        4
    } else if body.chars().all(|c| c.is_uppercase()) {
        3
    } else {
        0
    };
    LevelKind::Number(nfc, suffix)
}

/// A text box's paragraphs, its tables' included, in order.
fn text_box_paragraphs(blocks: &[Block]) -> Vec<&Paragraph> {
    let mut out = Vec::new();
    for b in blocks {
        match b {
            Block::Paragraph(p) => out.push(p),
            Block::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        out.extend(text_box_paragraphs(&cell.blocks));
                    }
                }
            }
            Block::Raw(_) | Block::SectionProperties(_) => {}
        }
    }
    out
}

/// `s` as RTF text: `\`, `{` and `}` escaped, tab and line feed as control
/// words, other control characters dropped, and every character past ASCII
/// as `\uN?` (N the signed 16-bit UTF-16 unit; an astral character is its
/// two surrogate halves), `?` being the fallback a reader without Unicode
/// shows (`\uc1`).
fn escape(s: &str, out: &mut String) {
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '{' => out.push_str("\\{"),
            '}' => out.push_str("\\}"),
            '\t' => out.push_str("\\tab "),
            '\n' => out.push_str("\\line "),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {}
            c if c.is_ascii() => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{}?", *u as i16);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::rtf::import_rtf;
    use crate::model::{Hyperlink, ParProps, Run};

    fn run(s: &str, props: RunProps) -> Inline {
        Inline::Run(Run {
            text: s.into(),
            props,
        })
    }

    fn plain(s: &str) -> Inline {
        run(s, RunProps::default())
    }

    fn para(props: ParProps, content: Vec<Inline>) -> Block {
        Block::Paragraph(Paragraph { props, content })
    }

    fn rtf(doc: &Document) -> String {
        to_rtf(doc, &crate::numbering::package_markers(None, doc))
    }

    fn back(doc: &Document) -> Document {
        let s = rtf(doc);
        import_rtf(s.as_bytes()).unwrap_or_else(|e| panic!("{e}: {s}"))
    }

    fn paras(doc: &Document) -> Vec<&Paragraph> {
        doc.body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    /// The runs' text and formatting of a paragraph, adjacent equal runs
    /// joined.
    fn runs(p: &Paragraph) -> Vec<(String, RunProps)> {
        let mut out: Vec<(String, RunProps)> = Vec::new();
        for i in &p.content {
            if let Inline::Run(r) = i {
                match out.last_mut() {
                    Some((t, props)) if *props == r.props => t.push_str(&r.text),
                    _ => out.push((r.text.clone(), r.props.clone())),
                }
            }
        }
        out
    }

    #[test]
    fn it_is_seven_bit_rtf_with_balanced_groups() {
        let doc = crate::markdown::from_markdown("# Title\n\nCaf\u{e9} {braces} \\ back\n");
        let s = rtf(&doc);
        assert!(
            s.starts_with("{\\rtf1\\ansi\\ansicpg1252\\uc1\\deff0"),
            "{s}"
        );
        assert!(s.is_ascii(), "{s}");
        let mut depth = 0i32;
        let mut esc = false;
        for c in s.chars() {
            match c {
                _ if esc => esc = false,
                '\\' => esc = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    assert!(depth >= 0, "{s}");
                }
                _ => {}
            }
        }
        assert_eq!(depth, 0, "{s}");
        assert!(s.ends_with('}'));
    }

    #[test]
    fn character_formatting_round_trips() {
        let b = RunProps {
            bold: true,
            ..RunProps::default()
        };
        let i = RunProps {
            italic: true,
            ..RunProps::default()
        };
        let u = RunProps {
            underline: true,
            ..RunProps::default()
        };
        let s = RunProps {
            strike: true,
            ..RunProps::default()
        };
        let sup = RunProps {
            vert_align: VertAlign::Superscript,
            ..RunProps::default()
        };
        let sub = RunProps {
            vert_align: VertAlign::Subscript,
            ..RunProps::default()
        };
        let all = RunProps {
            bold: true,
            italic: true,
            underline: true,
            ..RunProps::default()
        };
        let doc = Document {
            body: vec![para(
                ParProps::default(),
                vec![
                    plain("n "),
                    run("b", b.clone()),
                    run("i", i.clone()),
                    run("u", u.clone()),
                    run("s", s.clone()),
                    run("2", sup.clone()),
                    run("x", sub.clone()),
                    run("all", all.clone()),
                ],
            )],
        };
        let got = back(&doc);
        assert_eq!(
            runs(paras(&got)[0]),
            vec![
                ("n ".into(), RunProps::default()),
                ("b".into(), b),
                ("i".into(), i),
                ("u".into(), u),
                ("s".into(), s),
                ("2".into(), sup),
                ("x".into(), sub),
                ("all".into(), all),
            ]
        );
    }

    #[test]
    fn bold_does_not_leak_into_the_next_paragraph() {
        let doc = Document {
            body: vec![
                para(
                    ParProps {
                        align: Align::Center,
                        ..ParProps::default()
                    },
                    vec![run(
                        "bold",
                        RunProps {
                            bold: true,
                            ..RunProps::default()
                        },
                    )],
                ),
                para(ParProps::default(), vec![plain("plain")]),
            ],
        };
        let got = back(&doc);
        let p = paras(&got);
        assert_eq!(runs(p[1]), vec![("plain".into(), RunProps::default())]);
        assert_eq!(p[0].props.align, Align::Center);
        assert_eq!(p[1].props.align, Align::Left);
    }

    #[test]
    fn alignment_headings_and_breaks_round_trip() {
        let mut doc = crate::markdown::from_markdown("# One\n\n## Two\n\nBody");
        if let Block::Paragraph(p) = &mut doc.body[2] {
            p.props.align = Align::Right;
            p.content.push(Inline::Tab(RunProps::default()));
            p.content.push(plain("t"));
            p.content
                .push(Inline::Break(BreakKind::Line, RunProps::default()));
            p.content.push(plain("l"));
            p.content
                .push(Inline::Break(BreakKind::Page, RunProps::default()));
            p.content.push(plain("p"));
        }
        doc.body.push(para(
            ParProps {
                align: Align::Justify,
                ..ParProps::default()
            },
            vec![plain("j")],
        ));
        let got = back(&doc);
        let p = paras(&got);
        assert_eq!(p[0].props.heading_level, Some(1));
        assert_eq!(p[1].props.heading_level, Some(2));
        assert_eq!(p[2].props.heading_level, None);
        assert_eq!(p[2].props.align, Align::Right);
        assert_eq!(p[3].props.align, Align::Justify);
        assert_eq!(p[2].plain_text(), "Body\tt\nl\np");
        assert!(
            p[2].content
                .iter()
                .any(|i| matches!(i, Inline::Break(BreakKind::Page, _))),
            "{:?}",
            p[2].content
        );
    }

    #[test]
    fn lists_round_trip_numbered_or_bulleted() {
        let doc = crate::markdown::from_markdown("1. one\n2. two\n\n- dot\n  - deeper\n");
        let s = rtf(&doc);
        assert!(s.contains("{\\*\\listtable"), "{s}");
        assert!(s.contains("{\\listtext 1.\\tab}"), "{s}");
        let got = back(&doc);
        let p = paras(&got);
        let lists: Vec<(Option<i32>, i32, String)> = p
            .iter()
            .map(|p| (p.props.num_id, p.props.ilvl, p.plain_text()))
            .collect();
        assert_eq!(
            lists,
            vec![
                (Some(2), 0, "one".to_string()),
                (Some(2), 0, "two".to_string()),
                (Some(1), 0, "dot".to_string()),
                (Some(1), 1, "deeper".to_string()),
            ]
        );
    }

    #[test]
    fn tables_round_trip() {
        let doc =
            crate::markdown::from_markdown("Before\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\nAfter\n");
        let got = back(&doc);
        let shape: Vec<String> = got
            .body
            .iter()
            .map(|b| match b {
                Block::Paragraph(p) => p.plain_text(),
                Block::Table(t) => t
                    .rows
                    .iter()
                    .map(|r| {
                        r.cells
                            .iter()
                            .map(|c| c.blocks.iter().map(Block::plain_text).collect::<String>())
                            .collect::<Vec<_>>()
                            .join("|")
                    })
                    .collect::<Vec<_>>()
                    .join("/"),
                _ => String::new(),
            })
            .collect();
        assert_eq!(shape, ["Before", "A|B/1|2", "After"]);
    }

    #[test]
    fn hyperlinks_keep_their_text_and_write_their_target() {
        let doc = Document {
            body: vec![para(
                ParProps::default(),
                vec![
                    plain("see "),
                    Inline::Hyperlink(Hyperlink {
                        target: Some("https://example.com/a\\b".into()),
                        runs: vec![Run {
                            text: "site".into(),
                            props: RunProps::default(),
                        }],
                        ..Hyperlink::default()
                    }),
                ],
            )],
        };
        let s = rtf(&doc);
        assert!(
            s.contains("HYPERLINK \"https://example.com/a\\\\b\""),
            "{s}"
        );
        assert_eq!(paras(&back(&doc))[0].plain_text(), "see site");
    }

    #[test]
    fn unicode_and_escapes_round_trip() {
        let text = "a\\b{c}d \u{e9}\u{4e2d}\u{ffff}\u{1f600} end";
        let doc = Document {
            body: vec![para(ParProps::default(), vec![plain(text)])],
        };
        let s = rtf(&doc);
        assert!(s.contains("a\\\\b\\{c\\}d"), "{s}");
        assert!(s.contains("\\u233?"), "{s}");
        assert!(s.contains("\\u-1?"), "{s}");
        // U+1F600 is D83D DE00.
        assert!(s.contains("\\u-10179?\\u-8704?"), "{s}");
        assert_eq!(paras(&back(&doc))[0].plain_text(), text);
    }

    #[test]
    fn tracked_changes_are_written_as_their_final_view() {
        let rev = |kind, s: &str| Inline::Revision {
            kind,
            metadata: Default::default(),
            raw: String::new(),
            content: vec![plain(s)],
            content_changed: false,
        };
        let doc = Document {
            body: vec![para(
                ParProps::default(),
                vec![
                    plain("keep "),
                    rev(RevisionKind::Insert, "added "),
                    rev(RevisionKind::Delete, "removed "),
                    plain("end"),
                ],
            )],
        };
        assert_eq!(paras(&back(&doc))[0].plain_text(), "keep added end");
    }

    #[test]
    fn fonts_sizes_and_colours_are_written() {
        let props = RunProps {
            font: Some("Georgia".into()),
            size_half_pts: Some(28),
            color: Some("FF0000".into()),
            ..RunProps::default()
        };
        let doc = Document {
            body: vec![para(ParProps::default(), vec![run("x", props)])],
        };
        let s = rtf(&doc);
        assert!(s.contains("{\\f1\\fnil\\fcharset0 Georgia;}"), "{s}");
        assert!(s.contains("{\\colortbl;\\red255\\green0\\blue0;}"), "{s}");
        assert!(s.contains("{\\f1\\fs28\\cf1 x}"), "{s}");
    }

    #[test]
    fn an_empty_document_is_a_valid_header() {
        let s = rtf(&Document::default());
        assert!(s.starts_with("{\\rtf1"), "{s}");
        assert!(s.ends_with('}'));
        // It holds no text, which the importer says rather than inventing a
        // paragraph.
        assert!(import_rtf(s.as_bytes()).is_err());
    }

    #[test]
    fn markers_say_their_level_kind() {
        assert_eq!(level_kind("1."), LevelKind::Number(0, '.'));
        assert_eq!(level_kind("a)"), LevelKind::Number(4, ')'));
        assert_eq!(level_kind("B."), LevelKind::Number(3, '.'));
        assert_eq!(level_kind("iv."), LevelKind::Number(2, '.'));
        assert_eq!(level_kind("\u{2022}"), LevelKind::Bullet);
    }
}
