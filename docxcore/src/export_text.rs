//! Document → Plain Text (#635): what Word's Save As Plain Text writes.
//!
//! One line per paragraph, ended by CRLF; a line break is a CRLF too and a
//! tab a tab. A list paragraph starts with its marker and a tab (Word's
//! default number suffix). A table row is one line, its cells separated by
//! tabs (a cell's own paragraphs by CRLF, as Word writes them). Page and
//! column breaks add no character of their own: between text they end the
//! line, at a paragraph's edge the paragraph break already does.
//!
//! The text is the final view of tracked changes (insertions kept, deletions
//! dropped, a deleted paragraph mark joining its paragraph to the next; see
//! [`final_view`]). Hidden text (a hidden field result, tab or break
//! included), pictures, objects and content the model keeps only as raw XML
//! write nothing. Hidden is direct formatting only: styles do not model
//! `w:vanish` yet. The caller encodes the string (UTF-8, no BOM).

use std::collections::HashMap;

use crate::export_context::{ExportContext, final_view};
use crate::model::{Block, BreakKind, Cell, Document, Inline, Paragraph, RevisionKind, VMerge};

const CRLF: &str = "\r\n";

/// The plain text of `doc`'s final view, its lists numbered as `ctx`
/// defines them. An empty document is an empty string; otherwise every line,
/// the last included, ends in CRLF.
pub fn to_text(doc: &Document, ctx: &ExportContext) -> String {
    let doc = &final_view(doc);
    let markers = ctx.markers(doc);
    let w = Writer { markers: &markers };
    let mut lines = Vec::new();
    w.blocks_lines(&doc.body, &mut Vec::new(), &mut lines);
    let mut out = lines.join(CRLF);
    if !out.is_empty() {
        out.push_str(CRLF);
    }
    out
}

struct Writer<'a> {
    markers: &'a HashMap<Vec<usize>, String>,
}

impl Writer<'_> {
    fn blocks_lines(&self, blocks: &[Block], path: &mut Vec<usize>, lines: &mut Vec<String>) {
        for (i, block) in blocks.iter().enumerate() {
            path.push(i);
            match block {
                Block::Paragraph(p) => {
                    lines.push(self.paragraph_line(p, self.markers.get(path.as_slice())))
                }
                Block::Table(t) => {
                    for (ri, row) in t.rows.iter().enumerate() {
                        let cells: Vec<String> = row
                            .cells
                            .iter()
                            .enumerate()
                            .map(|(ci, cell)| {
                                path.push(ri);
                                path.push(ci);
                                let text = self.cell_text(cell, path);
                                path.pop();
                                path.pop();
                                text
                            })
                            .collect();
                        lines.push(cells.join("\t"));
                    }
                }
                // Content controls and other block XML the model does not
                // read, and the final section's properties: no text.
                Block::Raw(_) | Block::SectionProperties(_) => {}
            }
            path.pop();
        }
    }

    /// A cell's text: its paragraphs by CRLF; a cell merged into the one
    /// above is empty.
    fn cell_text(&self, cell: &Cell, path: &mut Vec<usize>) -> String {
        if cell.v_merge == VMerge::Continue {
            return String::new();
        }
        let mut lines = Vec::new();
        self.blocks_lines(&cell.blocks, path, &mut lines);
        lines.join(CRLF)
    }

    fn paragraph_line(&self, p: &Paragraph, marker: Option<&String>) -> String {
        let mut line = Line::default();
        if let Some(m) = marker {
            line.text(m);
            line.text("\t");
        }
        self.inlines(&p.content, &mut line);
        line.out
    }

    fn inlines(&self, content: &[Inline], line: &mut Line) {
        for inline in content {
            match inline {
                Inline::Run(r) if r.props.vanish => {}
                Inline::Run(r) => line.text(&r.text),
                Inline::Hyperlink(h) => {
                    for r in h.runs.iter().filter(|r| !r.props.vanish) {
                        line.text(&r.text);
                    }
                    self.inlines(&h.content, line);
                }
                // Hidden tabs and breaks are hidden text too.
                Inline::Break(_, props) | Inline::Tab(props) if props.vanish => {}
                Inline::Break(BreakKind::Line | BreakKind::Clear(_), _) => line.text("\n"),
                Inline::Break(BreakKind::Page | BreakKind::Column, _) => line.pending_break = true,
                Inline::Tab(_) => line.text("\t"),
                Inline::SmartArt { text, .. } => line.text(&text.join("\n")),
                Inline::Chart { chart, .. } => line.text(chart.title.as_deref().unwrap_or("")),
                Inline::Equation { text, .. } => line.text(text),
                // A field shows its cached result, unless the result is hidden.
                Inline::Field { raw, text } => {
                    if !crate::load::field_result_props(raw).vanish {
                        line.text(text)
                    }
                }
                Inline::TextBox { blocks, .. } => {
                    let mut lines = Vec::new();
                    let inner = Writer {
                        markers: &HashMap::new(),
                    };
                    inner.blocks_lines(blocks, &mut Vec::new(), &mut lines);
                    line.text(&lines.join("\n"));
                }
                Inline::Revision {
                    kind: RevisionKind::Insert,
                    content,
                    ..
                } => self.inlines(content, line),
                Inline::Revision {
                    kind: RevisionKind::Delete,
                    ..
                } => {}
                Inline::UnsupportedRevision { .. } => {}
                Inline::FootnoteRef { id, .. } => line.text(&id.to_string()),
                Inline::Raw(_) => {}
            }
        }
    }
}

