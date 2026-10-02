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
    // A PDF reader accepts junk before the header, within the first 1 KB.
    if find(&bytes[..bytes.len().min(1024)], b"%PDF-").is_some() {
        return Format::Pdf;
    }
    let text = trim_text_start(bytes);
    if text.starts_with(b"{\\rtf") {
        return Format::Rtf;
    }
    if looks_like_html(text) {
        return Format::Html;
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
/// table cells, the shape all the importers produce.
#[derive(Default)]
pub(crate) struct Builder {
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

impl Builder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Append `text` in `props`, joining the previous run when it has the
    /// same properties.
    pub(crate) fn text(&mut self, text: &str, props: &RunProps) {
        if text.is_empty() {
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
        self.para.push(Inline::Tab(props.clone()));
    }

    pub(crate) fn line_break(&mut self, props: &RunProps) {
        self.para
            .push(Inline::Break(BreakKind::Line, props.clone()));
    }

    pub(crate) fn page_break(&mut self, props: &RunProps) {
        self.para
            .push(Inline::Break(BreakKind::Page, props.clone()));
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
    /// paragraph, and a cell always has at least one.
    pub(crate) fn end_cell(&mut self, props: ParProps) {
        let has_para = self.table.as_ref().is_some_and(|t| t.cell_has_para);
        if !self.para.is_empty() || !has_para {
            self.end_para(props, true);
        }
        let t = self.table.get_or_insert_with(TableAcc::default);
        t.cells.push(Cell {
            blocks: std::mem::take(&mut t.blocks),
            ..Cell::default()
        });
        t.cell_has_para = false;
    }

    /// End a table row. A row with text but no cell end closes that cell.
    pub(crate) fn end_row(&mut self, props: ParProps) {
        let open_text =
            !self.para.is_empty() || self.table.as_ref().is_some_and(|t| !t.blocks.is_empty());
        if open_text {
            self.end_cell(props);
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
            if !t.blocks.is_empty() {
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
        let ncols = t
            .rows
            .iter()
            .map(|r| r.cells.len())
            .max()
            .unwrap_or(1)
            .max(1);
        // Rows shorter than the widest get empty cells, so the grid is square.
        for row in &mut t.rows {
            while row.cells.len() < ncols {
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
        let mut b = Builder::new();
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
