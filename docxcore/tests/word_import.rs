//! The importers (#633) against what Word itself wrote: one document Word
//! saved as `.docx`, `.rtf`, filtered Web Page and PDF
//! (`corpus/word-import/`, made by `corpus/tools/gen_word_import.ps1`). The
//! `.docx` is the oracle the other three are compared with.

use docxcore::import::{self, paragraph_texts};
use docxcore::model::{Block, Document, Inline, Paragraph};
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../corpus/word-import")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn oracle() -> Document {
    docxcore::package::load_package(&fixture("source.docx"))
        .expect("Word's .docx loads")
        .document
}

/// Every paragraph, tables row-major, in order.
fn paragraphs(doc: &Document) -> Vec<&Paragraph> {
    fn walk<'a>(blocks: &'a [Block], out: &mut Vec<&'a Paragraph>) {
        for b in blocks {
            match b {
                Block::Paragraph(p) => out.push(p),
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

/// The text of every run with `pick` set, in order, joined.
fn runs_where(doc: &Document, pick: fn(&docxcore::model::RunProps) -> bool) -> String {
    let mut out = String::new();
    for p in paragraphs(doc) {
        for i in &p.content {
            if let Inline::Run(r) = i {
                if pick(&r.props) && !r.text.trim().is_empty() {
                    out.push_str(r.text.trim());
                    out.push('|');
                }
            }
        }
    }
    out
}

fn heading_levels(doc: &Document) -> Vec<Option<u8>> {
    paragraphs(doc)
        .iter()
        .map(|p| {
            p.props
                .style_id
                .as_deref()
                .and_then(docxcore::load::heading_level)
        })
        .collect()
}

fn listed(doc: &Document) -> Vec<bool> {
    paragraphs(doc)
        .iter()
        .map(|p| p.props.num_id.is_some())
        .collect()
}

/// White space (tabs, non-breaking spaces, line breaks) collapsed to one
/// space and trimmed: what a Web Page or a PDF keeps of a paragraph.
fn collapse(s: &str) -> String {
    s.split(|c: char| c.is_whitespace())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The formatting checks shared by RTF and HTML: bold, italic and underlined
/// runs, heading levels and list membership, paragraph by paragraph.
fn same_formatting(got: &Document, want: &Document, what: &str) {
    assert_eq!(
        heading_levels(got),
        heading_levels(want),
        "{what}: headings"
    );
    assert_eq!(listed(got), listed(want), "{what}: list membership");
    assert_eq!(
        runs_where(got, |p| p.bold),
        runs_where(want, |p| p.bold),
        "{what}: bold"
    );
    assert_eq!(
        runs_where(got, |p| p.italic),
        runs_where(want, |p| p.italic),
        "{what}: italic"
    );
    assert_eq!(
        runs_where(got, |p| p.underline),
        runs_where(want, |p| p.underline),
        "{what}: underline"
    );
}

#[test]
fn word_rtf_matches_word_docx() {
    let want = oracle();
    let got = import::import_rtf(&fixture("source.rtf")).expect("RTF imports");
    assert_eq!(paragraph_texts(&got), paragraph_texts(&want));
    same_formatting(&got, &want, "rtf");
    // Lists are bullets, then numbers, as Word's.
    let nums: Vec<i32> = paragraphs(&got)
        .iter()
        .filter_map(|p| p.props.num_id)
        .collect();
    assert_eq!(nums, [1, 1, 1, 2, 2], "bullets then decimals");
}

#[test]
fn word_web_page_matches_word_docx() {
    let want = oracle();
    let got = import::import_html(&fixture("source.htm")).expect("HTML imports");
    // A Web Page writes a tab as spaces; everything else is the same text.
    let collapse_all =
        |d: &Document| -> Vec<String> { paragraph_texts(d).iter().map(|t| collapse(t)).collect() };
    assert_eq!(collapse_all(&got), collapse_all(&want));
    same_formatting(&got, &want, "htm");
    let nums: Vec<i32> = paragraphs(&got)
        .iter()
        .filter_map(|p| p.props.num_id)
        .collect();
    assert_eq!(nums, [1, 1, 1, 2, 2], "bullets then decimals");
}

#[test]
fn word_pdf_text_matches_word_docx_in_reading_order() {
    let want = oracle();
    let got = import::import_pdf(&fixture("source.pdf")).expect("PDF imports");
    let collapsed = |d: &Document| -> Vec<String> {
        paragraph_texts(d)
            .iter()
            .map(|t| collapse(t))
            .filter(|t| !t.is_empty())
            .collect()
    };
    // The same words in the same order: a PDF has no paragraph or cell
    // boundaries of its own, so only white space may differ.
    assert_eq!(collapsed(&got).join(" "), collapsed(&want).join(" "));
    // Each oracle paragraph starts a paragraph of the PDF's text, or a
    // tab-separated column of one (table cells share a baseline).
    let starts: Vec<String> = paragraph_texts(&got)
        .iter()
        .flat_map(|t| t.split('\t').map(collapse).collect::<Vec<_>>())
        .collect();
    // Compared up to the oracle paragraph's own first tab or manual line
    // break, where the PDF splits it too.
    for w in paragraph_texts(&want) {
        let head = collapse(w.split(['\t', '\n']).next().unwrap_or(""));
        if head.is_empty() {
            continue;
        }
        assert!(
            starts.iter().any(|s| s.starts_with(head.as_str())),
            "{head:?} starts no paragraph in {starts:?}"
        );
    }
    // The list markers set the lists and are not text; the larger sizes
    // are the headings, the largest Heading 1.
    let nums: Vec<i32> = paragraphs(&got)
        .iter()
        .filter_map(|p| p.props.num_id)
        .collect();
    assert_eq!(nums, [1, 1, 1, 2, 2], "bullets then decimals");
    let heads: Vec<(u8, String)> = paragraphs(&got)
        .iter()
        .filter_map(|p| p.props.heading_level.map(|l| (l, p.plain_text())))
        .collect();
    assert_eq!(
        heads,
        [
            (1, "Import fixture".to_string()),
            (2, "Formatting".to_string()),
            (2, "Lists".to_string()),
            (2, "Table".to_string()),
        ]
    );
}

#[test]
fn half_of_word_docx_recovers_a_prefix_of_its_text() {
    let bytes = fixture("source.docx");
    let full: Vec<String> = paragraph_texts(&oracle());
    let cut = &bytes[..bytes.len() / 2];
    assert!(
        docxcore::package::load_package(cut).is_err(),
        "the cut file does not load"
    );
    let got = paragraph_texts(&import::recover_docx_text(cut).expect("text recovered"));
    assert!(!got.is_empty());
    let (last, whole) = got.split_last().unwrap();
    assert_eq!(whole, &full[..whole.len()]);
    assert!(full[whole.len()].starts_with(last.as_str()), "{last:?}");
}

#[test]
fn every_prefix_of_every_fixture_imports_without_panicking() {
    for (name, f) in [
        (
            "source.rtf",
            import::import_rtf as fn(&[u8]) -> Result<Document, String>,
        ),
        ("source.htm", import::import_html),
        ("source.pdf", import::import_pdf),
    ] {
        let bytes = fixture(name);
        for k in 0..=64 {
            let _ = f(&bytes[..bytes.len() * k / 64]);
        }
    }
    let docx = fixture("source.docx");
    for k in 0..=64 {
        let prefix = &docx[..docx.len() * k / 64];
        let _ = import::recover_docx_text(prefix);
        let _ = import::recover_any_text(prefix);
    }
}
