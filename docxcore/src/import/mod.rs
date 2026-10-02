//! Opening what Word opens besides `.docx` (#633): RTF, a Web Page, a PDF,
//! and the text of a damaged file (Word's Recover Text from Any File).
//!
//! Each importer turns bytes into a plain [`Document`]: paragraphs of runs
//! with bold / italic / underline, heading styles (`Heading1`..`Heading6`),
//! list membership (numbering ids 1 = bullets and 2 = decimal, the two that
//! [`crate::package::new_markdown_package`] defines) and simple tables. The
//! result has no package of its own; it is saved through a fresh Markdown
//! package, which defines every style and list it uses. Images, fields,
//! notes, comments and section layout are not converted.
//!
//! Format is decided by content first ([`sniff`]), so an RTF or HTML file
//! named `.doc` or `.docx` still opens as what it is.
//!
//! Every import runs against one [`Budget`] (#633 r2): the input it scans,
//! the bytes it decodes and the document it builds are all charged, so a
//! hostile file fails with a load error ([`TOO_BIG`]) instead of aborting the
//! process or hanging the UI thread. A cost that is not charged directly is
//! O(1) per charged unit, and says so where that is not obvious.

pub mod html;
pub mod pdf;
pub mod recover;
pub mod rtf;

pub use html::import_html;
pub use pdf::import_pdf;
pub use recover::{paragraph_count, recover_any_text, recover_docx_text};
pub use rtf::import_rtf;

use crate::model::{
    Block, BreakKind, Cell, Document, Inline, ParProps, Paragraph, Row, Run, RunProps, Table,
};
use std::cell::Cell as Counter;

/// What any import says when its [`Budget`] runs out.
pub const TOO_BIG: &str = "the file is too large or too complex to convert";

/// The most stream data one import may decode (inflate and predictor
/// output, content streams joined).
pub(crate) const MAX_DECODED: usize = 256 << 20;
/// The most work one import may do, in units: a token, a glyph, a run, a
/// paragraph, a cell, a map entry, or 16 bytes of input scanned.
pub(crate) const MAX_WORK: usize = 64_000_000;
/// Word's widest table; cells past it in a row join its last cell.
pub(crate) const MAX_COLUMNS: usize = 63;
/// The deepest nesting an importer keeps (HTML elements, RTF groups);
/// opens past it are ignored.
pub(crate) const MAX_DEPTH: usize = 256;

/// What an import may still spend: decoded bytes and units of work. Once
/// either runs out it stays out, and the import fails with [`TOO_BIG`].
pub(crate) struct Budget {
    bytes: Counter<usize>,
    work: Counter<usize>,
    out: Counter<bool>,
}

impl Budget {
    pub(crate) fn new(bytes: usize, work: usize) -> Self {
        Budget {
            bytes: Counter::new(bytes),
            work: Counter::new(work),
            out: Counter::new(false),
        }
    }

    /// The budget every import gets.
    pub(crate) fn standard() -> Self {
        Self::new(MAX_DECODED, MAX_WORK)
    }

    /// Decoded bytes that may still be made.
    pub(crate) fn room(&self) -> usize {
        if self.out.get() { 0 } else { self.bytes.get() }
    }

    /// Spend `n` decoded bytes; `false` (and out for good) past the budget.
    pub(crate) fn take_bytes(&self, n: usize) -> bool {
        match self.bytes.get().checked_sub(n) {
            Some(left) if !self.out.get() => {
                self.bytes.set(left);
                true
            }
            _ => self.fail(),
        }
    }

    /// Spend `n` units of work; `false` (and out for good) past the budget.
    pub(crate) fn work(&self, n: usize) -> bool {
        match self.work.get().checked_sub(n) {
            Some(left) if !self.out.get() => {
                self.work.set(left);
                true
            }
            _ => self.fail(),
        }
    }

    /// Spend one unit of work.
    pub(crate) fn op(&self) -> bool {
        self.work(1)
    }

    /// Spend the work of scanning `n` bytes of input once.
    pub(crate) fn scanned(&self, n: usize) -> bool {
        self.work(n / 16 + 1)
    }

    /// Run out now.
    pub(crate) fn fail(&self) -> bool {
        self.out.set(true);
        false
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.out.get()
    }
}

/// What a file holds, by its first bytes (and its extension when they do
/// not say).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A ZIP container: a Word package, or what is left of one.
    Docx,
    Rtf,
    Html,
    Pdf,
    /// An OLE2 compound file: a Word 97-2003 `.doc`, or an encrypted
    /// (password-protected) `.docx`. Neither is read.
    Cfb,
    /// Nothing recognised.
    Unknown,
}

