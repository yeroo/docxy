//! A document clip as Rich Text Format, for the system clipboard (#1074):
//! what a copy puts there beside its plain text, so Pages, TextEdit and Word
//! keep its formatting, and what a paste takes from another app's copy.
//!
//! Both directions reuse the Save As writer ([`crate::export_rtf::to_rtf`])
//! and the RTF importer ([`crate::import::rtf::import_rtf`]); this module
//! only maps between a [`Clip`]'s paragraphs and a document's.
//!
//! The paragraph mark is the one thing a clip says that a document does
//! not. A clip copied from inside a paragraph (`[["abc"]]`) ends with no
//! mark, so its RTF ends without `\par`: a target app would otherwise add a
//! newline to every partial copy. A copy through the mark (`[["abc"], []]`)
//! ends with one `\par`. An importer closes the last paragraph either way,
//! so the paste takes the mark from the plain text copied with the RTF: when
//! that text ends with more newlines than the clip read from the RTF does,
//! the clip gains the mark it lost (an empty last paragraph). A trailing line
//! break is a newline in both, so it gains nothing.

use crate::editor::Clip;
use crate::export_context::ExportContext;
use crate::export_rtf::to_rtf;
use crate::import::Budget;
use crate::import::rtf::import_rtf_within;
use crate::model::{Block, BreakKind, Document, Inline, ParProps, Paragraph, RunProps};

impl Clip {
    /// The RTF for this clip, its runs' styles resolved as `ctx` (the
    /// source document's styles and numbering) defines them. Paragraph
    /// formatting is not in a clip, so none is written.
    pub fn to_rtf(&self, ctx: &ExportContext) -> String {
        let mut paras: &[Vec<Inline>] = &self.paras;
        // A copy through the last paragraph's mark: that paragraph's `\par`
        // is the mark, with no empty paragraph after it.
        let through_mark = paras.len() > 1 && paras.last().is_some_and(|p| p.is_empty());
        if through_mark {
            paras = &paras[..paras.len() - 1];
        }
        let doc = Document {
            body: paras
                .iter()
                .map(|content| {
                    Block::Paragraph(Paragraph {
                        props: ParProps::default(),
                        content: content.clone(),
                    })
                })
                .collect(),
        };
        let rtf = to_rtf(&doc, ctx);
        if through_mark {
            return rtf;
        }
        // The writer ends every paragraph with `\par`; the last of a partial
        // copy has none.
        match rtf.strip_suffix("\\par\n}") {
            Some(body) => format!("{body}\n}}"),
            None => rtf,
        }
    }

    /// The clip another app's RTF copy holds, or `None` when `bytes` is not
    /// RTF, holds no text, or is past what an import may cost: the paste
    /// then takes the plain text. `plain` is the text copied with it: it
    /// says whether the clip ends with a paragraph mark (see the module). A
    /// table row is one paragraph, its cells joined by tabs, as in plain
    /// text.
    pub fn from_rtf(bytes: &[u8], plain: Option<&str>) -> Option<Clip> {
        from_rtf_within(bytes, plain, &Budget::standard())
    }
}

/// [`Clip::from_rtf`] against `budget`.
fn from_rtf_within(bytes: &[u8], plain: Option<&str>, budget: &Budget) -> Option<Clip> {
    let doc = import_rtf_within(bytes, budget).ok()?;
    let mut paras = Vec::new();
    for block in &doc.body {
        match block {
            Block::Paragraph(p) => paras.push(p.content.clone()),
            Block::Table(t) => {
                for row in &t.rows {
                    let mut line = Vec::new();
                    for (i, cell) in row.cells.iter().enumerate() {
                        if i > 0 {
                            line.push(Inline::Tab(RunProps::default()));
                        }
                        cell_inlines(&cell.blocks, &mut line, &mut true);
                    }
                    paras.push(line);
                }
            }
            Block::Raw(_) | Block::SectionProperties(_) => {}
        }
    }
    if paras.is_empty() {
        return None;
    }
    let mut clip = Clip { paras };
    if plain.is_some_and(|t| trailing_newlines(t) > trailing_newlines(&clip.to_text())) {
        clip.paras.push(Vec::new());
    }
    Some(clip)
}

/// How many newlines `text` ends with, a CRLF counting as one.
fn trailing_newlines(text: &str) -> usize {
    text.replace("\r\n", "\n")
        .chars()
        .rev()
        .take_while(|&c| c == '\n')
        .count()
}

