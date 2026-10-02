//! The Word 97-2003 import (#634) against Word itself: each `<stem>.doc` of
//! `corpus/legacy/word` must import to what Word read from it, which is
//! `<stem>.docx` (Word opened the `.doc` and saved it as `.docx`; see
//! `corpus/tools/gen_doc_corpus.ps1`). Compared:
//!
//! - (a) the paragraphs' text, in order (a table's cells flattened);
//! - (b) each paragraph's direct run formatting: bold, italic, underline,
//!   strike, size, font and colour, over its text (adjacent equal runs
//!   merged), as the runs state it, not as a style does;
//! - (c) each paragraph's alignment and heading level;
//! - (d) each table's rows and cells, and the cells' text;
//! - (e) (a)-(d) again after saving the import and loading it back, with
//!   `compatibilityMode` 11, and 15 after Convert's change.
//!
//! Every exception is an entry in [`ALLOW`], with its reason.

use docxcore::legacy::doc::import_doc;
use docxcore::load::field_result_props;
use docxcore::model::{Align, Block, Document, Inline, RunProps};
use docxcore::package::{Package, load_package, save_package};
use std::path::PathBuf;

/// The fixtures `gen_doc_corpus.ps1` makes; each must be there, so the test
/// can't pass by finding nothing.
const FIXTURES: &[&str] = &[
    "plain",
    "cp1252",
    "unicode",
    "formatting",
    "align-headings",
    "table",
    "fields-breaks",
];

/// What an allowlist entry exempts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Check {
    /// (b), the formatting of the runs.
    Runs,
}

/// One exemption: `check` is skipped for `file`, because `why`.
struct Allow {
    file: &'static str,
    check: Check,
    why: &'static str,
}

const ALLOW: &[Allow] = &[];

fn allowed(file: &str, check: Check) -> bool {
    ALLOW.iter().any(|a| a.file == file && a.check == check)
}

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../corpus/legacy/word")
}

/// Formatting compared in (b).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fmt {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    size: Option<u32>,
    font: Option<String>,
    color: Option<String>,
}

impl From<&RunProps> for Fmt {
    fn from(p: &RunProps) -> Fmt {
        Fmt {
            bold: p.bold,
            italic: p.italic,
            underline: p.underline,
            strike: p.strike,
            size: p.size_half_pts,
            font: p.font.clone(),
            color: p.color.clone(),
        }
    }
}

/// One paragraph as the test compares it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Para {
    text: String,
    align: Align,
    heading: Option<u8>,
    runs: Vec<(String, Fmt)>,
}

/// The document as the test compares it: its paragraphs in order (a
/// table's flattened cell by cell), and each table's cell texts.
#[derive(Debug, Default)]
struct Shape {
    paras: Vec<Para>,
    tables: Vec<Vec<Vec<String>>>,
}

fn shape(doc: &Document) -> Shape {
    let mut out = Shape::default();
    walk(&doc.body, &mut out);
    out
}