/// Decide a file's format from its content, then from `ext` (the extension,
/// any case, without the dot).
pub fn sniff(bytes: &[u8], ext: &str) -> Format {
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        return Format::Docx;
    }
    if bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]) {
        return Format::Cfb;
    }
    // The signatures that must start the file come first, so a page or an
    // RTF that only mentions `%PDF-` near its top is still what it is.
    let text = trim_text_start(bytes);
    if text.starts_with(b"{\\rtf") {
        return Format::Rtf;
    }
    if looks_like_html(text) {
        return Format::Html;
    }
    // A PDF reader accepts junk before the header, within the first 1 KB.
    if find(&bytes[..bytes.len().min(1024)], b"%PDF-").is_some() {
        return Format::Pdf;
    }
    match ext.to_ascii_lowercase().as_str() {
        "rtf" => Format::Rtf,
        "htm" | "html" | "xhtml" => Format::Html,
        "pdf" => Format::Pdf,
        "docx" | "docm" | "dotx" | "dotm" => Format::Docx,
        _ => Format::Unknown,
    }
}

/// `bytes` without a UTF-8 byte-order mark and leading whitespace.
fn trim_text_start(bytes: &[u8]) -> &[u8] {
    let b = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let start = b
        .iter()
        .position(|c| !c.is_ascii_whitespace())
        .unwrap_or(b.len());
    &b[start..]
}

fn looks_like_html(text: &[u8]) -> bool {
    if !text.starts_with(b"<") {
        return false;
    }
    let head: Vec<u8> = text[..text.len().min(4096)].to_ascii_lowercase();
    [b"<html".as_slice(), b"<!doctype html", b"<head", b"<body"]
        .iter()
        .any(|m| find(&head, m).is_some())
}

pub(crate) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The paragraph properties of a heading at `level` (1-based; deeper than 6
/// is styled as 6, the deepest the Markdown package defines).
pub(crate) fn heading_props(level: u8) -> ParProps {
    let level = level.clamp(1, 6);
    ParProps {
        heading_level: Some(level),
        style_id: Some(format!("Heading{level}")),
        ..ParProps::default()
    }
}

/// Whether a list marker as Word writes it (`1.`, `a)`, `iv.`) is a
/// numbered one; anything else (`·`, `o`, `§`, `•`, U+F0B7) is a bullet.
pub(crate) fn marker_is_numbered(marker: &str) -> bool {
    let m = marker.trim();
    let Some(body) = m.strip_suffix('.').or_else(|| m.strip_suffix(')')) else {
        return false;
    };
    let body = body.strip_prefix('(').unwrap_or(body);
    !body.is_empty() && body.chars().all(|c| c.is_alphanumeric())
}

/// Builds a [`Document`] from a stream of text, breaks, paragraph ends and
/// table cells, the shape all the importers produce. Everything it makes is
/// charged to the import's [`Budget`]; once that is out it makes nothing
/// more.
pub(crate) struct Builder<'b> {
    budget: &'b Budget,
    body: Vec<Block>,
    para: Vec<Inline>,
    table: Option<TableAcc>,
}

#[derive(Default)]
struct TableAcc {
    rows: Vec<Row>,
    cells: Vec<Cell>,
    blocks: Vec<Block>,
    /// A paragraph has been ended in the current cell since it began, so an
    /// empty cell end need not add one.
    cell_has_para: bool,
}

impl<'b> Builder<'b> {
    pub(crate) fn new(budget: &'b Budget) -> Self {
        Builder {
            budget,
            body: Vec::new(),
            para: Vec::new(),
            table: None,
        }
    }

    /// Append `text` in `props`, joining the previous run when it has the
    /// same properties.
    pub(crate) fn text(&mut self, text: &str, props: &RunProps) {
        if text.is_empty() || !self.budget.scanned(text.len()) {
            return;
        }
        if let Some(Inline::Run(run)) = self.para.last_mut() {
            if run.props == *props {
                run.text.push_str(text);
                return;
            }
        }
        self.para.push(Inline::Run(Run {
            text: text.to_string(),
            props: props.clone(),
        }));
    }

    pub(crate) fn tab(&mut self, props: &RunProps) {
        if self.budget.op() {
            self.para.push(Inline::Tab(props.clone()));
        }
    }

    pub(crate) fn line_break(&mut self, props: &RunProps) {
        if self.budget.op() {
            self.para
                .push(Inline::Break(BreakKind::Line, props.clone()));
        }
    }

    pub(crate) fn page_break(&mut self, props: &RunProps) {
        if self.budget.op() {
            self.para
                .push(Inline::Break(BreakKind::Page, props.clone()));
        }
    }

    /// The text of the paragraph in progress.
    pub(crate) fn para_text(&self) -> String {
        self.para.iter().map(|i| i.text()).collect()
    }

