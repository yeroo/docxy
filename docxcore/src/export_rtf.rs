//! Document → Rich Text Format (#635): what Save As Rich Text Format writes,
//! read back by [`crate::import::rtf::import_rtf`].
//!
//! Written: paragraphs, bold / italic / underline / strikethrough /
//! superscript / subscript / caps / small caps / hidden, fonts, sizes and
//! colours, paragraph alignment and direction, headings (`heading N` styles
//! in the stylesheet, with their style's character formatting), lists (a
//! list table Word numbers from, each level's format, start and number text
//! taken from the document's numbering, plus the marker in `\listtext` that
//! plain readers show), tables (`\trowd … \cell … \row`, the grid's column
//! edges, columns skipped before a row, vertical merges), hyperlinks as
//! `HYPERLINK` fields, tabs and line / page / column breaks. Formatting a
//! paragraph or run takes from its styles is resolved and written as direct
//! formatting, as the PDF exporter resolves it. Every character past ASCII is
//! `\uN?`, so the file is 7-bit and needs no code page.
//!
//! Like the plain-text writer, tracked changes are their final view. Not
//! written: pictures, objects, headers and footers, notes (a reference is
//! its number, superscript), comments and content the model keeps only as
//! raw XML. A table inside a table cell is written as that cell's
//! paragraphs.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::export_context::{ExportContext, final_view};
use crate::model::{
    Align, Block, BreakKind, Cell, Document, Inline, Paragraph, RevisionKind, RunProps, Table,
    VMerge, VertAlign,
};
use crate::numbering::{LevelDef, NumFmt, Numbering};
use crate::styles::StyleSheet;
use crate::table::{GridMap, RowMap};

/// Twips per table column when the table has no grid for it.
const DEFAULT_COLUMN: u32 = 2160;

/// The RTF for `doc`, its lists and styles as `ctx` defines them.
pub fn to_rtf(doc: &Document, ctx: &ExportContext) -> String {
    let doc = &final_view(doc);
    let markers = ctx.markers(doc);
    let mut w = Writer {
        markers: &markers,
        numbering: &ctx.numbering,
        styles: &ctx.styles,
        body: String::new(),
        fonts: vec!["Calibri".to_string()],
        colors: Vec::new(),
        lists: Vec::new(),
        heading_styles: Default::default(),
        para_base: RunProps::default(),
    };
    let mut path = Vec::new();
    w.blocks(&doc.body, &mut path, false);
    w.finish()
}

/// A list definition written to the list table: one per `numId` used.
struct ListDef {
    num_id: i32,
    levels: [Option<LevelKind>; 9],
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum LevelKind {
    Bullet,
    Number {
        /// `\levelnfcN`.
        nfc: u8,
        /// `\levelstartatN`.
        start: i32,
        /// The number text, `%1`…`%9` standing for the levels' numbers
        /// (`w:lvlText`).
        text: String,
    },
}

impl LevelKind {
    fn from_def(def: &LevelDef) -> LevelKind {
        let nfc = match def.format {
            NumFmt::Bullet => return LevelKind::Bullet,
            NumFmt::Decimal => 0,
            NumFmt::UpperRoman => 1,
            NumFmt::LowerRoman => 2,
            NumFmt::UpperLetter => 3,
            NumFmt::LowerLetter => 4,
        };
        LevelKind::Number {
            nfc,
            start: def.start,
            text: def.text.clone(),
        }
    }
}

struct Writer<'a> {
    markers: &'a HashMap<Vec<usize>, String>,
    numbering: &'a Numbering,
    styles: &'a StyleSheet,
    body: String,
    fonts: Vec<String>,
    /// `RRGGBB`, `\cfN` being the index plus one (0 is "auto").
    colors: Vec<String>,
    lists: Vec<ListDef>,
    /// The paragraph style of the first heading of each level, whose
    /// character formatting the stylesheet's `heading N` carries.
    heading_styles: [Option<String>; 9],
    /// What the stylesheet entry of the paragraph being written turns on, so
    /// a run that turns it off says so (`\b0`): a reader applying the
    /// paragraph's style would otherwise show it.
    para_base: RunProps,
}