/// A line being written, with a page or column break waiting to end it if
/// more text follows.
#[derive(Default)]
struct Line {
    out: String,
    pending_break: bool,
}

impl Line {
    fn text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if std::mem::take(&mut self.pending_break) && !self.out.is_empty() {
            self.out.push_str(CRLF);
        }
        // A line break inside a run's text (`\n`) is a CRLF too.
        for (i, piece) in s.split('\n').enumerate() {
            if i > 0 {
                self.out.push_str(CRLF);
            }
            self.out.push_str(piece.strip_suffix('\r').unwrap_or(piece));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Hyperlink, ParProps, Row, Run, RunProps, Table};

    fn run(s: &str) -> Inline {
        Inline::Run(Run {
            text: s.into(),
            props: RunProps::default(),
        })
    }

    fn para(content: Vec<Inline>) -> Block {
        Block::Paragraph(Paragraph {
            props: ParProps::default(),
            content,
        })
    }

    fn list(num_id: i32, s: &str) -> Block {
        Block::Paragraph(Paragraph {
            props: ParProps {
                num_id: Some(num_id),
                ..ParProps::default()
            },
            content: vec![run(s)],
        })
    }

    fn cell(s: &str) -> Cell {
        Cell {
            blocks: vec![para(vec![run(s)])],
            ..Cell::default()
        }
    }

    fn table(rows: &[&[&str]]) -> Block {
        Block::Table(Table {
            rows: rows
                .iter()
                .map(|r| Row {
                    cells: r.iter().map(|s| cell(s)).collect(),
                    ..Row::default()
                })
                .collect(),
            ..Table::default()
        })
    }

    fn text(doc: &Document) -> String {
        to_text(doc, &ExportContext::for_package(None))
    }

    #[test]
    fn the_fixed_document_writes_exactly_this() {
        let doc = Document {
            body: vec![
                para(vec![run("First paragraph.")]),
                para(vec![
                    run("Soft"),
                    Inline::Break(BreakKind::Line, RunProps::default()),
                    run("break\tand"),
                    Inline::Tab(RunProps::default()),
                    run("tab"),
                ]),
                table(&[&["A1", "B1"], &["A2", "B2"]]),
                list(2, "One"),
                list(2, "Two"),
                list(1, "Dot"),
                para(vec![
                    run("See "),
                    Inline::Hyperlink(Hyperlink {
                        target: Some("https://example.com".into()),
                        runs: vec![Run {
                            text: "the site".into(),
                            props: RunProps::default(),
                        }],
                        ..Hyperlink::default()
                    }),
                    run("."),
                ]),
            ],
        };
        assert_eq!(
            text(&doc),
            "First paragraph.\r\n\
             Soft\r\nbreak\tand\ttab\r\n\
             A1\tB1\r\n\
             A2\tB2\r\n\
             1.\tOne\r\n\
             2.\tTwo\r\n\
             \u{2022}\tDot\r\n\
             See the site.\r\n"
        );
    }

    #[test]
    fn every_line_ends_in_crlf_and_no_lone_lf() {
        let doc = crate::markdown::from_markdown(
            "a\n\nb  \nc\n\n- d\n- e\n\n| x | y |\n|---|---|\n| 1 | 2 |\n",
        );
        let s = text(&doc);
        assert!(s.ends_with("\r\n"), "{s:?}");
        assert!(!s.replace("\r\n", "").contains('\n'), "{s:?}");
        assert!(!s.replace("\r\n", "").contains('\r'), "{s:?}");
    }

    #[test]
    fn an_empty_document_is_an_empty_file() {
        assert_eq!(text(&Document::default()), "");
        assert_eq!(
            text(&Document {
                body: vec![para(vec![])]
            }),
            ""
        );
        assert_eq!(
            text(&Document {
                body: vec![para(vec![]), para(vec![])]
            }),
            "\r\n\r\n"
        );
    }