    /// Drop everything in the paragraph in progress (text only made of
    /// spaces that stands for an empty paragraph).
    pub(crate) fn clear_para(&mut self) {
        self.para.clear();
    }

    /// Remove a list marker typed at the start of the paragraph in progress
    /// (`·`, `1.`, then spaces or a tab) and return it; `None`, changing
    /// nothing, when the paragraph does not start with a short word followed
    /// by white space.
    pub(crate) fn take_leading_marker(&mut self) -> Option<String> {
        let text = self.para_text();
        let marker: String = text.chars().take_while(|c| !c.is_whitespace()).collect();
        let n = marker.chars().count();
        if n == 0 || n > 6 {
            return None;
        }
        let rest = text.chars().skip(n);
        let gap = rest.take_while(|c| c.is_whitespace()).count();
        if gap == 0 {
            return None;
        }
        // Drop `n + gap` characters from the front, run by run.
        let mut drop = n + gap;
        while drop > 0 && !self.para.is_empty() {
            let len = self.para[0].text().chars().count();
            if len <= drop {
                self.para.remove(0);
                drop -= len;
            } else if let Inline::Run(run) = &mut self.para[0] {
                run.text = run.text.chars().skip(drop).collect();
                drop = 0;
            } else {
                break;
            }
        }
        Some(marker)
    }

    /// End the paragraph in progress, with `props`, in the open table cell
    /// when `in_table`, else in the body (closing a table left open).
    pub(crate) fn end_para(&mut self, props: ParProps, in_table: bool) {
        if !self.budget.op() {
            self.para.clear();
            return;
        }
        let p = Block::Paragraph(Paragraph {
            props,
            content: std::mem::take(&mut self.para),
        });
        if in_table {
            let t = self.table.get_or_insert_with(TableAcc::default);
            t.blocks.push(p);
            t.cell_has_para = true;
        } else {
            self.end_table();
            self.body.push(p);
        }
    }

    /// End the paragraph in progress only when it has content.
    pub(crate) fn end_para_if_any(&mut self, props: ParProps, in_table: bool) {
        if !self.para.is_empty() {
            self.end_para(props, in_table);
        }
    }

    /// End a table cell: text since the last paragraph end is its last
    /// paragraph, and a cell always has at least one. A row keeps at most
    /// [`MAX_COLUMNS`] cells: past that, a cell's paragraphs join the last.
    pub(crate) fn end_cell(&mut self, props: ParProps) {
        let has_para = self.table.as_ref().is_some_and(|t| t.cell_has_para);
        if !self.para.is_empty() || !has_para {
            self.end_para(props, true);
        }
        if !self.budget.op() {
            return;
        }
        let t = self.table.get_or_insert_with(TableAcc::default);
        let blocks = std::mem::take(&mut t.blocks);
        let full = t.cells.len() >= MAX_COLUMNS;
        match t.cells.last_mut() {
            Some(last) if full => last.blocks.extend(blocks),
            _ => t.cells.push(Cell {
                blocks,
                ..Cell::default()
            }),
        }
        t.cell_has_para = false;
    }

    /// End a table row. A row with text but no cell end closes that cell.
    pub(crate) fn end_row(&mut self, props: ParProps) {
        let open_text =
            !self.para.is_empty() || self.table.as_ref().is_some_and(|t| !t.blocks.is_empty());
        if open_text {
            self.end_cell(props);
        }
        if !self.budget.op() {
            return;
        }
        if let Some(t) = self.table.as_mut() {
            if !t.cells.is_empty() {
                t.rows.push(Row {
                    cells: std::mem::take(&mut t.cells),
                    ..Row::default()
                });
            }
        }
    }

    /// Close the table in progress, if any, into the body.
    pub(crate) fn end_table(&mut self) {
        let Some(mut t) = self.table.take() else {
            return;
        };
        if !t.cells.is_empty() || !t.blocks.is_empty() {
            // Cells with no row end: keep them as a last row.
            if !t.blocks.is_empty() && t.cells.len() < MAX_COLUMNS {
                t.cells.push(Cell {
                    blocks: std::mem::take(&mut t.blocks),
                    ..Cell::default()
                });
            }
            t.rows.push(Row {
                cells: std::mem::take(&mut t.cells),
                ..Row::default()
            });
        }
        if t.rows.is_empty() {
            return;
        }
        // At most MAX_COLUMNS (end_cell keeps rows to that), so the padding
        // below is at most that many cells a row, each charged.
        let ncols = t
            .rows
            .iter()
            .map(|r| r.cells.len())
            .max()
            .unwrap_or(1)
            .clamp(1, MAX_COLUMNS);
        // Rows shorter than the widest get empty cells, so the grid is square.
        for row in &mut t.rows {
            while row.cells.len() < ncols && self.budget.op() {
                row.cells.push(Cell {
                    blocks: vec![Block::Paragraph(Paragraph::default())],
                    ..Cell::default()
                });
            }
        }
        // A 6.5" text column shared evenly.
        let w = (9360 / ncols) as u32;
        self.body.push(Block::Table(Table {
            grid: vec![w; ncols],
            rows: t.rows,
            ..Table::default()
        }));
    }