impl Writer<'_> {
    fn finish(mut self) -> String {
        // The stylesheet first: its formatting adds to the font and colour
        // tables written before it.
        let mut sheet = String::from("{\\stylesheet{\\s0 Normal;}");
        for level in 1..=9usize {
            let _ = write!(sheet, "{{\\s{level}\\outlinelevel{}", level - 1);
            let style = self.heading_styles[level - 1]
                .clone()
                .unwrap_or_else(|| format!("Heading{level}"));
            let props = self
                .styles
                .effective_run(Some(&style), None, &RunProps::default());
            let formatting = self.props_words(&props, &RunProps::default());
            sheet.push_str(&formatting);
            let _ = write!(sheet, " heading {level};}}");
        }
        sheet.push_str("}\n");
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
        out.push_str(&sheet);
        if !self.lists.is_empty() {
            out.push_str("{\\*\\listtable");
            for (i, list) in self.lists.iter().enumerate() {
                let id = i + 1;
                let _ = write!(out, "{{\\list\\listtemplateid{id}");
                for (l, kind) in list.levels.iter().enumerate() {
                    list_level(l, kind.as_ref().unwrap_or(&LevelKind::Bullet), &mut out);
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
        let pstyle = p.props.style_id.as_deref();
        self.body.push_str("\\pard\\plain");
        self.para_base = RunProps::default();
        if let Some(level) = p.props.heading_level.filter(|l| (1..=9).contains(l)) {
            let _ = write!(self.body, "\\s{level}\\outlinelevel{}", level - 1);
            let slot = &mut self.heading_styles[usize::from(level) - 1];
            if slot.is_none() {
                *slot = p.props.style_id.clone();
            }
            // The stylesheet's `heading N` is the first such heading's style.
            let style = slot.clone().unwrap_or_else(|| format!("Heading{level}"));
            self.para_base = self
                .styles
                .effective_run(Some(&style), None, &RunProps::default());
        }
        self.body
            .push_str(match self.styles.effective_align(pstyle, p.props.align) {
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
        let listed = p.props.num_id;
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
        self.inlines(&p.content, pstyle);
        self.body.push_str(end);
        self.body.push('\n');
    }

    /// The `\lsN` for `num_id`. Its levels are the document's numbering
    /// definition; a level it does not define is what its first marker
    /// says.
    fn list(&mut self, num_id: i32, ilvl: usize, marker: Option<&str>) -> usize {
        let i = match self.lists.iter().position(|l| l.num_id == num_id) {
            Some(i) => i,
            None => {
                let numbering = self.numbering;
                let levels = std::array::from_fn(|l| {
                    numbering
                        .level(num_id, l as i32)
                        .map(|def| LevelKind::from_def(&def))
                });
                self.lists.push(ListDef { num_id, levels });
                self.lists.len() - 1
            }
        };
        let level = &mut self.lists[i].levels[ilvl];
        if level.is_none() {
            *level = Some(marker.map_or(LevelKind::Bullet, |m| level_kind(m, ilvl)));
        }
        i + 1
    }

    fn table(&mut self, t: &Table, path: &mut Vec<usize>) {
        let map = GridMap::of(t);
        for (ri, row) in t.rows.iter().enumerate() {
            let def = row_definition(t, row.cells.as_slice(), &map.rows[ri]);
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

    fn inlines(&mut self, content: &[Inline], pstyle: Option<&str>) {
        for inline in content {
            match inline {
                Inline::Run(r) => self.run(&r.text, &r.props, pstyle),
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
                            self.run(&r.text, &r.props, pstyle);
                        }
                        self.inlines(&h.content, pstyle);
                        continue;
                    }
                    self.body.push_str("{\\field{\\*\\fldinst {");
                    escape(&inst, &mut self.body);
                    self.body.push_str("}}{\\fldrslt {");
                    for r in &h.runs {
                        self.run(&r.text, &r.props, pstyle);
                    }
                    self.inlines(&h.content, pstyle);
                    self.body.push_str("}}}");
                }
                Inline::Break(kind, props) => {
                    let word = match kind {
                        BreakKind::Line | BreakKind::Clear(_) => "\\line",
                        BreakKind::Page => "\\page",
                        BreakKind::Column => "\\column",
                    };
                    self.control_run(word, props, pstyle);
                }
                Inline::Tab(props) => self.control_run("\\tab", props, pstyle),
                Inline::SmartArt { text, .. } => {
                    self.run(&text.join("\n"), &RunProps::default(), pstyle)
                }
                Inline::Chart { chart, .. } => self.run(
                    chart.title.as_deref().unwrap_or(""),
                    &RunProps::default(),
                    pstyle,
                ),
                Inline::Equation { text, .. } => self.run(text, &RunProps::default(), pstyle),
                Inline::Field { raw, text } => {
                    self.run(text, &crate::load::field_result_props(raw), pstyle)
                }
                Inline::TextBox { blocks, .. } => {
                    let mut first = true;
                    for p in text_box_paragraphs(blocks) {
                        if !std::mem::take(&mut first) {
                            self.body.push_str("\\line ");
                        }
                        self.inlines(&p.content, p.props.style_id.as_deref());
                    }
                }
                Inline::Revision {
                    kind: RevisionKind::Insert,
                    content,
                    ..
                } => self.inlines(content, pstyle),
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

    /// The formatting text with direct properties `props` has in a paragraph
    /// of style `pstyle`: its styles resolved, as the PDF exporter does.
    /// The underline and strike are the user's: a tracked change's display
    /// cue is not formatting (the OOXML serializer drops it too).
    fn effective(&self, props: &RunProps, pstyle: Option<&str>) -> RunProps {
        let direct = RunProps {
            underline: props.user_underline(),
            strike: props.user_strike(),
            ..props.clone()
        };
        self.styles
            .effective_run(pstyle, direct.style_id.as_deref(), &direct)
    }

    fn run(&mut self, text: &str, props: &RunProps, pstyle: Option<&str>) {
        if text.is_empty() {
            return;
        }
        let eff = self.effective(props, pstyle);
        let base = std::mem::take(&mut self.para_base);
        let words = self.props_words(&eff, &base);
        self.para_base = base;
        self.body.push('{');
        if !words.is_empty() {
            self.body.push_str(&words);
            self.body.push(' ');
        }
        escape(text, &mut self.body);
        self.body.push('}');
    }

    /// A tab or break, in its run's formatting.
    fn control_run(&mut self, word: &str, props: &RunProps, pstyle: Option<&str>) {
        let eff = self.effective(props, pstyle);
        let base = std::mem::take(&mut self.para_base);
        let words = self.props_words(&eff, &base);
        self.para_base = base;
        self.body.push('{');
        self.body.push_str(&words);
        self.body.push_str(word);
        self.body.push('}');
    }

    /// The control words for run properties `p`, adding its font and colour
    /// to their tables. What `base` (the paragraph style's formatting) turns
    /// on and `p` does not is turned off explicitly (`\\b0`).
    fn props_words(&mut self, p: &RunProps, base: &RunProps) -> String {
        let mut out = String::new();
        let flags = [
            (p.bold, base.bold, "\\b", "\\b0"),
            (p.italic, base.italic, "\\i", "\\i0"),
            (p.underline, base.underline, "\\ul", "\\ulnone"),
            (p.strike, base.strike, "\\strike", "\\strike0"),
            (p.caps, base.caps, "\\caps", "\\caps0"),
            (p.small_caps, base.small_caps, "\\scaps", "\\scaps0"),
            (p.vanish, base.vanish, "\\v", "\\v0"),
        ];
        for (on, inherited, word, off) in flags {
            if on {
                out.push_str(word);
            } else if inherited {
                out.push_str(off);
            }
        }
        match p.vert_align {
            VertAlign::Superscript => out.push_str("\\super"),
            VertAlign::Subscript => out.push_str("\\sub"),
            VertAlign::Baseline => {}
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
            let _ = write!(out, "\\f{i}");
        }
        if let Some(size) = p.size_half_pts {
            let _ = write!(out, "\\fs{size}");
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
            let _ = write!(out, "\\cf{}", i + 1);
        }
        out
    }
}

/// `\trowd` and the cell definitions for a row laid on the grid as `map`
/// says: the row starts past the columns `w:gridBefore` skips, and each
/// cell's right edge is the grid's edge after the columns it spans.
fn row_definition(t: &Table, cells: &[Cell], map: &RowMap) -> String {
    let width = |c: usize| {
        t.grid
            .get(c)
            .copied()
            .filter(|&w| w > 0)
            .unwrap_or(DEFAULT_COLUMN)
    };
    let edge = |end: usize| (0..end).map(width).sum::<u32>();
    let mut def = format!("\\trowd\\trgaph108\\trleft{}", edge(map.before));
    for (cell, &(start, span)) in cells.iter().zip(&map.cells) {
        match cell.v_merge {
            VMerge::Restart => def.push_str("\\clvmgf"),
            VMerge::Continue => def.push_str("\\clvmrg"),
            VMerge::None => {}
        }
        let _ = write!(def, "\\cellx{}", edge(start + span.max(1)));
    }
    def
}

/// A list level's `\listlevel` group: Word's default indents, a tab after
/// the number, and a numbered level's format, start and number text, or a
/// bullet.
fn list_level(level: usize, kind: &LevelKind, out: &mut String) {
    let indent = 720 * (level + 1);
    match kind {
        LevelKind::Bullet => {
            let _ = write!(
                out,
                "{{\\listlevel\\levelnfc23\\levelnfcn23\\leveljc0\\levelstartat1\\levelfollow0\
                 {{\\leveltext\\'01\\u8226 ?;}}{{\\levelnumbers;}}\\fi-360\\li{indent}}}"
            );
        }
        LevelKind::Number { nfc, start, text } => {
            let (leveltext, numbers) = level_text(text);
            let _ = write!(
                out,
                "{{\\listlevel\\levelnfc{nfc}\\levelnfcn{nfc}\\leveljc0\\levelstartat{start}\
                 \\levelfollow0{{\\leveltext{leveltext};}}{{\\levelnumbers{numbers};}}\
                 \\fi-360\\li{indent}}}"
            );
        }
    }
}

/// A `w:lvlText` (`%1.%2.`) as RTF's `\leveltext` (its length, then the
/// text, each `%N` as the placeholder `\'0(N-1)`) and `\levelnumbers` (the
/// 1-based position of each placeholder in that text).
fn level_text(text: &str) -> (String, String) {
    let mut body = String::new();
    let mut numbers = String::new();
    let mut len = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some(d) = chars
                .peek()
                .and_then(|d| d.to_digit(10))
                .filter(|d| (1..=9).contains(d))
            {
                chars.next();
                len += 1;
                let _ = write!(body, "\\'{:02x}", d - 1);
                let _ = write!(numbers, "\\'{len:02x}");
                continue;
            }
        }
        len += c.len_utf16();
        escape(&c.to_string(), &mut body);
    }
    (format!("\\'{len:02x}{body}"), numbers)
}

/// What a level's first marker says it is, for a list the document's
/// numbering does not define: a number (`1.`, `a)`, `i.`, `1.1.`) and its
/// format, or a bullet. The first marker of a level is its first number, so
/// a lone `i` is roman, not the ninth letter.
fn level_kind(marker: &str, ilvl: usize) -> LevelKind {
    let m = marker.trim();
    if !crate::import::marker_is_numbered(m) {
        return LevelKind::Bullet;
    }
    let suffix = m.chars().last().unwrap_or('.');
    let body = m.trim_end_matches(['.', ')']).trim_start_matches('(');
    // The last number of a compound marker (`1.2.`) is this level's.
    let own = body.rsplit('.').next().unwrap_or(body);
    let all_in = |set: &str| !own.is_empty() && own.chars().all(|c| set.contains(c));
    let nfc = if own.chars().all(|c| c.is_ascii_digit()) {
        0
    } else if all_in("ivxlcdm") {
        2
    } else if all_in("IVXLCDM") {
        1
    } else if own.chars().all(|c| c.is_lowercase()) {
        4
    } else if own.chars().all(|c| c.is_uppercase()) {
        3
    } else {
        0
    };
    LevelKind::Number {
        nfc,
        start: 1,
        text: format!("%{}{suffix}", ilvl + 1),
    }
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
pub(crate) mod tests {
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

    /// "Hello" with its paragraph mark deleted, then " world".
    pub(crate) fn deleted_mark_doc() -> Document {
        let mut first = ParProps::default();
        first
            .mark_revisions
            .push(crate::model::ParagraphMarkRevision {
                kind: RevisionKind::Delete,
                metadata: Default::default(),
            });
        Document {
            body: vec![
                para(first, vec![plain("Hello")]),
                para(ParProps::default(), vec![plain(" world")]),
            ],
        }
    }

    fn rtf(doc: &Document) -> String {
        to_rtf(doc, &ExportContext::for_package(None))
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
        let num = |nfc, suffix: &str, ilvl: usize| LevelKind::Number {
            nfc,
            start: 1,
            text: format!("%{}{suffix}", ilvl + 1),
        };
        assert_eq!(level_kind("1.", 0), num(0, ".", 0));
        assert_eq!(level_kind("a)", 0), num(4, ")", 0));
        assert_eq!(level_kind("B.", 0), num(3, ".", 0));
        assert_eq!(level_kind("iv.", 0), num(2, ".", 0));
        // A level's first marker: a lone i is roman (FIX r1 #1).
        assert_eq!(level_kind("i.", 0), num(2, ".", 0));
        assert_eq!(level_kind("I.", 1), num(1, ".", 1));
        // A compound marker is numbered, by its last number.
        assert_eq!(level_kind("1.1.", 1), num(0, ".", 1));
        assert_eq!(level_kind("\u{2022}", 0), LevelKind::Bullet);
    }

    /// The list's numbering definition, not its first marker, gives each
    /// level's format, start and number text (FIX r1 #1).
    #[test]
    fn list_levels_come_from_the_numbering_definition() {
        let numbering = crate::numbering::parse_numbering_xml(
            r#"<w:numbering>
            <w:abstractNum w:abstractNumId="0">
              <w:lvl w:ilvl="0"><w:start w:val="5"/><w:numFmt w:val="lowerRoman"/><w:lvlText w:val="%1)"/></w:lvl>
              <w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1.%2."/></w:lvl>
            </w:abstractNum>
            <w:num w:numId="7"><w:abstractNumId w:val="0"/></w:num>
            </w:numbering>"#,
        );
        let ctx = ExportContext {
            numbering,
            ..ExportContext::default()
        };
        let item = |ilvl, s: &str| {
            para(
                ParProps {
                    num_id: Some(7),
                    ilvl,
                    ..ParProps::default()
                },
                vec![plain(s)],
            )
        };
        let doc = Document {
            body: vec![item(0, "five"), item(1, "nested"), item(0, "six")],
        };
        let s = to_rtf(&doc, &ctx);
        assert!(
            s.contains("\\levelnfc2\\levelnfcn2\\leveljc0\\levelstartat5\\levelfollow0{\\leveltext\\'02\\'00);}{\\levelnumbers\\'01;}"),
            "{s}"
        );
        assert!(
            s.contains("\\levelnfc0\\levelnfcn0\\leveljc0\\levelstartat1\\levelfollow0{\\leveltext\\'04\\'00.\\'01.;}{\\levelnumbers\\'01\\'03;}"),
            "{s}"
        );
        assert!(s.contains("{\\listtext v)\\tab}"), "{s}");
        assert!(s.contains("{\\listtext v.1.\\tab}"), "{s}");
        // Read back, every item is numbered, the compound one included.
        let back = import_rtf(s.as_bytes()).unwrap();
        let ordered: Vec<(Option<i32>, i32)> = paras(&back)
            .iter()
            .map(|p| (p.props.num_id, p.props.ilvl))
            .collect();
        assert_eq!(ordered, [(Some(2), 0), (Some(2), 1), (Some(2), 0)]);
    }

    /// A numbered list whose number text has no `.` or `)` after it (`%1`,
    /// `%1.%2`, `Article %1:`) reads back numbered (FIX r2 #2): the reader
    /// takes the list table's format, not the marker's look. A bullet list
    /// stays bulleted.
    #[test]
    fn numbered_lists_of_any_number_text_read_back_numbered() {
        for (text, numbered) in [
            ("%1", true),
            ("%1.%2", true),
            ("Article %1:", true),
            ("\u{2022}", false),
        ] {
            let fmt = if numbered { "decimal" } else { "bullet" };
            let numbering = crate::numbering::parse_numbering_xml(&format!(
                r#"<w:numbering><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="{fmt}"/><w:lvlText w:val="{text}"/></w:lvl></w:abstractNum><w:num w:numId="3"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#
            ));
            let ctx = ExportContext {
                numbering,
                ..ExportContext::default()
            };
            let doc = Document {
                body: vec![para(
                    ParProps {
                        num_id: Some(3),
                        ..ParProps::default()
                    },
                    vec![plain("item")],
                )],
            };
            let s = to_rtf(&doc, &ctx);
            let back = import_rtf(s.as_bytes()).unwrap();
            let want = Some(if numbered { 2 } else { 1 });
            assert_eq!(paras(&back)[0].props.num_id, want, "{text}: {s}");
        }
    }

    /// A deleted paragraph mark joins its paragraph to the next (FIX r2 #1).
    #[test]
    fn a_deleted_paragraph_mark_joins_the_paragraphs() {
        let doc = deleted_mark_doc();
        let before = doc.clone();
        let got = back(&doc);
        assert_eq!(crate::import::paragraph_texts(&got), ["Hello world"]);
        assert_eq!(doc, before, "the document itself is unchanged");
    }

    /// A heading in a list keeps its list and marker (FIX r2 #3).
    #[test]
    fn a_numbered_heading_keeps_its_list() {
        let mut heading = crate::import::heading_props(1);
        heading.num_id = Some(2);
        let doc = Document {
            body: vec![para(heading, vec![plain("Intro")])],
        };
        let s = rtf(&doc);
        assert!(s.contains("\\s1\\outlinelevel0\\ql\\ls1\\ilvl0"), "{s}");
        assert!(s.contains("{\\listtext 1.\\tab}"), "{s}");
        // Read back, it is a heading and a numbered list item (FIX r3 #4).
        let back = import_rtf(s.as_bytes()).unwrap();
        let p = paras(&back)[0].props.clone();
        assert_eq!((p.heading_level, p.num_id, p.ilvl), (Some(1), Some(2), 0));
    }

    /// A tracked change's underline or strike cue is not formatting (FIX r2
    /// #4); the user's own underline still is.
    #[test]
    fn revision_cues_are_not_written_as_formatting() {
        let cued = |underline_added, strike_added| RunProps {
            underline: true,
            strike: true,
            revision_cues: crate::model::RevisionDisplayCues {
                insertions: 1,
                deletions: 0,
                underline_added,
                strike_added,
            },
            ..RunProps::default()
        };
        let doc = Document {
            body: vec![para(
                ParProps::default(),
                vec![
                    run("cue", cued(true, true)),
                    run("real", cued(false, false)),
                ],
            )],
        };
        let s = rtf(&doc);
        // The control words of the group a text is in.
        let words = |text: &str| {
            let end = s
                .find(&format!(" {text}}}"))
                .unwrap_or_else(|| panic!("{s}"));
            s[..end].rsplit('{').next().unwrap().to_string()
        };
        assert!(!words("cue").contains("\\ul"), "{s}");
        assert!(!words("cue").contains("\\strike"), "{s}");
        assert!(words("real").starts_with("\\ul\\strike"), "{s}");
        let back = import_rtf(s.as_bytes()).unwrap();
        let got: Vec<(String, bool, bool)> = runs(paras(&back)[0])
            .into_iter()
            .map(|(t, r)| (t, r.underline, r.strike))
            .collect();
        assert_eq!(
            got,
            [
                ("cue".to_string(), false, false),
                ("real".to_string(), true, true)
            ]
        );
    }

    #[test]
    fn level_text_counts_placeholders_and_text() {
        assert_eq!(
            level_text("%1."),
            ("\\'02\\'00.".to_string(), "\\'01".to_string())
        );
        assert_eq!(
            level_text("(%2)"),
            ("\\'03(\\'01)".to_string(), "\\'02".to_string())
        );
        assert_eq!(
            level_text("%1.%2.%3"),
            (
                "\\'05\\'00.\\'01.\\'02".to_string(),
                "\\'01\\'03\\'05".to_string()
            )
        );
        // A literal % stays text.
        assert_eq!(
            level_text("%%1"),
            ("\\'02%\\'00".to_string(), "\\'02".to_string())
        );
    }

    /// Formatting from styles is written (FIX r1 #3): a run in a bold
    /// character style is bold, a paragraph its style centres is centred,
    /// and a heading's stylesheet entry carries its style's formatting.
    #[test]
    fn styles_are_resolved_into_direct_formatting() {
        let styles = crate::styles::parse_styles_xml(
            r#"<w:styles>
            <w:style w:type="character" w:styleId="Strong"><w:name w:val="Strong"/><w:rPr><w:b/></w:rPr></w:style>
            <w:style w:type="paragraph" w:styleId="Title"><w:name w:val="Title"/><w:pPr><w:jc w:val="center"/></w:pPr></w:style>
            <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style>
            </w:styles>"#,
        );
        let ctx = ExportContext {
            styles,
            ..ExportContext::default()
        };
        let strong = RunProps {
            style_id: Some("Strong".into()),
            ..RunProps::default()
        };
        let doc = Document {
            body: vec![
                para(
                    ParProps {
                        style_id: Some("Title".into()),
                        ..ParProps::default()
                    },
                    vec![plain("Title")],
                ),
                para(
                    ParProps::default(),
                    vec![plain("a "), run("strong", strong)],
                ),
                para(crate::import::heading_props(1), vec![plain("Heading")]),
            ],
        };
        let s = to_rtf(&doc, &ctx);
        assert!(
            s.contains("{\\s1\\outlinelevel0\\b\\fs32 heading 1;}"),
            "{s}"
        );
        let back = import_rtf(s.as_bytes()).unwrap();
        let p = paras(&back);
        assert_eq!(p[0].props.align, Align::Center);
        let bold: Vec<(String, bool)> = runs(p[1]).into_iter().map(|(t, r)| (t, r.bold)).collect();
        assert_eq!(
            bold,
            [("a ".to_string(), false), ("strong".to_string(), true)]
        );
        assert!(runs(p[2]).iter().all(|(_, r)| r.bold), "{:?}", runs(p[2]));
    }

    /// A run that turns off what its heading's style turns on says so
    /// (FIX r3 #2): `\\b0` in the run and its tab, nothing for a run that
    /// keeps it.
    #[test]
    fn a_run_turning_off_its_headings_bold_says_so() {
        let styles = crate::styles::parse_styles_xml(
            r#"<w:styles>
            <w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:rPr><w:b/><w:i/></w:rPr></w:style>
            <w:style w:type="character" w:styleId="Plain"><w:name w:val="Plain"/><w:rPr><w:b w:val="0"/></w:rPr></w:style>
            </w:styles>"#,
        );
        let ctx = ExportContext {
            styles,
            ..ExportContext::default()
        };
        let plain_style = RunProps {
            style_id: Some("Plain".into()),
            ..RunProps::default()
        };
        let doc = Document {
            body: vec![
                para(
                    crate::import::heading_props(1),
                    vec![
                        plain("Bold"),
                        run("Not", plain_style.clone()),
                        Inline::Tab(plain_style),
                    ],
                ),
                para(ParProps::default(), vec![plain("Body")]),
            ],
        };
        let s = to_rtf(&doc, &ctx);
        assert!(s.contains("{\\b\\i Bold}"), "{s}");
        assert!(s.contains("{\\b0\\i Not}"), "{s}");
        assert!(s.contains("{\\b0\\i\\tab}"), "{s}");
        // A body paragraph's style turns nothing on: no resets.
        assert!(s.contains("\\ql {Body}"), "{s}");
    }

    /// A row's `w:gridBefore` columns are skipped (FIX r1 #5): the row
    /// starts at their edge and its cells' edges follow the grid.
    #[test]
    fn grid_before_moves_the_row_and_its_cell_edges() {
        let cell = |s: &str| Cell {
            blocks: vec![para(ParProps::default(), vec![plain(s)])],
            ..Cell::default()
        };
        let t = Table {
            grid: vec![1000, 3000],
            rows: vec![
                crate::model::Row {
                    cells: vec![cell("a"), cell("b")],
                    ..Default::default()
                },
                crate::model::Row {
                    cells: vec![cell("c")],
                    raw_props: vec!["<w:trPr><w:gridBefore w:val=\"1\"/></w:trPr>".into()],
                    ..Default::default()
                },
            ],
            ..Table::default()
        };
        let s = rtf(&Document {
            body: vec![Block::Table(t)],
        });
        assert!(
            s.contains("\\trowd\\trgaph108\\trleft0\\cellx1000\\cellx4000"),
            "{s}"
        );
        assert!(
            s.contains("\\trowd\\trgaph108\\trleft1000\\cellx4000"),
            "{s}"
        );
    }
}