    #[test]
    fn tracked_changes_are_written_as_their_final_view() {
        let rev = |kind, s: &str| Inline::Revision {
            kind,
            metadata: Default::default(),
            raw: String::new(),
            content: vec![run(s)],
            content_changed: false,
        };
        let doc = Document {
            body: vec![para(vec![
                run("keep "),
                rev(RevisionKind::Insert, "added "),
                rev(RevisionKind::Delete, "removed "),
                run("end"),
            ])],
        };
        assert_eq!(text(&doc), "keep added end\r\n");
    }

    #[test]
    fn page_and_column_breaks_end_a_line_only_between_text() {
        let pb = || Inline::Break(BreakKind::Page, RunProps::default());
        let cb = || Inline::Break(BreakKind::Column, RunProps::default());
        let doc = Document {
            body: vec![
                para(vec![run("before"), pb()]),
                para(vec![pb(), run("after")]),
                para(vec![run("a"), cb(), run("b")]),
            ],
        };
        assert_eq!(text(&doc), "before\r\nafter\r\na\r\nb\r\n");
    }

    #[test]
    fn hidden_text_and_raw_content_write_nothing() {
        let hidden = Inline::Run(Run {
            text: "secret".into(),
            props: RunProps {
                vanish: true,
                ..RunProps::default()
            },
        });
        let doc = Document {
            body: vec![
                para(vec![
                    run("a"),
                    hidden,
                    Inline::Raw("<w:bookmarkStart/>".into()),
                    run("b"),
                ]),
                Block::Raw("<w:sdt/>".into()),
            ],
        };
        assert_eq!(text(&doc), "ab\r\n");
    }

    /// A field shows its cached result, unless that result is hidden
    /// (FIX r1 #4), as the RTF writer marks it `\v`.
    #[test]
    fn a_hidden_field_result_writes_nothing() {
        let field = |vanish: &str| Inline::Field {
            raw: format!(
                "<w:fldSimple w:instr=\" PAGE \"><w:r><w:rPr>{vanish}</w:rPr><w:t>7</w:t></w:r></w:fldSimple>"
            ),
            text: "7".into(),
        };
        let doc = Document {
            body: vec![
                para(vec![run("page "), field("")]),
                para(vec![run("hidden "), field("<w:vanish/>")]),
            ],
        };
        assert_eq!(text(&doc), "page 7\r\nhidden \r\n");
    }

    /// A deleted paragraph mark joins its paragraph to the next (FIX r2
    /// #1); the document itself is unchanged.
    #[test]
    fn a_deleted_paragraph_mark_joins_the_paragraphs() {
        let doc = crate::export_rtf::tests::deleted_mark_doc();
        let before = doc.clone();
        assert_eq!(text(&doc), "Hello world\r\n");
        assert_eq!(doc, before);
    }

    /// Hidden tabs and breaks write nothing, and a hidden page break does
    /// not end the line (FIX r2 #5).
    #[test]
    fn hidden_tabs_and_breaks_write_nothing() {
        let hidden = RunProps {
            vanish: true,
            ..RunProps::default()
        };
        let doc = Document {
            body: vec![para(vec![
                run("a"),
                Inline::Tab(hidden.clone()),
                Inline::Break(BreakKind::Line, hidden.clone()),
                Inline::Break(BreakKind::Page, hidden.clone()),
                Inline::Break(BreakKind::Column, hidden),
                run("b"),
            ])],
        };
        assert_eq!(text(&doc), "ab\r\n");
    }

    /// A heading in a list writes its marker (FIX r2 #3, as the RTF writer).
    #[test]
    fn a_numbered_heading_writes_its_marker() {
        let mut heading = crate::import::heading_props(1);
        heading.num_id = Some(2);
        let doc = Document {
            body: vec![Block::Paragraph(Paragraph {
                props: heading,
                content: vec![run("Intro")],
            })],
        };
        assert_eq!(text(&doc), "1.\tIntro\r\n");
    }

    #[test]
    fn a_merged_cell_is_empty_and_cell_paragraphs_break_lines() {
        let mut t = table(&[&["top", "x"], &["gone", "y"]]);
        if let Block::Table(t) = &mut t {
            t.rows[0].cells[0].v_merge = VMerge::Restart;
            t.rows[1].cells[0].v_merge = VMerge::Continue;
            t.rows[0].cells[1].blocks.push(para(vec![run("x2")]));
        }
        assert_eq!(text(&Document { body: vec![t] }), "top\tx\r\nx2\r\n\ty\r\n");
    }

    #[test]
    fn list_markers_in_a_table_come_from_their_path() {
        let mut t = table(&[&["cell"]]);
        if let Block::Table(t) = &mut t {
            t.rows[0].cells[0].blocks = vec![list(2, "in"), list(2, "cell")];
        }
        let doc = Document {
            body: vec![list(2, "out"), t],
        };
        assert_eq!(text(&doc), "1.\tout\r\n2.\tin\r\n3.\tcell\r\n");
    }
}