    pub(crate) fn finish(mut self, props: ParProps) -> Document {
        let in_table = self.table.is_some() && !self.para.is_empty();
        if in_table {
            self.end_cell(props.clone());
        } else {
            self.end_para_if_any(props, false);
        }
        self.end_table();
        Document { body: self.body }
    }

    /// Whether nothing at all has been produced.
    pub(crate) fn is_empty(&self) -> bool {
        self.body.is_empty() && self.para.is_empty() && self.table.is_none()
    }
}

/// Text of every paragraph of `doc`, tables row-major, one entry per
/// paragraph: what the tests compare.
pub fn paragraph_texts(doc: &Document) -> Vec<String> {
    fn walk(blocks: &[Block], out: &mut Vec<String>) {
        for b in blocks {
            match b {
                Block::Paragraph(p) => out.push(p.plain_text()),
                Block::Table(t) => {
                    for row in &t.rows {
                        for cell in &row.cells {
                            walk(&cell.blocks, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(&doc.body, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_reads_content_before_the_extension() {
        assert_eq!(sniff(b"{\\rtf1\\ansi hi}", "docx"), Format::Rtf);
        assert_eq!(sniff(b"\xEF\xBB\xBF  {\\rtf1}", ""), Format::Rtf);
        assert_eq!(sniff(b"%PDF-1.7\n", "docx"), Format::Pdf);
        assert_eq!(sniff(b"junk\r\n%PDF-1.4", ""), Format::Pdf);
        assert_eq!(sniff(b"PK\x03\x04rest", "rtf"), Format::Docx);
        assert_eq!(
            sniff(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0], "docx"),
            Format::Cfb
        );
        assert_eq!(sniff(b"<html><body>x</body></html>", "doc"), Format::Html);
        assert_eq!(sniff(b"<!DOCTYPE HTML PUBLIC>", ""), Format::Html);
        assert_eq!(sniff(b"plain words", "htm"), Format::Html);
        assert_eq!(sniff(b"plain words", "DOCX"), Format::Docx);
        assert_eq!(sniff(b"plain words", "txt"), Format::Unknown);
        assert_eq!(sniff(b"<note>xml</note>", "xml"), Format::Unknown);
        // Mentioning the PDF header does not make a page or an RTF a PDF.
        assert_eq!(
            sniff(
                b"<html><head><title>Notes on %PDF-1.7 headers</title>",
                "htm"
            ),
            Format::Html
        );
        assert_eq!(sniff(b"{\\rtf1 about %PDF-1.4}", "rtf"), Format::Rtf);
    }

    #[test]
    fn list_markers_say_numbered_or_bullet() {
        for m in ["1.", "12.", "a)", "(iv)", "B."] {
            assert!(marker_is_numbered(m), "{m}");
        }
        for m in ["\u{b7}", "o", "\u{a7}", "\u{2022}", "\u{f0b7}", "-", "."] {
            assert!(!marker_is_numbered(m), "{m}");
        }
    }

    #[test]
    fn builder_makes_paragraphs_and_a_square_table() {
        let plain = RunProps::default();
        let bold = RunProps {
            bold: true,
            ..RunProps::default()
        };
        let budget = Budget::standard();
        let mut b = Builder::new(&budget);
        b.text("Hello ", &plain);
        b.text("big", &bold);
        b.text(" world", &plain);
        b.end_para(ParProps::default(), false);
        b.text("a", &plain);
        b.end_cell(ParProps::default());
        b.text("b", &plain);
        b.end_cell(ParProps::default());
        b.end_row(ParProps::default());
        b.text("c", &plain);
        b.end_cell(ParProps::default());
        b.end_row(ParProps::default());
        b.text("after", &plain);
        b.end_para(ParProps::default(), false);
        let doc = b.finish(ParProps::default());
        assert_eq!(
            paragraph_texts(&doc),
            ["Hello big world", "a", "b", "c", "", "after"]
        );
        let Block::Table(t) = &doc.body[1] else {
            panic!("{:?}", doc.body[1]);
        };
        assert_eq!(t.rows.len(), 2);
        assert!(t.rows.iter().all(|r| r.cells.len() == 2));
        let Block::Paragraph(p) = &doc.body[0] else {
            panic!()
        };
        assert_eq!(p.content.len(), 3, "same-props text joins one run");
    }
}