/// A table cell's content on one line: its paragraphs (and a nested table's)
/// joined by line breaks. `first` is true until the cell's first paragraph.
fn cell_inlines(blocks: &[Block], out: &mut Vec<Inline>, first: &mut bool) {
    for block in blocks {
        match block {
            Block::Paragraph(p) => {
                if !std::mem::take(first) {
                    out.push(Inline::Break(BreakKind::Line, RunProps::default()));
                }
                out.extend(p.content.iter().cloned());
            }
            Block::Table(t) => {
                for row in &t.rows {
                    for cell in &row.cells {
                        cell_inlines(&cell.blocks, out, first);
                    }
                }
            }
            Block::Raw(_) | Block::SectionProperties(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Run;

    fn run(text: &str, props: RunProps) -> Inline {
        Inline::Run(Run {
            text: text.into(),
            props,
        })
    }

    fn plain(text: &str) -> Inline {
        run(text, RunProps::default())
    }

    fn bold() -> RunProps {
        RunProps {
            bold: true,
            ..RunProps::default()
        }
    }

    /// A run's text and its bold, italic, underline and strike.
    type Shaped = (String, bool, bool, bool, bool);

    /// Each paragraph's runs, adjacent runs of equal formatting joined.
    fn shape(clip: &Clip) -> Vec<Vec<Shaped>> {
        clip.paras
            .iter()
            .map(|p| {
                let mut out: Vec<Shaped> = Vec::new();
                for i in p {
                    let (text, props) = match i {
                        Inline::Run(r) => (r.text.clone(), r.props.clone()),
                        Inline::Tab(props) => ("\t".to_string(), props.clone()),
                        other => (other.text(), RunProps::default()),
                    };
                    let key = (props.bold, props.italic, props.underline, props.strike);
                    match out.last_mut() {
                        Some(last) if (last.1, last.2, last.3, last.4) == key => {
                            last.0.push_str(&text)
                        }
                        _ => out.push((text, key.0, key.1, key.2, key.3)),
                    }
                }
                out
            })
            .collect()
    }

    /// The `\\par` control words in `rtf` (not `\\pard`).
    fn par_marks(rtf: &str) -> usize {
        rtf.match_indices("\\par")
            .filter(|(i, _)| {
                !rtf[i + 4..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
            })
            .count()
    }

    fn round_trip(clip: &Clip) -> Clip {
        let rtf = clip.to_rtf(&ExportContext::default());
        Clip::from_rtf(rtf.as_bytes(), Some(&clip.to_text()))
            .unwrap_or_else(|| panic!("no clip back from {rtf}"))
    }

    #[test]
    fn formatting_and_paragraphs_round_trip() {
        let all = RunProps {
            italic: true,
            underline: true,
            strike: true,
            ..bold()
        };
        let clip = Clip {
            paras: vec![
                vec![plain("Plain "), run("bold", bold()), plain(" end")],
                vec![run("all four", all)],
            ],
        };
        assert_eq!(shape(&round_trip(&clip)), shape(&clip));
    }

    /// The paragraph mark survives exactly: a partial copy gains no mark, a
    /// copy through it keeps it (and gains no empty paragraph).
    #[test]
    fn the_trailing_paragraph_mark_round_trips() {
        let a = || vec![plain("a")];
        let b = || vec![plain("b")];
        let a_break = || {
            vec![
                plain("a"),
                Inline::Break(BreakKind::Line, RunProps::default()),
            ]
        };
        for paras in [
            vec![a()],
            vec![a(), vec![]],
            vec![a(), b()],
            vec![a(), b(), vec![]],
            // Marks after blank paragraphs.
            vec![vec![], vec![]],
            vec![a(), vec![], vec![]],
            // A trailing line break is no paragraph mark.
            vec![a_break()],
            vec![a_break(), vec![]],
        ] {
            let clip = Clip { paras };
            assert_eq!(shape(&round_trip(&clip)), shape(&clip), "{clip:?}");
        }
    }

    #[test]
    fn a_partial_copy_ends_without_a_paragraph_mark() {
        let clip = Clip {
            paras: vec![vec![run("abc", bold())]],
        };
        let rtf = clip.to_rtf(&ExportContext::default());
        assert_eq!(par_marks(&rtf), 0, "{rtf}");
        assert!(rtf.ends_with('}'), "{rtf}");
        let through = Clip {
            paras: vec![vec![plain("abc")], vec![]],
        };
        let rtf = through.to_rtf(&ExportContext::default());
        assert_eq!(par_marks(&rtf), 1, "{rtf}");
    }

    /// A run bold through its character style is bold in the RTF: the
    /// source document's styles are resolved.
    #[test]
    fn character_styles_resolve_through_the_context() {
        let styles = crate::styles::parse_styles_xml(
            r#"<w:styles>
            <w:style w:type="character" w:styleId="Strong"><w:name w:val="Strong"/><w:rPr><w:b/></w:rPr></w:style>
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
        let clip = Clip {
            paras: vec![vec![run("strong", strong)]],
        };
        let rtf = clip.to_rtf(&ctx);
        assert!(rtf.contains("\\b"), "{rtf}");
        let back = Clip::from_rtf(rtf.as_bytes(), Some("strong")).unwrap();
        assert_eq!(
            shape(&back),
            vec![vec![("strong".to_string(), true, false, false, false)]]
        );
    }

    /// RTF as TextEdit and Pages put it on the pasteboard.
    #[test]
    fn a_pages_style_copy_pastes_its_bold_and_italic_runs() {
        // Lines kept in an array: Cocoa ends a run's line with its space.
        let rtf = [
            r"{\rtf1\ansi\ansicpg1252\cocoartf2822",
            r"\cocoatextscaling0\cocoaplatform0{\fonttbl\f0\fswiss\fcharset0 Helvetica;\f1\fswiss\fcharset0 Helvetica-Bold;\f2\fswiss\fcharset0 Helvetica-Oblique;}",
            r"{\colortbl;\red255\green255\blue255;}",
            r"{\*\expandedcolortbl;;}",
            r"\pard\tx720\tx1440\pardirnatural\partightenfactor0",
            "",
            r"\f0\fs24 \cf0 Plain then ",
            r"\f1\b BOLD",
            r"\f0\b0  then ",
            r"\f2\i italic",
            r"\f0\i0  from Pages}",
        ]
        .join("\n");
        let rtf = rtf.as_bytes();
        let plain_text = "Plain then BOLD then italic from Pages";
        let clip = Clip::from_rtf(rtf, Some(plain_text)).unwrap();
        assert_eq!(clip.to_text(), plain_text);
        assert_eq!(
            shape(&clip),
            vec![vec![
                ("Plain then ".to_string(), false, false, false, false),
                ("BOLD".to_string(), true, false, false, false),
                (" then ".to_string(), false, false, false, false),
                ("italic".to_string(), false, true, false, false),
                (" from Pages".to_string(), false, false, false, false),
            ]]
        );
    }

    /// Word's copy of a whole paragraph: `\par` at the end, CRLF text.
    #[test]
    fn a_whole_paragraph_copy_keeps_its_mark() {
        let rtf = br"{\rtf1\ansi\deff0{\fonttbl{\f0 Calibri;}}\pard\plain x\par
}";
        let clip = Clip::from_rtf(rtf, Some("x\r\n")).unwrap();
        assert_eq!(clip.paras.len(), 2);
        assert!(clip.paras[1].is_empty());
        // Without the plain text the mark cannot be told.
        assert_eq!(Clip::from_rtf(rtf, None).unwrap().paras.len(), 1);
    }

    #[test]
    fn a_table_row_is_one_tab_separated_paragraph() {
        let rtf = br"{\rtf1\ansi\deff0\trowd\cellx1000\cellx2000\intbl a\cell\intbl b\cell\row\trowd\cellx1000\cellx2000\intbl c\cell\intbl d\cell\row}";
        let clip = Clip::from_rtf(rtf, Some("a\tb\nc\td")).unwrap();
        assert_eq!(clip.to_text(), "a\tb\nc\td");
    }

    /// A cell's paragraphs are joined by a line break whatever they hold: an
    /// empty first one, or one ending in a tab.
    #[test]
    fn a_cell_keeps_its_paragraph_boundaries() {
        let rtf = br"{\rtf1\ansi\trowd\cellx1000\cellx2000\intbl\par\intbl b\cell\intbl a\tab\par\intbl b\cell\row}";
        let clip = Clip::from_rtf(rtf, None).unwrap();
        assert_eq!(clip.to_text(), "\nb\ta\t\nb");
    }

    #[test]
    fn what_is_not_rtf_is_no_clip() {
        assert_eq!(Clip::from_rtf(b"plain words", Some("plain words")), None);
        assert_eq!(Clip::from_rtf(b"", None), None);
        assert_eq!(Clip::from_rtf(b"{\\rtf1\\ansi }", None), None);
        assert_eq!(Clip::from_rtf(&[0xff, 0xfe, 0x00, b'{'], None), None);
    }

    /// Past an import's budget the paste falls back to the plain text.
    #[test]
    fn an_rtf_past_the_import_budget_is_no_clip() {
        let rtf = format!("{{\\rtf1\\ansi {}}}", "word ".repeat(10_000));
        assert!(Clip::from_rtf(rtf.as_bytes(), None).is_some());
        let budget = Budget::new(1 << 20, 100);
        assert_eq!(from_rtf_within(rtf.as_bytes(), None, &budget), None);
    }
}