fn walk(blocks: &[Block], out: &mut Shape) {
    for b in blocks {
        match b {
            Block::Paragraph(p) => {
                let mut runs: Vec<(String, Fmt)> = Vec::new();
                for inline in &p.content {
                    let (text, fmt) = match inline {
                        Inline::Run(r) => (r.text.clone(), Fmt::from(&r.props)),
                        Inline::Field { raw, text } => {
                            (text.clone(), Fmt::from(&field_result_props(raw)))
                        }
                        Inline::Tab(props) => ("\t".into(), Fmt::from(props)),
                        Inline::Break(_, props) => ("\n".into(), Fmt::from(props)),
                        other => (other.text(), Fmt::from(&RunProps::default())),
                    };
                    if text.is_empty() {
                        continue;
                    }
                    match runs.last_mut() {
                        Some((t, f)) if *f == fmt => t.push_str(&text),
                        _ => runs.push((text, fmt)),
                    }
                }
                out.paras.push(Para {
                    text: p.plain_text(),
                    align: p.props.align,
                    heading: p.props.heading_level,
                    runs,
                });
            }
            Block::Table(t) => {
                let mut cells = Vec::new();
                for row in &t.rows {
                    let mut texts = Vec::new();
                    for cell in &row.cells {
                        texts.push(
                            cell.blocks
                                .iter()
                                .map(Block::plain_text)
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                        walk(&cell.blocks, out);
                    }
                    cells.push(texts);
                }
                out.tables.push(cells);
            }
            _ => {}
        }
    }
}

/// (a)-(d) for one file; the differences, one line each.
fn compare(file: &str, got: &Document, want: &Document) -> Vec<String> {
    let (got, want) = (shape(got), shape(want));
    let mut diffs = Vec::new();
    let texts = |s: &Shape| s.paras.iter().map(|p| p.text.clone()).collect::<Vec<_>>();
    if texts(&got) != texts(&want) {
        diffs.push(format!(
            "(a) text:\n  import {:?}\n  Word   {:?}",
            texts(&got),
            texts(&want)
        ));
        // Paragraph-by-paragraph checks mean nothing once the text is off.
        return diffs;
    }
    for (i, (g, w)) in got.paras.iter().zip(&want.paras).enumerate() {
        if g.runs != w.runs && !allowed(file, Check::Runs) {
            diffs.push(format!(
                "(b) paragraph {i} runs:\n  import {:?}\n  Word   {:?}",
                g.runs, w.runs
            ));
        }
        if (g.align, g.heading) != (w.align, w.heading) {
            diffs.push(format!(
                "(c) paragraph {i}: import {:?}/{:?}, Word {:?}/{:?}",
                g.align, g.heading, w.align, w.heading
            ));
        }
    }
    if got.tables != want.tables {
        diffs.push(format!(
            "(d) tables:\n  import {:?}\n  Word   {:?}",
            got.tables, want.tables
        ));
    }
    diffs
}

fn read(stem: &str, ext: &str) -> Vec<u8> {
    let path = corpus().join(format!("{stem}.{ext}"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn doc_imports_match_words_own_docx() {
    for a in ALLOW {
        println!("ALLOW {} {:?}: {}", a.file, a.check, a.why);
    }
    let mut failures = Vec::new();
    for stem in FIXTURES {
        let file = format!("{stem}.doc");
        let import = import_doc(&read(stem, "doc")).unwrap_or_else(|e| panic!("{file}: {e}"));
        let word: Package = load_package(&read(stem, "docx")).expect("Word's .docx loads");
        for diff in compare(&file, &import.document, &word.document) {
            failures.push(format!("{file} {diff}"));
        }

        // (e) The import saved and loaded back, in and out of Compatibility Mode.
        let saved = load_package(&save_package(&import)).expect("the saved import loads");
        assert_eq!(saved.compatibility_mode(), Some(11), "{file}");
        for diff in compare(&file, &saved.document, &word.document) {
            failures.push(format!("{file} (e) {diff}"));
        }
        let mut converted = import.clone();
        converted.set_compatibility_mode(15);
        let converted = load_package(&save_package(&converted)).unwrap();
        assert_eq!(converted.compatibility_mode(), Some(15), "{file}");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Damaged copies of real Word files import or fail cleanly, and quickly.
#[test]
fn damaged_corpus_documents_never_panic() {
    let started = std::time::Instant::now();
    for stem in ["plain", "table", "formatting"] {
        let good = read(stem, "doc");
        let mut state = 0x9E37_79B9u32 ^ good.len() as u32;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as usize
        };
        for round in 0..300 {
            let mut bytes = good.clone();
            if round % 3 == 0 {
                bytes.truncate(next() % bytes.len());
            } else {
                for _ in 0..8 {
                    let at = next() % bytes.len();
                    bytes[at] ^= 1 << (next() % 8);
                }
            }
            let _ = import_doc(&bytes);
        }
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(30));
}

/// The comparison means something only if Word's files hold what each
/// fixture is for: the formatting, headings, alignments, table and breaks.
#[test]
fn the_oracles_exercise_each_feature() {
    let word = |stem: &str| shape(&load_package(&read(stem, "docx")).unwrap().document);
    let runs: Vec<Fmt> = word("formatting")
        .paras
        .iter()
        .flat_map(|p| p.runs.iter().map(|(_, f)| f.clone()))
        .collect();
    assert!(runs.iter().any(|f| f.bold && !f.italic));
    assert!(runs.iter().any(|f| f.italic && !f.bold));
    assert!(runs.iter().any(|f| f.bold && f.italic));
    assert!(runs.iter().any(|f| f.underline));
    assert!(runs.iter().any(|f| f.strike));
    assert!(runs.iter().any(|f| f.size == Some(32)));
    assert!(runs.iter().any(|f| f.size == Some(16)));
    assert!(
        runs.iter()
            .any(|f| f.font.as_deref() == Some("Courier New"))
    );
    assert!(runs.iter().any(|f| f.color.as_deref() == Some("FF0000")));

    let aligned = word("align-headings");
    for level in 1..=3 {
        assert!(
            aligned.paras.iter().any(|p| p.heading == Some(level)),
            "{level}"
        );
    }
    for align in [Align::Center, Align::Right, Align::Justify] {
        assert!(aligned.paras.iter().any(|p| p.align == align), "{align:?}");
    }

    let table = word("table").tables;
    assert_eq!(table.len(), 1);
    assert_eq!(table[0].len(), 3);
    assert!(table[0].iter().all(|row| row.len() == 3));

    let breaks = word("fields-breaks");
    assert!(breaks.paras.iter().any(|p| p.text.contains(
        "Line one
line two	"
    )));
    assert!(
        breaks
            .paras
            .iter()
            .any(|p| p.text == "Page 1 of the document.")
    );

    assert!(word("unicode").paras.iter().any(|p| p.text.contains('😀')));
    assert!(word("cp1252").paras.iter().any(|p| p.text.contains('€')));
}
