//! `docxy` — terminal viewer/**editor** for `.docx`.
//!
//! Usage:
//!   docxy                          open the editor with a new blank document
//!   docxy <file.docx>              open in the editor
//!   docxy <file.docx> --pdf <out>  headless: export to PDF and exit
//!   docxy <in> --md <out.md>       headless: convert to Markdown and exit
//!   docxy <in> --docx <out.docx>   headless: convert to .docx and exit
//!   docxy <in> --html <out.docx.html>  headless: export editable HTML and exit
//!
//! The logic lives in the pure `docxcore` crate; this binary is the TUI shell:
//! it maps `docxcore::render` lines onto ratatui, draws a caret via the render
//! line-map, and routes keys into a `docxcore::editor::Editor`.

mod backstage;
mod bidi;
mod control;
mod html;
mod mcp;
mod metafile;
mod protection;
mod ribbon;
mod skill;
#[cfg(test)]
mod test_fixtures;
mod watermark;

use opccore::fsio::{export_atomic, write_atomic};
use std::path::Path;

use std::collections::HashMap;
use std::io;
use std::process::ExitCode;

// Bring the trait's methods (`extensions`, `default_save_name`, …) into scope
// for the `impl backstage::BackstageHost for App` call sites below.
use backstage::BackstageHost as _;

use docxcore::compare::{CompareOptions, CompareResult, CompareSkip, compare_packages};
use docxcore::editor::{Caret, Clip, Editor, FoundMatch};
use docxcore::export::{PdfOptions, to_pdf};
#[cfg(test)]
use docxcore::load::parse_header_footer;
use docxcore::load::{Relationships, parse_rels_xml};
use docxcore::markdown::{decode_markdown, from_markdown, to_markdown_with};
use docxcore::model::{
    Align, Block, BreakKind, Document, Hyperlink, Inline, PageGeom, PropertyScope, RevisionAddress,
    RevisionCategory, RevisionKind, Run, RunProps, UnsupportedRevisionKind,
};
use docxcore::numbering::{Numbering, compute_markers, parse_numbering_xml};
use docxcore::package::{
    Package, Protection, load_package, new_markdown_package, new_package, save_package,
    save_package_preserving_document,
};
use docxcore::page_bg::PageBackground;
use docxcore::render::{
    Color as DocColor, ImageBox, Line as DocLine, LineCaret, LineMap, PageParts, RenderOptions,
    Span as DocSpan, Style as DocStyle, render_with_images, render_with_page_layout,
};
use docxcore::review::{RevisionAction, RevisionOutcome};
use docxcore::sect::{BorderSide, PageBorders, PgBorderDisplay, PgBorderOffset};
use docxcore::serialize::blocks_to_xml;
use docxcore::styles::{StyleSheet, parse_styles_xml};
use docxcore::watermark::TextWatermarkSpec;
use std::rc::Rc;

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as RLine, Span as RSpan, Text};
use ratatui::widgets::{
    Block as RBlock, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Wrap,
};
use ratatui::{Frame, Terminal};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};

/// The on-disk format the open document is bound to. Drives load (`.docx` vs
/// Markdown), save, and whether the View ▸ Markdown source/rendered switch shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocFormat {
    Docx,
    Markdown,
    /// A `.docx` carried inside an editable-HTML bundle (`sample.docx.html`).
    /// It edits exactly like [`DocFormat::Docx`]; save rewraps the bundle.
    Html,
}

impl DocFormat {
    /// Whether the document is a Word package (plain or inside a bundle).
    fn is_docx(self) -> bool {
        matches!(self, DocFormat::Docx | DocFormat::Html)
    }
}

/// Pick a format from a path's extension: any `.html`/`.htm` is an editable-HTML
/// bundle (opened by its content, so `sample.docx (1).html` works, and never
/// written as anything else); Markdown for `.md`/`.markdown`/`.mdown`; `.docx`
/// otherwise.
fn format_for(path: &str) -> DocFormat {
    let lower = path.to_ascii_lowercase();
    if htmlbundle::is_html_path(&lower) {
        DocFormat::Html
    } else if lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".mdown") {
        DocFormat::Markdown
    } else {
        DocFormat::Docx
    }
}

/// The terminal window title: `* AppName - filename` (the `* ` only when the
/// document has unsaved changes).
fn window_title(app: &str, path: &str, dirty: bool) -> String {
    let name = std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    format!("{}{app} - {name}", if dirty { "* " } else { "" })
}

/// A loaded input file: the package, the format it was read as, and for an
/// editable-HTML bundle the opened bundle itself (its HTML, kept so save can
/// rewrap it; the embedded package's exact bytes; its recorded name; and the
/// "changed since export" warning).
struct Input {
    pkg: Package,
    format: DocFormat,
    bundle: Option<html::Opened>,
    encoding: Option<&'static str>,
}

/// Load a file into a package: Markdown parsed into a numbered package, an
/// editable-HTML file by its embedded package, `.docx` otherwise.
fn load_input(path: &str) -> Result<Input, String> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".xlsx")
        || lower.ends_with(".xls")
        || htmlbundle::bundle_inner_ext(&lower).is_some_and(|e| e == "xlsx" || e == "xls")
    {
        return Err(format!(
            "{path} is a spreadsheet, not a document — try: xlsxy {path}"
        ));
    }
    let format = format_for(path);
    if format == DocFormat::Html {
        let opened = html::open(path)?;
        let pkg = load_package(&opened.docx).map_err(|e| format!("{path}: {e}"))?;
        return Ok(Input {
            pkg,
            format,
            bundle: Some(opened),
            encoding: None,
        });
    }
    let data = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let (pkg, encoding) = if format == DocFormat::Markdown {
        let (text, encoding) = decode_markdown(&data).map_err(|e| format!("{path}: {e}"))?;
        (new_markdown_package(from_markdown(&text)), encoding)
    } else {
        (load_package(&data).map_err(|e| e.to_string())?, None)
    };
    Ok(Input {
        pkg,
        format,
        bundle: None,
        encoding,
    })
}

/// Turn Markdown text into a document of literal lines: one paragraph per line,
/// each holding the line verbatim (no inline parsing). This is the editable buffer
/// for Markdown *source* view; toggling back to rendered re-parses it.
fn source_lines_to_doc(md: &str) -> Document {
    use docxcore::model::{Inline, ParProps, Paragraph, Run, RunProps};
    let mut body: Vec<Block> = md
        .trim_end_matches('\n')
        .split('\n')
        .map(|line| {
            let content = if line.is_empty() {
                Vec::new()
            } else {
                vec![Inline::Run(Run {
                    text: line.to_string(),
                    props: RunProps::default(),
                })]
            };
            Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content,
            })
        })
        .collect();
    if body.is_empty() {
        body.push(Block::Paragraph(docxcore::model::Paragraph::default()));
    }
    Document { body }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--mcp` runs the headless MCP stdio bridge (a client of a running docxy),
    // not the editor, so handle it before the file-oriented argument parsing.
    if args.iter().any(|a| a == "--mcp") {
        return match mcp::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("mcp: {e}");
                ExitCode::FAILURE
            }
        };
    }
    // `docxy install skill` writes the agent SKILL.md and exits.
    if args.first().map(String::as_str) == Some("install")
        && args.get(1).map(String::as_str) == Some("skill")
    {
        return match skill::install() {
            Ok(msg) => {
                println!("{msg}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("install skill: {e}");
                ExitCode::FAILURE
            }
        };
    }
    // `docxy compare <original> <revised> -o <out>` writes a Review ▸ Compare
    // result headless and exits.
    if args.first().map(String::as_str) == Some("compare") && args.len() > 1 {
        return compare_cli(&args[1..]);
    }
    // `docxy merge <main.docx> <data.csv> -o <out>` runs a mail merge (#628)
    // headless and exits.
    if args.first().map(String::as_str) == Some("merge") && args.len() > 1 {
        return merge_cli(&args[1..]);
    }
    let parsed = match parse_args(&args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            print_usage();
            return ExitCode::from(2);
        }
    };
    if parsed.help {
        print_usage();
        return ExitCode::SUCCESS;
    }
    // No file argument → open on the welcome screen instead of a blank document.
    let start = parsed.input.is_none();
    // With a file argument we load it (Markdown or .docx, by extension); with none,
    // start a fresh blank document.
    let (loaded, input) = match parsed.input {
        Some(input) => match load_input(&input) {
            Ok(loaded) => (loaded, input),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            if parsed.pdf_out.is_some()
                || parsed.md_out.is_some()
                || parsed.docx_out.is_some()
                || parsed.html_out.is_some()
            {
                eprintln!(
                    "error: headless conversion (--pdf/--md/--docx/--html) requires an input file"
                );
                return ExitCode::from(2);
            }
            let pkg = new_package(Document {
                body: vec![Block::Paragraph(docxcore::model::Paragraph::default())],
            });
            let loaded = Input {
                pkg,
                format: DocFormat::Docx,
                bundle: None,
                encoding: None,
            };
            (loaded, "untitled.docx".to_string())
        }
    };
    let format = loaded.format;

    if let Some(out) = parsed.html_out {
        return match export_html_headless(&loaded, &input, &out) {
            Ok(len) => {
                println!("wrote {out} ({len} bytes)");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    // A bundle's embedded package is the original file: `--docx` hands it back
    // byte for byte instead of re-serializing it.
    if let (Some(out), Some(bundle)) = (&parsed.docx_out, &loaded.bundle) {
        return match export_atomic(Some(Path::new(&input)), Path::new(out), &bundle.docx) {
            Ok(()) => {
                println!("wrote {out} ({} bytes)", bundle.docx.len());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: cannot write {out}: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let pkg = loaded.pkg;

    let conversion = parsed
        .pdf_out
        .map(|out| (HeadlessFormat::Pdf, out))
        .or_else(|| parsed.md_out.map(|out| (HeadlessFormat::Markdown, out)))
        .or_else(|| parsed.docx_out.map(|out| (HeadlessFormat::Docx, out)));
    if let Some((kind, out)) = conversion {
        return match convert_headless(&pkg, &input, &out, kind) {
            Ok(len) => {
                println!("wrote {out} ({len} bytes)");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: cannot write {out}: {e}");
                ExitCode::FAILURE
            }
        };
    }

    match run_tui(
        pkg,
        &input,
        format,
        loaded.bundle,
        loaded.encoding,
        parsed.vim,
        start,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Clone, Copy)]
enum HeadlessFormat {
    Pdf,
    Markdown,
    Docx,
}

fn convert_headless(
    pkg: &Package,
    source: &str,
    out: &str,
    kind: HeadlessFormat,
) -> io::Result<usize> {
    let bytes = match kind {
        HeadlessFormat::Pdf => {
            let styles = pkg
                .part("word/styles.xml")
                .map(|b| parse_styles_xml(std::str::from_utf8(b).unwrap_or("")))
                .unwrap_or_default();
            to_pdf(
                &pkg.document,
                &PdfOptions::from_package(pkg, Rc::new(styles)),
            )
        }
        HeadlessFormat::Markdown => {
            let numbering = pkg
                .part("word/numbering.xml")
                .map(|b| parse_numbering_xml(std::str::from_utf8(b).unwrap_or("")))
                .unwrap_or_default();
            let markers = compute_markers(&pkg.document, &numbering);
            to_markdown_with(&pkg.document, &markers).into_bytes()
        }
        HeadlessFormat::Docx => save_package(pkg),
    };
    export_atomic(Some(Path::new(source)), Path::new(out), &bytes)?;
    Ok(bytes.len())
}

/// `--html`: wrap the input as a new editable-HTML bundle. The embedded
/// package is the input's own bytes where it has them (a `.docx`, or a
/// bundle's payload), so export never re-serializes a Word file.
fn export_html_headless(loaded: &Input, source: &str, out: &str) -> Result<usize, String> {
    let (name, docx) = if let Some(bundle) = &loaded.bundle {
        (bundle.source_name.clone(), bundle.docx.clone())
    } else if loaded.format == DocFormat::Docx {
        let bytes = std::fs::read(source).map_err(|e| format!("cannot read {source}: {e}"))?;
        (html::file_name(source), bytes)
    } else {
        let stem = Path::new(source)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document".into());
        (format!("{stem}.docx"), save_package(&loaded.pkg))
    };
    // Replace nothing but a missing file or a Word bundle.
    htmlbundle::check_html_target(Path::new(out))?;
    let page = html::export(&name, &docx)?;
    export_atomic(Some(Path::new(source)), Path::new(out), page.as_bytes())
        .map_err(|e| format!("cannot write {out}: {e}"))?;
    Ok(page.len())
}

/// The reviewer name when the OS gives none ([`App::review_author`]), and
/// the author `docxy compare` attributes its revisions to.
const DEFAULT_AUTHOR: &str = "docxy";

/// The comment ids a freshly loaded document holds: its comments' and its
/// body markers' ([`App::used_comment_ids`]).
fn used_comment_ids(
    comments: &[docxcore::comments::Comment],
    doc: &Document,
) -> std::collections::BTreeSet<String> {
    let mut used: std::collections::BTreeSet<String> =
        comments.iter().map(|c| c.id.clone()).collect();
    used.extend(docxcore::inspect::comment_marker_ids(doc));
    used
}

/// The OS account name (`USERNAME` on Windows, `USER` elsewhere), if set.
fn os_user_name() -> Option<String> {
    ["USERNAME", "USER"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|v| v.trim().to_string())
        .find(|v| !v.is_empty())
}

/// Now as a UTC ISO-8601 `w:date` (`YYYY-MM-DDTHH:MM:SSZ`).
fn utc_now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    docxcore::field::format_iso(&docxcore::field::civil_from_unix(secs))
}

/// Review ▸ Compare over two `.docx` files: the result's tracked changes turn
/// `original` into `revised`. Neither file is written.
fn compare_files(original: &str, revised: &str, author: &str) -> Result<CompareResult, String> {
    let load = |path: &str| -> Result<Package, String> {
        if !path.to_ascii_lowercase().ends_with(".docx") {
            return Err(format!(
                "{path} is not a .docx file (compare takes Word documents)"
            ));
        }
        let data = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        load_package(&data).map_err(|e| format!("{path}: {e}"))
    };
    let (original, revised) = (load(original)?, load(revised)?);
    let opts = CompareOptions {
        author: author.to_string(),
        date: utc_now_iso(),
    };
    Ok(compare_packages(&original, &revised, &opts))
}

/// `3 insertions, 2 deletions`, plus `; skipped: 1 table, 2 object` when the
/// comparison left anything out.
fn compare_summary(insertions: usize, deletions: usize, skipped: &[CompareSkip]) -> String {
    let mut summary = format!("{insertions} insertions, {deletions} deletions");
    let counts = docxcore::compare::skip_counts(skipped);
    if !counts.is_empty() {
        let parts: Vec<String> = counts.iter().map(|(k, n)| format!("{n} {k}")).collect();
        summary.push_str(&format!("; skipped: {}", parts.join(", ")));
    }
    summary
}

/// Where a terminal compare result is titled: `Compare Result N.docx` next to
/// the revised file, with the smallest N not already on disk.
fn next_compare_result_path(revised: &Path) -> String {
    let dir = revised.parent().filter(|d| !d.as_os_str().is_empty());
    (1..)
        .map(|n| {
            let name = format!("Compare Result {n}.docx");
            dir.map_or_else(|| std::path::PathBuf::from(&name), |d| d.join(&name))
        })
        .find(|candidate| !candidate.exists())
        .expect("an unused compare result name")
        .display()
        .to_string()
}

/// The two input paths and the `.docx` output of a headless subcommand
/// (`docxy <cmd> <a> <b> -o <out.docx>`), or the exit code it ends with:
/// success after `--help`, 2 for a usage error, failure when the output
/// already exists. `what` names the output in the errors ("the compare
/// result"), `cmd` the subcommand.
fn two_inputs_and_out(
    args: &[String],
    usage: &str,
    what: &str,
    cmd: &str,
) -> Result<(String, String, String), ExitCode> {
    let mut inputs = Vec::new();
    let mut out = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                i += 1;
                match args.get(i) {
                    Some(path) => out = Some(path.clone()),
                    None => {
                        eprintln!("error: {} requires an output path\n{usage}", args[i - 1]);
                        return Err(ExitCode::from(2));
                    }
                }
            }
            "-h" | "--help" => {
                println!("{usage}");
                return Err(ExitCode::SUCCESS);
            }
            path => inputs.push(path.to_string()),
        }
        i += 1;
    }
    let (Some(out), [a, b]) = (out, inputs.as_slice()) else {
        eprintln!("{usage}");
        return Err(ExitCode::from(2));
    };
    if !out.to_ascii_lowercase().ends_with(".docx") {
        eprintln!("error: {out}: {what} must be a .docx file");
        return Err(ExitCode::from(2));
    }
    if Path::new(&out).exists() {
        eprintln!("error: {out} already exists ({cmd} never overwrites a file)");
        return Err(ExitCode::FAILURE);
    }
    Ok((a.clone(), b.clone(), out))
}

/// Write a headless subcommand's result to `out`, which must not exist:
/// `create_atomic` refuses any existing destination, so a file that appeared
/// since [`two_inputs_and_out`] checked is refused too.
fn write_new(out: &str, bytes: &[u8], cmd: &str) -> Result<(), ExitCode> {
    match opccore::fsio::create_atomic(Path::new(out), bytes) {
        Ok(()) => Ok(()),
        Err(e) => {
            if e.kind() == io::ErrorKind::AlreadyExists {
                eprintln!("error: {out} already exists ({cmd} never overwrites a file)");
            } else {
                eprintln!("error: cannot write {out}: {e}");
            }
            Err(ExitCode::FAILURE)
        }
    }
}

/// `docxy compare <original.docx> <revised.docx> -o <out.docx>`: write the
/// comparison as a new file. Never replaces an existing file.
fn compare_cli(args: &[String]) -> ExitCode {
    const USAGE: &str = "usage: docxy compare <original.docx> <revised.docx> -o <out.docx>";
    let (original, revised, out) =
        match two_inputs_and_out(args, USAGE, "the compare result", "compare") {
            Ok(v) => v,
            Err(code) => return code,
        };
    let result = match compare_files(&original, &revised, DEFAULT_AUTHOR) {
        Ok(result) => result,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(code) = write_new(&out, &save_package(&result.package), "compare") {
        return code;
    }
    println!(
        "wrote {out} ({} insertions, {} deletions)",
        result.insertions, result.deletions
    );
    for (kind, n) in docxcore::compare::skip_counts(&result.skipped) {
        println!("skipped: {n} {kind}");
    }
    ExitCode::SUCCESS
}

/// `docxy merge <main.docx> <data.csv> -o <out.docx>`: merge the main
/// document with every recipient in the CSV (Mailings ▸ Finish & Merge ▸ Edit
/// Individual Documents) into a new file. Never replaces an existing file.
fn merge_cli(args: &[String]) -> ExitCode {
    const USAGE: &str = "usage: docxy merge <main.docx> <data.csv> -o <out.docx>";
    let (main, data, out) = match two_inputs_and_out(args, USAGE, "the merged document", "merge") {
        Ok(v) => v,
        Err(code) => return code,
    };
    let pkg = match std::fs::read(&main)
        .map_err(|e| e.to_string())
        .and_then(|b| load_package(&b).map_err(|e| e.to_string()))
    {
        Ok(pkg) => pkg,
        Err(e) => {
            eprintln!("error: cannot open {main}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let recipients = match std::fs::read(&data)
        .map_err(|e| e.to_string())
        .and_then(|b| docxcore::merge::Recipients::parse_csv(&b))
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: cannot read the recipient list {data}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let opts = docxcore::merge::MergeOptions {
        range: docxcore::merge::MergeRange::All,
        doc_type: pkg.mail_merge().map(|m| m.doc_type).unwrap_or_default(),
        map: docxcore::merge::FieldMap::auto(&recipients),
    };
    let merged = match docxcore::merge::merge_package(&pkg, &recipients, &opts) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {data}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(code) = write_new(&out, &save_package(&merged), "merge") {
        return code;
    }
    let n = docxcore::merge::merge_rows(&recipients, opts.range).len();
    println!("wrote {out} ({n} records)");
    ExitCode::SUCCESS
}

fn print_usage() {
    eprintln!(
        "Docxy — terminal .docx & Markdown editor\n\n\
         USAGE:\n  \
           docxy                           welcome screen (new .docx/.md, open)\n  \
           docxy <file.docx|.md>           open a Word or Markdown file\n  \
           docxy <file> --vim              open with vim keybindings\n  \
           docxy <file> --pdf <out>        export to PDF and exit\n  \
           docxy <file> --md <out.md>      convert to Markdown and exit\n  \
           docxy <file> --docx <out.docx>  convert to Word .docx and exit\n  \
           docxy <file> --html <out.docx.html>  export as editable HTML and exit\n  \
           docxy compare <orig.docx> <rev.docx> -o <out.docx>\n  \
                                           write a tracked-changes comparison and exit\n  \
           docxy merge <main.docx> <data.csv> -o <out.docx>\n  \
                                           mail-merge every CSV recipient and exit\n  \
           docxy --mcp                      run the MCP bridge to drive a live docxy\n  \
           docxy install skill              install the agent SKILL.md (self-onboarding)\n  \
           (Save As to a .md/.docx/.docx.html name converts between the formats;\n   \
            View ▸ Markdown switches a .md between rendered and source)\n\n\
         EDITOR KEYS:\n  \
           type / Enter / Backspace / Delete    edit text\n  \
           arrows / Home / End / PgUp / PgDn     move   (Ctrl-←/→ by word)\n  \
           Shift + move                          select   (Esc clears)\n  \
           Ctrl-B bold  Ctrl-I italic  Ctrl-U underline   (over selection)\n  \
           Ctrl-L/E/R/J align left / center / right / justify\n  \
           Ctrl-A select all   Ctrl-C copy   Ctrl-X cut   Ctrl-V paste\n  \
           Ctrl-F find   Ctrl-H replace   Ctrl-Shift-8 show marks\n  \
           Ctrl-S save   Ctrl-Z undo   Ctrl-Y redo   Ctrl-Q quit\n  \
           F9 ribbon (←→ tabs · ↓ enter · arrows move · Enter apply · Esc leave)\n  \
           F2 page view   F3 show marks   F4 table borders\n  \
           F6 edit header   F7 edit footer   (Esc returns)   F8 section break\n  \
           mouse: click to move · click ribbon buttons · click a link · wheel to scroll"
    );
}

struct Parsed {
    input: Option<String>,
    pdf_out: Option<String>,
    md_out: Option<String>,
    docx_out: Option<String>,
    html_out: Option<String>,
    help: bool,
    vim: bool,
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut input = None;
    let mut pdf_out = None;
    let mut md_out = None;
    let mut docx_out = None;
    let mut html_out = None;
    let mut help = false;
    let mut vim = false;
    let mut i = 0;
    // Read the path argument following a flag like `--pdf`, erroring if missing.
    let value = |i: &mut usize, flag: &str| -> Result<String, String> {
        *i += 1;
        args.get(*i)
            .cloned()
            .ok_or_else(|| format!("error: {flag} requires an output path"))
    };
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => help = true,
            "--vim" => vim = true,
            "--pdf" => pdf_out = Some(value(&mut i, "--pdf")?),
            "--md" => md_out = Some(value(&mut i, "--md")?),
            "--docx" => docx_out = Some(value(&mut i, "--docx")?),
            "--html" => html_out = Some(value(&mut i, "--html")?),
            s if s.starts_with('-') => return Err(format!("error: unknown option {s}")),
            s => {
                if input.is_some() {
                    return Err(format!("error: unexpected extra argument {s}"));
                }
                input = Some(s.to_string());
            }
        }
        i += 1;
    }
    Ok(Parsed {
        input,
        pdf_out,
        md_out,
        docx_out,
        html_out,
        help,
        vim,
    })
}

// ---- TUI ----

/// State for the find / replace bar.
struct FindState {
    query: String,
    /// `None` = find-only; `Some` = replace mode (the replacement text).
    replacement: Option<String>,
    editing_replacement: bool,
    matches: Vec<FoundMatch>,
    idx: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VimMode {
    Normal,
    Insert,
    Visual,
    VisualLine,
}

/// Modal-editing (vim) state.
struct VimState {
    mode: VimMode,
    count: String,
    pending_op: Option<char>, // 'd' | 'c' | 'y'
    pending_g: bool,
    cmdline: Option<String>, // Some while typing a `:` command
    last_search: String,
    linewise_clip: bool,
}

impl VimState {
    fn new() -> Self {
        VimState {
            mode: VimMode::Normal,
            count: String::new(),
            pending_op: None,
            pending_g: false,
            cmdline: None,
            last_search: String::new(),
            linewise_clip: false,
        }
    }
    fn take_count(&mut self) -> usize {
        let n = self.count.parse::<usize>().unwrap_or(1).max(1);
        self.count.clear();
        n
    }
    fn reset_pending(&mut self) {
        self.count.clear();
        self.pending_op = None;
        self.pending_g = false;
    }
}

/// Cached render state for one embedded image. The source is scaled once to the
/// box's full pixel size; as the box scrolls we crop the visible pixel window so
/// the image is *cut* at the viewport edge instead of squashed.
struct ImgState {
    /// Source scaled to the full interior box (`box_cols*fw` × `box_rows*fh` px).
    resized: image::DynamicImage,
    box_cols: usize,
    box_rows: usize,
    /// The window currently encoded into `proto`: (top cell, height cells, width cells).
    win: (usize, usize, usize),
    /// Pre-encoded image for the current window. Encoded once per window (not per
    /// position), so re-emitting it while scrolling/settling is cheap.
    proto: Protocol,
}

/// Active focus-edit of a header/footer: the body editor is parked here while the
/// main editor temporarily operates on the header/footer document.
struct HfEdit {
    body: Editor,
    is_header: bool,
    part: String,
    saved_page_view: bool,
}

/// A comment whose record follows its markers ([`App::tracked_comments`]).
#[derive(Clone, Debug)]
struct TrackedComment {
    comment: docxcore::comments::Comment,
    /// Its `<w:comment>` exactly as `pkg` held it when Delete Comment took
    /// its markers, written back as is when an undo restores them: a
    /// loaded comment's paragraphs, formatting and `w14:paraId` survive.
    /// `None` while it has not been deleted, or when `pkg` held no XML for
    /// it then (added and never saved): a restore then writes it from
    /// `comment`, which needs a numeric id (new comments always have one;
    /// any other is not written back).
    raw: Option<String>,
    /// Where it sat in `comments`, so a restored one goes back there.
    index: usize,
}

/// What a confirmed (Yes) modal should do.
#[derive(Clone, PartialEq, Eq, Debug)]
enum ConfirmAction {
    Exit,
    /// Overwrite an existing PDF at this path during Export.
    OverwritePdf(std::path::PathBuf),
    /// Apply one undoable action to every tracked change in the document.
    ReviewAll(RevisionAction),
    /// Discard unsaved changes and open a Review ▸ Compare result.
    Compare {
        original: String,
        revised: String,
    },
    /// Edit Anyway on a document marked as final (#617).
    EditAnyway,
}

/// The question a refused edit asks on a document marked as final.
const MARKED_FINAL_PROMPT: &str =
    "An author has marked this document as final to discourage editing. Edit anyway?";

// The Yes/No modal itself lives in `backstage::Confirm<ConfirmAction>` (shared
// across all apps); docxy only supplies the action carried on Yes.

pub(crate) fn property_scope_name(scope: PropertyScope) -> &'static str {
    match scope {
        PropertyScope::Run => "run properties",
        PropertyScope::Paragraph => "paragraph properties",
        PropertyScope::Table => "table properties",
        PropertyScope::TableRow => "row properties",
        PropertyScope::TableCell => "cell properties",
        PropertyScope::Section => "section properties",
    }
}

pub(crate) fn unsupported_revision_name(kind: &UnsupportedRevisionKind) -> String {
    use UnsupportedRevisionKind::*;
    match kind {
        MoveFrom => "move-from".to_string(),
        MoveTo => "move-to".to_string(),
        MoveFromRangeStart => "move-from range start".to_string(),
        MoveFromRangeEnd => "move-from range end".to_string(),
        MoveToRangeStart => "move-to range start".to_string(),
        MoveToRangeEnd => "move-to range end".to_string(),
        CustomXmlInsRangeStart => "custom-XML insertion range start".to_string(),
        CustomXmlInsRangeEnd => "custom-XML insertion range end".to_string(),
        CustomXmlDelRangeStart => "custom-XML deletion range start".to_string(),
        CustomXmlDelRangeEnd => "custom-XML deletion range end".to_string(),
        CustomXmlMoveFromRangeStart => "custom-XML move-from range start".to_string(),
        CustomXmlMoveFromRangeEnd => "custom-XML move-from range end".to_string(),
        CustomXmlMoveToRangeStart => "custom-XML move-to range start".to_string(),
        CustomXmlMoveToRangeEnd => "custom-XML move-to range end".to_string(),
        CellInsert => "cell insertion".to_string(),
        CellDelete => "cell deletion".to_string(),
        CellMerge => "cell merge".to_string(),
        ConflictInsert => "conflict insertion".to_string(),
        ConflictDelete => "conflict deletion".to_string(),
        Other(name) => name.clone(),
    }
}

pub(crate) fn revision_category_name(category: &RevisionCategory) -> String {
    match category {
        RevisionCategory::Inline(RevisionKind::Insert) => "insertion".to_string(),
        RevisionCategory::Inline(RevisionKind::Delete) => "deletion".to_string(),
        RevisionCategory::ParagraphMark(RevisionKind::Insert) => {
            "paragraph mark insertion".to_string()
        }
        RevisionCategory::ParagraphMark(RevisionKind::Delete) => {
            "paragraph mark deletion".to_string()
        }
        RevisionCategory::Property(scope) => property_scope_name(*scope).to_string(),
        RevisionCategory::Unsupported(kind) => {
            format!("unsupported {}", unsupported_revision_name(kind))
        }
    }
}

fn revision_status(address: &RevisionAddress, total: usize) -> String {
    let mut details = vec![format!("target {}", address.target.0)];
    if let Some(id) = address.metadata.id.as_deref() {
        details.push(format!("id {id}"));
    }
    if let Some(author) = address.metadata.author.as_deref() {
        details.push(format!("author {author}"));
    }
    if let Some(date) = address.metadata.date.as_deref() {
        details.push(format!("date {date}"));
    }
    format!(
        "Change {}/{}: {} ({})",
        address.ordinal + 1,
        total,
        revision_category_name(&address.category),
        details.join(" · ")
    )
}

/// One way to paste the clipboard, offered by the Paste Special dialog.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PasteOpt {
    /// Paste docxy's own copied content with its original formatting.
    KeepSource,
    /// Paste the text, adopting the formatting where it is dropped.
    Merge,
    /// Paste plain text with no formatting at all.
    Unformatted,
    /// Paste the text as a hyperlink to its own address (URLs only).
    Hyperlink,
}

impl PasteOpt {
    fn label(self) -> &'static str {
        match self {
            PasteOpt::KeepSource => "Keep Source Formatting",
            PasteOpt::Merge => "Merge Formatting",
            PasteOpt::Unformatted => "Unformatted Text",
            PasteOpt::Hyperlink => "Paste as Hyperlink",
        }
    }
    /// The "Result" description, like Word's box.
    fn result(self) -> &'static str {
        match self {
            PasteOpt::KeepSource => {
                "Inserts the clipboard contents keeping their original formatting."
            }
            PasteOpt::Merge => "Inserts the text and adopts the formatting of where it is pasted.",
            PasteOpt::Unformatted => {
                "Inserts the clipboard contents as text without any formatting."
            }
            PasteOpt::Hyperlink => "Inserts the text as a hyperlink to its address.",
        }
    }
}

/// A field type offered by the Insert Field dialog.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FieldKind {
    Date,
    Time,
    Page,
    NumPages,
    Author,
    Title,
    Subject,
    FileName,
}

impl FieldKind {
    const ALL: [FieldKind; 8] = [
        FieldKind::Date,
        FieldKind::Time,
        FieldKind::Page,
        FieldKind::NumPages,
        FieldKind::Author,
        FieldKind::Title,
        FieldKind::Subject,
        FieldKind::FileName,
    ];
    fn label(self) -> &'static str {
        match self {
            FieldKind::Date => "Date",
            FieldKind::Time => "Time",
            FieldKind::Page => "Page Number",
            FieldKind::NumPages => "Number of Pages",
            FieldKind::Author => "Author",
            FieldKind::Title => "Title",
            FieldKind::Subject => "Subject",
            FieldKind::FileName => "File Name",
        }
    }
    /// The field instruction (entity-decoded), e.g. `DATE \@ "M/d/yyyy"`.
    fn instr(self) -> &'static str {
        match self {
            FieldKind::Date => "DATE \\@ \"M/d/yyyy\"",
            FieldKind::Time => "TIME \\@ \"h:mm AM/PM\"",
            FieldKind::Page => "PAGE",
            FieldKind::NumPages => "NUMPAGES",
            FieldKind::Author => "AUTHOR",
            FieldKind::Title => "TITLE",
            FieldKind::Subject => "SUBJECT",
            FieldKind::FileName => "FILENAME",
        }
    }
    /// Value used when the field can't be computed here (no clock/metadata/pages).
    fn fallback(self) -> &'static str {
        match self {
            FieldKind::Page | FieldKind::NumPages => "1",
            _ => "",
        }
    }
}

/// The modal Insert Field dialog: pick a field to insert at the caret.
struct InsertFieldDialog {
    sel: usize,
}

/// The Paragraph dialog: precise left indent plus a first-line / hanging
/// "special" indent. Values are twips; rows are adjusted with ←/→ (steppers).
struct ParagraphDialog {
    left: i32,   // left indent (>= 0)
    special: u8, // 0 = none, 1 = first line, 2 = hanging
    by: i32,     // the first-line/hanging amount (>= 0)
    sel: usize,  // focused row: 0 = left, 1 = special, 2 = by
}

impl ParagraphDialog {
    const ROWS: usize = 3;
    const STEP: i32 = 360; // 0.25"

    /// The signed first-line delta this dialog represents.
    fn first_line(&self) -> i32 {
        match self.special {
            1 => self.by,
            2 => -self.by,
            _ => 0,
        }
    }

    /// Adjust the focused row by `dir` (+1 / -1).
    fn adjust(&mut self, dir: i32) {
        match self.sel {
            0 => self.left = (self.left + dir * Self::STEP).max(0),
            1 => self.special = (self.special as i32 + dir).rem_euclid(3) as u8,
            _ => self.by = (self.by + dir * Self::STEP).max(0),
        }
    }
}

/// The Review ▸ Compare dialog: the original and revised `.docx` paths.
struct CompareDialog {
    original: String,
    revised: String,
    /// Focused field: 0 = original, 1 = revised.
    field: usize,
}

impl CompareDialog {
    fn focused(&mut self) -> &mut String {
        if self.field == 0 {
            &mut self.original
        } else {
            &mut self.revised
        }
    }
}

/// What a Review ▸ Compare produced (see [`App::compare_paths`]).
pub(crate) struct CompareSummary {
    pub path: String,
    pub insertions: usize,
    pub deletions: usize,
    pub skipped: Vec<CompareSkip>,
}

/// Format twips as inches for display, e.g. 720 → `0.50"`.
fn twips_in(tw: i32) -> String {
    format!("{:.2}\"", tw as f32 / 1440.0)
}

/// The Apply-Styles dialog: a scrollable list of every paragraph style the
/// document defines, applied to the selected paragraph(s) on Enter.
struct StylesDialog {
    /// `(styleId, display name)` pairs, sorted by name.
    items: Vec<(String, String)>,
    sel: usize,
    /// Index of the first visible row (scroll offset), maintained by the drawer.
    top: usize,
}

/// What a [`Picker`] sets on the selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PickerKind {
    FontName,
    FontSize,
    FontColor,
    Highlight,
    Symbol,
    LineSpacing,
    Equation,
    PageColor,
    Watermark,
    PageBorders,
}

/// Common equation templates for Insert ▸ Equation: (label, LaTeX). The engine
/// turns the LaTeX into real Word OMML on insert.
const EQUATIONS: &[(&str, &str)] = &[
    ("x\u{00B2}", "x^2"),
    ("a\u{207F}", "a^{n}"),
    ("a/b", "\\frac{a}{b}"),
    ("\u{221A}x", "\\sqrt{x}"),
    ("\u{03A3}", "\\sum_{i=1}^{n} i"),
    ("\u{222B}", "\\int_{a}^{b} f(x)\\,dx"),
    ("lim", "\\lim_{x \\to \\infty} f(x)"),
    ("\u{03B1}\u{03B2}\u{03B3}", "\\alpha\\beta\\gamma"),
    ("a\u{00B2}+b\u{00B2}=c\u{00B2}", "a^2 + b^2 = c^2"),
    ("Quadratic", "x = \\frac{-b \\pm \\sqrt{b^2 - 4ac}}{2a}"),
];

/// Design ▸ Page Color's palette (the suite's `design_tab.rs` palette): the
/// Office theme's ten base colours, then Word's ten standard ones, as
/// (label, RRGGBB). The labels back both the picker items and the status line.
const PAGE_COLORS: [(&str, u32); 20] = [
    ("White, Background 1", 0xFFFFFF),
    ("Black, Text 1", 0x000000),
    ("Gray, Background 2", 0xE7E6E6),
    ("Blue-Gray, Text 2", 0x44546A),
    ("Blue, Accent 1", 0x4472C4),
    ("Orange, Accent 2", 0xED7D31),
    ("Gray, Accent 3", 0xA5A5A5),
    ("Gold, Accent 4", 0xFFC000),
    ("Blue, Accent 5", 0x5B9BD5),
    ("Green, Accent 6", 0x70AD47),
    ("Dark Red", 0xC00000),
    ("Red", 0xFF0000),
    ("Orange", 0xFFC000),
    ("Yellow", 0xFFFF00),
    ("Light Green", 0x92D050),
    ("Green", 0x00B050),
    ("Light Blue", 0x00B0F0),
    ("Blue", 0x0070C0),
    ("Dark Blue", 0x002060),
    ("Purple", 0x7030A0),
];

/// Word's watermark gallery (the suite's `design_tab.rs` PRESETS): each text
/// diagonal (1) and horizontal (2), as (label, text, diagonal).
const WATERMARK_PRESETS: [(&str, &str, bool); 12] = [
    ("CONFIDENTIAL 1", "CONFIDENTIAL", true),
    ("CONFIDENTIAL 2", "CONFIDENTIAL", false),
    ("DO NOT COPY 1", "DO NOT COPY", true),
    ("DO NOT COPY 2", "DO NOT COPY", false),
    ("DRAFT 1", "DRAFT", true),
    ("DRAFT 2", "DRAFT", false),
    ("SAMPLE 1", "SAMPLE", true),
    ("SAMPLE 2", "SAMPLE", false),
    ("ASAP 1", "ASAP", true),
    ("ASAP 2", "ASAP", false),
    ("URGENT 1", "URGENT", true),
    ("URGENT 2", "URGENT", false),
];

/// Design ▸ Page Border's fixed choices: None removes `w:pgBorders`; Box and
/// Shadow write a single-line box (Word's default 24pt from the page edge).
const PAGE_BORDER_ITEMS: &[&str] = &["None", "Box", "Shadow"];

/// [`PAGE_COLORS`]' labels then No Color, in picker order.
const PAGE_COLOR_ITEMS: &[&str] = &[
    "White, Background 1",
    "Black, Text 1",
    "Gray, Background 2",
    "Blue-Gray, Text 2",
    "Blue, Accent 1",
    "Orange, Accent 2",
    "Gray, Accent 3",
    "Gold, Accent 4",
    "Blue, Accent 5",
    "Green, Accent 6",
    "Dark Red",
    "Red",
    "Orange",
    "Yellow",
    "Light Green",
    "Green",
    "Light Blue",
    "Blue",
    "Dark Blue",
    "Purple",
    "No Color",
];

/// [`WATERMARK_PRESETS`]' labels then Remove Watermark, in picker order.
const WATERMARK_ITEMS: &[&str] = &[
    "CONFIDENTIAL 1",
    "CONFIDENTIAL 2",
    "DO NOT COPY 1",
    "DO NOT COPY 2",
    "DRAFT 1",
    "DRAFT 2",
    "SAMPLE 1",
    "SAMPLE 2",
    "ASAP 1",
    "ASAP 2",
    "URGENT 1",
    "URGENT 2",
    "Remove Watermark",
];

/// A palette colour's name for the status line, else its hex (the suite's
/// `color_name`).
fn page_color_name(rgb: u32) -> String {
    PAGE_COLORS
        .iter()
        .find(|c| c.1 == rgb)
        .map_or_else(|| format!("#{rgb:06X}"), |c| c.0.to_string())
}

impl PickerKind {
    fn title(self) -> &'static str {
        match self {
            PickerKind::FontName => " Font ",
            PickerKind::FontSize => " Font Size ",
            PickerKind::FontColor => " Font Colour ",
            PickerKind::Highlight => " Highlight ",
            PickerKind::Symbol => " Symbol ",
            PickerKind::LineSpacing => " Line Spacing ",
            PickerKind::Equation => " Equation ",
            PickerKind::PageColor => " Page Color ",
            PickerKind::Watermark => " Watermark ",
            PickerKind::PageBorders => " Page Borders ",
        }
    }
    fn items(self) -> &'static [&'static str] {
        match self {
            PickerKind::FontName => &[
                "Calibri",
                "Arial",
                "Times New Roman",
                "Courier New",
                "Cambria",
                "Georgia",
                "Verdana",
                "Consolas",
                "Tahoma",
            ],
            PickerKind::FontSize => &[
                "8", "9", "10", "11", "12", "14", "16", "18", "20", "24", "28", "36", "48", "72",
            ],
            PickerKind::FontColor => &[
                "Automatic",
                "Black",
                "Red",
                "Orange",
                "Yellow",
                "Green",
                "Blue",
                "Purple",
                "Gray",
                "White",
            ],
            PickerKind::Highlight => &[
                "None",
                "Yellow",
                "Green",
                "Cyan",
                "Magenta",
                "Red",
                "Blue",
                "Gray",
                "Dark Yellow",
            ],
            PickerKind::Symbol => &[
                "\u{2014}", "\u{2013}", "\u{2011}", "\u{2026}", "\u{2022}", "\u{00B7}", "\u{00A9}",
                "\u{00AE}", "\u{2122}", "\u{00B0}", "\u{00B1}", "\u{00D7}", "\u{00F7}", "\u{2260}",
                "\u{2264}", "\u{2265}", "\u{221E}", "\u{00A7}", "\u{00B6}", "\u{20AC}", "\u{00A3}",
                "\u{00A5}", // Typographic quotes: guillemets, low/high, angle.
                "\u{00AB}", "\u{00BB}", "\u{201E}", "\u{201C}", "\u{201D}", "\u{201A}", "\u{2018}",
                "\u{2019}", "\u{2039}", "\u{203A}", "\u{2190}", "\u{2192}", "\u{2191}", "\u{2193}",
                "\u{03B1}", "\u{03B2}", "\u{03C0}", "\u{03BC}", "\u{03A9}", "\u{221A}", "\u{2211}",
                "\u{2605}",
            ],
            PickerKind::LineSpacing => &["1.0", "1.15", "1.5", "2.0", "2.5", "3.0"],
            PickerKind::Equation => &[
                "x\u{00B2}",
                "a\u{207F}",
                "a/b",
                "\u{221A}x",
                "\u{03A3}",
                "\u{222B}",
                "lim",
                "\u{03B1}\u{03B2}\u{03B3}",
                "a\u{00B2}+b\u{00B2}=c\u{00B2}",
                "Quadratic",
            ],
            PickerKind::PageColor => PAGE_COLOR_ITEMS,
            PickerKind::Watermark => WATERMARK_ITEMS,
            PickerKind::PageBorders => PAGE_BORDER_ITEMS,
        }
    }
}

/// The `w:line` twips (auto rule) for a line-spacing picker label.
fn line_spacing_twips(label: &str) -> Option<i32> {
    match label {
        "1.0" => Some(240),
        "1.15" => Some(276),
        "1.5" => Some(360),
        "2.0" => Some(480),
        "2.5" => Some(600),
        "3.0" => Some(720),
        _ => None,
    }
}

/// A hex RRGGBB for a font-colour name (`None` = automatic).
fn color_hex(name: &str) -> Option<String> {
    Some(
        match name {
            "Black" => "000000",
            "Red" => "FF0000",
            "Orange" => "FFA500",
            "Yellow" => "FFFF00",
            "Green" => "008000",
            "Blue" => "0000FF",
            "Purple" => "800080",
            "Gray" => "808080",
            "White" => "FFFFFF",
            _ => return None, // "Automatic"
        }
        .to_string(),
    )
}

/// The OOXML highlight name for a label (`None` clears the highlight).
fn highlight_name(name: &str) -> Option<String> {
    Some(
        match name {
            "Yellow" => "yellow",
            "Green" => "green",
            "Cyan" => "cyan",
            "Magenta" => "magenta",
            "Red" => "red",
            "Blue" => "blue",
            "Gray" => "lightGray",
            "Dark Yellow" => "darkYellow",
            _ => return None, // "None"
        }
        .to_string(),
    )
}

/// A modal list picker for a font/size/colour/highlight choice.
struct FontPicker {
    kind: PickerKind,
    sel: usize,
}

/// The modal Paste Special dialog: pick how the clipboard is pasted.
struct PasteSpecial {
    /// A short description of what is on the clipboard.
    source: String,
    /// The plain-text payload of the clipboard.
    text: String,
    /// docxy's own richly-formatted clip, when our content is still on the board.
    rich: Option<Clip>,
    /// The offered options and the highlighted one.
    opts: Vec<PasteOpt>,
    sel: usize,
}

struct App {
    pkg: Package,
    editor: Editor,
    path: String,
    /// The on-disk format this document is bound to (`.docx`, Markdown, or a
    /// `.docx` inside an editable-HTML bundle).
    format: DocFormat,
    /// For [`DocFormat::Html`]: the bundle's HTML, rewrapped around the new
    /// package on save so its engine and web UI stay exactly as they were.
    bundle_html: Option<String>,
    /// While editing a Markdown file: `true` shows the raw source (each line an
    /// editable paragraph), `false` shows it rendered. Always `false` for `.docx`.
    md_source: bool,
    modified: bool,
    /// When launched with no file, a welcome/start screen overlays everything
    /// until the user picks New/Open/Quit.
    start_screen: bool,
    /// The shared centered start card (item list, selection, click rects).
    start: backstage::Start,
    /// Set when the File ▸ Exit item is chosen, so the event loop quits.
    quit_requested: bool,
    status: Option<String>,
    /// Structured document-protection policy metadata. Display text is derived
    /// from its compatibility label; authorization never compares UI strings.
    doc_protection: Protection,
    /// Word marked the document as final (#617): every mutation is refused
    /// until Edit Anyway, which also removes the mark from `pkg`.
    marked_final: bool,
    /// Structured page/header-scoped watermark state used by both the TUI
    /// overlay renderer and compatibility status labels.
    watermark_state: watermark::State,
    doc_page_borders: bool,
    scroll: usize,
    viewport_h: usize,
    page_view: bool,
    invisibles: bool,
    borderless: bool,
    /// Light document page (black on white) instead of the terminal default.
    light_page: bool,
    /// Show the column ruler above the document.
    show_ruler: bool,
    /// Show the navigation (heading outline) pane on the left.
    show_nav: bool,
    /// Navigation pane geometry + entries (set by draw() for clicks).
    nav_rect: Rect,
    nav_items: Vec<(String, usize)>, // (heading text, doc line)
    /// Top-left of the document content area on screen (set by draw() so mouse
    /// coordinates map to document rows/cols across the nav pane and ruler).
    doc_x0: u16,
    doc_y0: u16,
    /// Whether to write view-mode toggles to the user config (only the real TUI;
    /// off in tests so the suite never reads or writes the shared config file).
    persist_prefs: bool,
    /// The Home ribbon, its expanded/collapsed state, keyboard focus, and the
    /// number of rows it currently occupies (for routing mouse coordinates).
    ribbon: ribbon::Ribbon,
    ribbon_open: bool,
    ribbon_focus: ribbon::Focus,
    ribbon_h: usize,
    /// When set, the ribbon collapses back to its tab strip after each use (and
    /// when clicking into the document); when clear, it stays expanded once open.
    auto_hide_ribbon: bool,
    /// Review comments parsed from the document, and whether the side panel that
    /// lists them is shown.
    comments: Vec<docxcore::comments::Comment>,
    /// Comments whose records follow their markers, by id: each one added
    /// since the document was opened (#620), and each one Delete Comment
    /// took all the markers of (#971). Their records live here, not in
    /// `pkg`: one is in `comments` and is saved only while a marker with its
    /// id is in the body or a header or footer
    /// ([`App::comment_marker_ids_everywhere`]), so undoing Add Comment
    /// removes it, undoing Delete Comment brings it back, and redo goes the
    /// other way. A Save keeps it; an open, or a Save As (which reloads the
    /// document and its undo history), clears it.
    tracked_comments: std::collections::BTreeMap<String, TrackedComment>,
    /// Every comment id the document has had since it loaded: those of its
    /// comments and body markers then ([`used_comment_ids`]), and each one
    /// allocated since. It never shrinks, not on Delete Comment, since an
    /// undo can bring any of their markers back; a new comment never takes
    /// one of them (#620). Reseeded wherever `tracked_comments` is cleared.
    used_comment_ids: std::collections::BTreeSet<String>,
    show_comments: bool,
    /// Display for Review (#625): how tracked changes are shown. Not saved;
    /// a view only, so the document and its save never depend on it.
    markup: docxcore::markup::MarkupView,
    /// The comment highlighted by Prev/Next navigation (only once `comment_active`).
    comment_sel: usize,
    comment_active: bool,
    /// While entering a new comment's text (the draft body).
    comment_input: Option<String>,
    /// First comment row shown in the panel (scroll offset).
    comments_scroll: usize,
    /// The comments panel rect (set by draw() for wheel hit-testing).
    comments_rect: Rect,
    /// Footnotes/endnotes parsed from the package, shown in a side panel.
    notes: Vec<docxcore::notes::Note>,
    show_notes: bool,
    notes_scroll: usize,
    /// The notes panel rect (set by draw() for wheel hit-testing).
    notes_rect: Rect,
    /// In page view, how far the canvas is scrolled right to reveal the comments
    /// that sit beside the (un-shrunk) page. 0 = comments off-screen.
    comments_hscroll: usize,
    /// The horizontal scroll applied to the document this frame (= comments_hscroll
    /// when comments sit aside, else 0). Used to map the caret and mouse columns.
    doc_hscroll: u16,
    /// The full-screen File backstage, when open.
    backstage: Option<backstage::Backstage>,
    /// A modal Yes/No confirmation (e.g. Exit). The shared widget records its
    /// own button rects for mouse hit-testing.
    confirm: Option<backstage::Confirm<ConfirmAction>>,
    /// The modal Paste Special dialog, plus the option-row and button rects that
    /// draw() records each frame for mouse hit-testing.
    paste_special: Option<PasteSpecial>,
    ps_rows: Vec<Rect>,
    ps_btns: [Rect; 2],
    /// The modal Insert Field dialog, plus its option-row and button rects.
    insert_field: Option<InsertFieldDialog>,
    if_rows: Vec<Rect>,
    if_btns: [Rect; 2],
    /// The modal Paragraph dialog (precise indent), plus its row/button rects.
    para_dialog: Option<ParagraphDialog>,
    pd_rows: Vec<Rect>,
    pd_btns: [Rect; 2],
    /// The modal Review ▸ Compare dialog, plus its field/button rects.
    compare_dialog: Option<CompareDialog>,
    cd_rows: Vec<Rect>,
    cd_btns: [Rect; 2],
    /// The modal Apply-Styles dialog, plus its visible row rects and buttons.
    styles_dialog: Option<StylesDialog>,
    sd_rows: Vec<Rect>,
    sd_btns: [Rect; 2],
    /// The modal font/size/colour/highlight picker, plus its row/button rects.
    font_picker: Option<FontPicker>,
    fp_rows: Vec<Rect>,
    fp_btns: [Rect; 2],
    /// Field-evaluation context (clock + document properties + filename), kept so
    /// newly inserted fields can be computed.
    field_ctx: docxcore::field::FieldContext,
    find: Option<FindState>,
    clipboard: Option<Clip>,
    os_clip: Option<arboard::Clipboard>,
    clip_text: Option<String>,
    styles: Rc<StyleSheet>,
    numbering: Rc<Numbering>,
    /// Header/footer block content (default/first/even variants) + section flags,
    /// for print-layout margins.
    headers: PageParts,
    footers: PageParts,
    title_page: bool,
    even_odd: bool,
    /// Part names of the default header/footer (for editing/saving), if present.
    header_part: Option<String>,
    /// The body's final sectPr that `headers`/`footers`/`title_page` and the
    /// part names were derived from ([`App::sync_page_parts`]).
    page_parts_sect: String,
    footer_part: Option<String>,
    /// Active header/footer focus-edit, if any.
    hf_edit: Option<HfEdit>,
    vim: Option<VimState>,
    pending_link: Option<String>,
    /// (caret, visual row, visual column) hint to disambiguate soft-wrap and
    /// bidi-run boundaries that share one logical offset.
    visual_hint: Option<(Caret, usize, usize)>,
    /// Desired visual column preserved across repeated vertical movement.
    vertical_col_hint: Option<usize>,
    /// When true, `draw` scrolls to keep the caret visible. Cleared while the
    /// user drives the viewport directly (wheel scroll, drag-select).
    follow_caret: bool,
    /// Where a left-button press landed, so a drag can select from there.
    drag_from: Option<Caret>,
    lines: Vec<DocLine>,
    maps: Vec<LineMap>,
    /// Where each embedded image's placeholder box sits (for pixel overlay).
    images: Vec<ImageBox>,
    /// Page-view-only labels painted after document rendering. They are kept
    /// outside `lines` and `maps`, so they cannot become editable or selectable.
    watermark_overlays: Vec<watermark::Overlay>,
    /// document.xml relationships (rId → media target).
    rels: Relationships,
    /// Terminal graphics capability (kitty/iTerm2/Sixel/half-block); None = no overlay.
    picker: Option<Picker>,
    /// Per-image render state by rId. `None` value = couldn't decode (keep box).
    img_cache: HashMap<String, Option<ImgState>>,
    rendered_width: u16,
    dirty: bool,
}

impl App {
    fn new(pkg: Package, path: &str, vim: bool) -> Self {
        let styles = pkg
            .part("word/styles.xml")
            .map(|b| parse_styles_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        let numbering = pkg
            .part("word/numbering.xml")
            .map(|b| parse_numbering_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        let rels = pkg
            .part("word/_rels/document.xml.rels")
            .map(|b| parse_rels_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        let even_odd = pkg.has_even_odd();
        let comments = docxcore::comments::parse_comments(&pkg);
        let notes = docxcore::notes::parse_notes(&pkg);
        let doc_protection = pkg.protection();
        let marked_final = pkg.marked_final();
        // Recompute fields that depend on the clock / document properties (DATE,
        // TIME, AUTHOR, CREATEDATE, …) so they show a live value like Word does,
        // rather than the value last cached in the file. This is a content
        // mutation, so protected modes that disallow content keep the cached
        // result both on screen and on save.
        let field_ctx = docxcore::field::FieldContext {
            now: local_now(),
            props: pkg
                .part("docProps/core.xml")
                .map(|b| docxcore::field::parse_core_props(std::str::from_utf8(b).unwrap_or("")))
                .unwrap_or_default(),
            filename: std::path::Path::new(path)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        let mut doc = pkg.document.clone();
        if protection::authorize(&doc_protection, protection::MutationKind::Content).is_ok() {
            docxcore::field::recompute(&mut doc, &field_ctx);
        }
        with_final_section(&mut doc, &pkg);
        let page_parts = PageState::derive(&pkg, &doc);
        let watermark_state = watermark::State::from_package(&pkg);
        let doc_page_borders = pkg.has_page_borders();
        let used_comment_ids = used_comment_ids(&comments, &doc);
        App {
            pkg,
            editor: Editor::new(doc),
            path: path.to_string(),
            format: format_for(path),
            bundle_html: None,
            md_source: false,
            modified: false,
            start_screen: false,
            start: backstage::Start::new(
                "docxy",
                vec![
                    backstage::StartItem {
                        label: "New Word document   (.docx)".to_string(),
                        desc: None,
                    },
                    backstage::StartItem {
                        label: "New Markdown document (.md)".to_string(),
                        desc: None,
                    },
                    backstage::StartItem {
                        label: "Open an existing file…".to_string(),
                        desc: None,
                    },
                    backstage::StartItem {
                        label: "Quit".to_string(),
                        desc: None,
                    },
                ],
                Color::LightBlue,
            ),
            quit_requested: false,
            status: None,
            doc_protection,
            marked_final,
            watermark_state,
            doc_page_borders,
            scroll: 0,
            viewport_h: 1,
            page_view: false,
            invisibles: false,
            borderless: false,
            light_page: false,
            show_ruler: false,
            show_nav: false,
            nav_rect: Rect::default(),
            nav_items: Vec::new(),
            doc_x0: 0,
            doc_y0: 0,
            persist_prefs: false,
            ribbon: ribbon::Ribbon::home(),
            ribbon_open: false,
            ribbon_focus: ribbon::Focus::None,
            auto_hide_ribbon: false,
            comments,
            tracked_comments: Default::default(),
            used_comment_ids,
            show_comments: false,
            markup: Default::default(),
            comment_sel: 0,
            comment_active: false,
            comment_input: None,
            comments_scroll: 0,
            comments_rect: Rect::default(),
            notes,
            show_notes: false,
            notes_scroll: 0,
            notes_rect: Rect::default(),
            comments_hscroll: 0,
            doc_hscroll: 0,
            // Set each frame by draw(); 0 until then so mouse rows map directly.
            ribbon_h: 0,
            backstage: None,
            confirm: None,
            paste_special: None,
            ps_rows: Vec::new(),
            ps_btns: [Rect::default(); 2],
            insert_field: None,
            if_rows: Vec::new(),
            if_btns: [Rect::default(); 2],
            para_dialog: None,
            compare_dialog: None,
            cd_rows: Vec::new(),
            cd_btns: [Rect::default(); 2],
            pd_rows: Vec::new(),
            pd_btns: [Rect::default(); 2],
            styles_dialog: None,
            sd_rows: Vec::new(),
            sd_btns: [Rect::default(); 2],
            font_picker: None,
            fp_rows: Vec::new(),
            fp_btns: [Rect::default(); 2],
            field_ctx,
            find: None,
            clipboard: None,
            os_clip: arboard::Clipboard::new().ok(),
            clip_text: None,
            styles: Rc::new(styles),
            numbering: Rc::new(numbering),
            headers: page_parts.headers,
            footers: page_parts.footers,
            title_page: page_parts.title_page,
            even_odd,
            header_part: page_parts.header_part,
            footer_part: page_parts.footer_part,
            page_parts_sect: page_parts.sect,
            hf_edit: None,
            vim: if vim { Some(VimState::new()) } else { None },
            pending_link: None,
            visual_hint: None,
            vertical_col_hint: None,
            follow_caret: true,
            drag_from: None,
            lines: Vec::new(),
            maps: Vec::new(),
            images: Vec::new(),
            watermark_overlays: Vec::new(),
            rels,
            picker: None,
            img_cache: HashMap::new(),
            rendered_width: 0,
            dirty: true,
        }
    }

    fn options(&self, width: u16) -> RenderOptions {
        // In find mode, highlight all matches; otherwise the live selection.
        let selection = if !self.markup.is_editable() {
            Vec::new()
        } else {
            match &self.find {
                Some(f) => f
                    .matches
                    .iter()
                    .map(|m| (m.path.clone(), m.start, m.end))
                    .collect(),
                None => self.editor.selection_spans(),
            }
        };
        RenderOptions {
            width: width.max(1) as usize,
            show_invisibles: self.invisibles,
            page_view: self.page_view,
            borderless_tables: self.borderless,
            selection,
            styles: self.styles.clone(),
            list_markers: Rc::new(compute_markers(
                &self.editor.doc.markup_view(self.markup),
                &self.numbering,
            )),
            page: self
                .editor
                .doc
                .trailing_section_properties()
                .map(|section| PageGeom::from_sect_pr(&section.raw))
                .unwrap_or_else(|| self.pkg.page_geom()),
            // While editing a header/footer the editor *is* that surface, so the
            // banner/margin copy is suppressed (no duplicate header).
            headers: if self.hf_edit.is_some() {
                PageParts::default()
            } else {
                self.headers.clone()
            },
            footers: if self.hf_edit.is_some() {
                PageParts::default()
            } else {
                self.footers.clone()
            },
            title_page: self.title_page,
            even_odd: self.even_odd,
            bidi: Some(bidi::projector()),
        }
    }

    fn save_view_prefs(&self) {
        if !self.persist_prefs {
            return;
        }
        ViewPrefs {
            page_view: self.page_view,
            invisibles: self.invisibles,
            borderless: self.borderless,
            light_page: self.light_page,
            show_ruler: self.show_ruler,
            show_nav: self.show_nav,
            show_comments: self.show_comments,
            show_notes: self.show_notes,
            auto_hide_ribbon: self.auto_hide_ribbon,
        }
        .save();
    }

    // ---- ribbon ----

    /// Rows the ribbon currently occupies: 1 for the collapsed tab strip, or the
    /// strip + body + yellow hint bar when expanded.
    fn ribbon_height(&self) -> usize {
        if self.ribbon_open {
            ribbon::EXPANDED_H as usize // tab strip + closed body box (6)
        } else {
            1
        }
    }

    /// Handle a key while the ribbon has focus. Returns `Some(quit)` if consumed,
    /// `None` to let it fall through to normal editing (e.g. Ctrl shortcuts).
    fn ribbon_key(&mut self, key: KeyEvent) -> Option<bool> {
        use ribbon::Dir;
        match key.code {
            KeyCode::Esc | KeyCode::F(9) => {
                self.ribbon_focus = ribbon::Focus::None;
                self.ribbon_open = false;
                self.dirty = true;
                Some(false)
            }
            KeyCode::Left => self.ribbon_move(Dir::Left),
            KeyCode::Right => self.ribbon_move(Dir::Right),
            KeyCode::Up => self.ribbon_move(Dir::Up),
            KeyCode::Down => self.ribbon_move(Dir::Down),
            KeyCode::Enter | KeyCode::Char(' ') => {
                match self.ribbon_focus {
                    ribbon::Focus::Tab(i) if self.ribbon.tab_label(i) == Some("File") => {
                        self.open_backstage();
                    }
                    ribbon::Focus::Tab(_) => {
                        self.ribbon_focus = self.ribbon.enter_body();
                        self.dirty = true;
                    }
                    ribbon::Focus::Button(_) => {
                        if let Some((act, _)) = self.ribbon.focus_act(self.ribbon_focus) {
                            self.run_act(act);
                        }
                    }
                    ribbon::Focus::None => {}
                }
                Some(false)
            }
            _ => None,
        }
    }

    fn ribbon_move(&mut self, dir: ribbon::Dir) -> Option<bool> {
        self.ribbon_focus = self.ribbon.nav(self.ribbon_focus, dir);
        // Moving across tabs switches the active ribbon so its body updates live.
        if let ribbon::Focus::Tab(i) = self.ribbon_focus {
            self.ribbon.set_active(i);
        }
        self.dirty = true;
        Some(false)
    }

    /// Toggle the comments review side panel.
    fn toggle_comments(&mut self) {
        self.show_comments = !self.show_comments;
        self.comments_scroll = 0;
        self.save_view_prefs();
        self.status = Some(if self.comments.is_empty() {
            "No comments in this document.".to_string()
        } else if self.show_comments {
            format!("Showing {} comment(s).", self.comments.len())
        } else {
            "Comments panel hidden.".to_string()
        });
        self.dirty = true;
    }

    /// Toggle the footnotes/endnotes side panel.
    /// Choose Display for Review. A view only: the document, its history and
    /// what a save writes are untouched.
    fn set_markup(&mut self, view: docxcore::markup::MarkupView) {
        self.markup = view;
        self.status = Some(format!("Display for Review: {}", view.label()));
        self.dirty = true;
    }

    fn toggle_notes(&mut self) {
        self.show_notes = !self.show_notes;
        self.notes_scroll = 0;
        self.save_view_prefs();
        self.status = Some(if self.notes.is_empty() {
            "No footnotes or endnotes in this document.".to_string()
        } else if self.show_notes {
            let f = self.notes.iter().filter(|n| !n.endnote).count();
            let e = self.notes.len() - f;
            format!("Showing {f} footnote(s), {e} endnote(s).")
        } else {
            "Notes panel hidden.".to_string()
        });
        self.dirty = true;
    }

    fn navigate_revision(&mut self, previous: bool) {
        let location = if previous {
            self.editor.previous_revision()
        } else {
            self.editor.next_revision()
        };
        self.status = Some(match location {
            Some(location) => {
                let total = self.editor.revision_locations().len();
                revision_status(&location.address, total)
            }
            None => "No tracked changes in this document.".to_string(),
        });
        self.dirty = true;
    }

    fn review_current_revision(&mut self, action: RevisionAction) {
        let Some(target) = self
            .editor
            .current_revision()
            .map(|location| location.address.target)
        else {
            self.status = Some(
                "No change at the caret. Use Previous Change or Next Change first.".to_string(),
            );
            self.dirty = true;
            return;
        };
        if !self.mutation_allowed(protection::MutationKind::Content) {
            return;
        }
        let outcome = match action {
            RevisionAction::Accept => self.editor.accept_revision(target),
            RevisionAction::Reject => self.editor.reject_revision(target),
        };
        let changed = outcome.is_applied();
        let message = Self::revision_outcome_status(&outcome);
        if changed {
            self.after_edit();
        }
        self.status = Some(message);
        self.dirty = true;
    }

    fn revision_outcome_status(outcome: &RevisionOutcome) -> String {
        match outcome {
            RevisionOutcome::Applied {
                target,
                action,
                category,
            } => format!(
                "{} {} (target {}).",
                match action {
                    RevisionAction::Accept => "Accepted",
                    RevisionAction::Reject => "Rejected",
                },
                revision_category_name(category),
                target.0
            ),
            RevisionOutcome::Stale { target, .. } => {
                format!(
                    "Change target {} is stale; refresh the review list.",
                    target.0
                )
            }
            RevisionOutcome::Unsupported { target, kind, .. } => format!(
                "Change target {} is unsupported ({}); it was left untouched.",
                target.0,
                unsupported_revision_name(kind)
            ),
            RevisionOutcome::Malformed { target, .. } => format!(
                "Change target {} is malformed; it was left untouched.",
                target.0
            ),
        }
    }

    fn request_review_all(&mut self, action: RevisionAction) {
        let total = self.editor.revision_locations().len();
        if total == 0 {
            self.status = Some("No tracked changes in this document.".to_string());
            self.dirty = true;
            return;
        }
        if !self.mutation_allowed(protection::MutationKind::Content) {
            return;
        }
        let verb = match action {
            RevisionAction::Accept => "Accept",
            RevisionAction::Reject => "Reject",
        };
        self.confirm = Some(
            backstage::Confirm::new(
                format!("{verb} all {total} tracked changes?"),
                ConfirmAction::ReviewAll(action),
                Color::LightBlue,
            )
            .default_no(),
        );
        self.dirty = true;
    }

    fn apply_review_all(&mut self, action: RevisionAction) {
        if !self.mutation_allowed(protection::MutationKind::Content) {
            return;
        }
        let outcomes = match action {
            RevisionAction::Accept => self.editor.accept_all_revisions(),
            RevisionAction::Reject => self.editor.reject_all_revisions(),
        };
        let applied = outcomes
            .iter()
            .filter(|outcome| outcome.is_applied())
            .count();
        let unsupported = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RevisionOutcome::Unsupported { .. }))
            .count();
        let malformed = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RevisionOutcome::Malformed { .. }))
            .count();
        let stale = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, RevisionOutcome::Stale { .. }))
            .count();
        if applied > 0 {
            self.after_edit();
        }
        let verb = match action {
            RevisionAction::Accept => "Accepted",
            RevisionAction::Reject => "Rejected",
        };
        let mut message = format!("{verb} {applied} of {} tracked changes", outcomes.len());
        let mut skipped = Vec::new();
        if unsupported > 0 {
            skipped.push(format!("{unsupported} unsupported"));
        }
        if malformed > 0 {
            skipped.push(format!("{malformed} malformed"));
        }
        if stale > 0 {
            skipped.push(format!("{stale} stale"));
        }
        if !skipped.is_empty() {
            message.push_str(&format!("; left {} untouched", skipped.join(", ")));
        }
        message.push('.');
        self.status = Some(message);
        self.dirty = true;
    }

    /// Run a ribbon command, mapping it to the matching editor operation.
    fn run_act(&mut self, act: ribbon::Act) {
        use ribbon::Act::*;
        if let Some(mutation) = Self::ribbon_mutation_kind(act) {
            if !self.mutation_allowed(mutation) {
                return;
            }
        }
        match act {
            Cut => self.do_cut(),
            Copy => self.do_copy(),
            PasteSpecial => self.open_paste_special(),
            HorizontalLine => {
                self.editor.insert_hrule();
                self.after_edit();
                self.status = Some("Inserted horizontal line".to_string());
            }
            InsertField => {
                self.insert_field = Some(InsertFieldDialog { sel: 0 });
                self.dirty = true;
            }
            InsertSymbol => self.open_picker(PickerKind::Symbol),
            InsertEquation => self.open_picker(PickerKind::Equation),
            LineSpacing => self.open_picker(PickerKind::LineSpacing),
            PageColor => self.open_design_picker(PickerKind::PageColor),
            Watermark => self.open_design_picker(PickerKind::Watermark),
            PageBorders => self.open_design_picker(PickerKind::PageBorders),
            PageNumber => {
                let inl = self.build_field(FieldKind::Page);
                self.editor.paste(&Clip {
                    paras: vec![vec![inl]],
                });
                self.after_edit();
                self.status = Some("Inserted page number".to_string());
            }
            PageBreak => {
                self.editor.insert_break(BreakKind::Page);
                self.after_edit();
                self.status = Some("Inserted page break".to_string());
            }
            InsertTable => {
                self.insert_table(2, 2);
                self.after_edit();
                self.status = Some("Inserted 2×2 table".to_string());
            }
            Columns => {
                let next = self.edit_final_sect_pr(true, |pkg| {
                    let next = match pkg.columns() {
                        1 => 2,
                        2 => 3,
                        _ => 1,
                    };
                    pkg.set_columns(next);
                    next
                });
                self.modified = true;
                self.dirty = true;
                self.status = Some(format!("Columns: {next}"));
            }
            Hyphenation => {
                let on = !self.pkg.has_auto_hyphenation();
                self.pkg.set_auto_hyphenation(on);
                self.modified = true;
                self.dirty = true;
                self.status = Some(format!(
                    "Automatic hyphenation: {}",
                    if on { "on" } else { "off" }
                ));
            }
            Paste => self.do_paste(),
            Bold => {
                self.editor.toggle_bold();
                self.after_edit();
            }
            Italic => {
                self.editor.toggle_italic();
                self.after_edit();
            }
            Underline => {
                self.editor.toggle_underline();
                self.after_edit();
            }
            Strike => {
                self.editor.toggle_strike();
                self.after_edit();
            }
            Subscript => {
                self.editor
                    .toggle_vert_align(docxcore::model::VertAlign::Subscript);
                self.after_edit();
            }
            Superscript => {
                self.editor
                    .toggle_vert_align(docxcore::model::VertAlign::Superscript);
                self.after_edit();
            }
            GrowFont => {
                self.editor.resize_font(2);
                self.after_edit();
            }
            ShrinkFont => {
                self.editor.resize_font(-2);
                self.after_edit();
            }
            ChangeCase => {
                self.editor.cycle_case();
                self.after_edit();
            }
            ClearFormatting => {
                self.editor.clear_run_formatting();
                self.after_edit();
            }
            FontName => self.open_picker(PickerKind::FontName),
            FontSize => self.open_picker(PickerKind::FontSize),
            FontColor => self.open_picker(PickerKind::FontColor),
            Highlight => self.open_picker(PickerKind::Highlight),
            Bullets => self.apply_list(true),
            Numbering => self.apply_list(false),
            IncreaseIndent => {
                self.editor.change_indent(720);
                self.after_edit();
            }
            DecreaseIndent => {
                self.editor.change_indent(-720);
                self.after_edit();
            }
            FirstLineIndent => {
                self.editor.set_first_line(720);
                self.after_edit();
            }
            HangingIndent => {
                self.editor.set_first_line(-720);
                self.after_edit();
            }
            ParagraphDialog => self.open_para_dialog(),
            Sort => {
                self.editor.sort_paragraphs();
                self.after_edit();
                self.status = Some("Sorted paragraphs".to_string());
            }
            ParaBorders => self.toggle_para_border(),
            AlignLeft => {
                self.editor.set_align(Align::Left);
                self.after_edit();
            }
            AlignCenter => {
                self.editor.set_align(Align::Center);
                self.after_edit();
            }
            AlignRight => {
                self.editor.set_align(Align::Right);
                self.after_edit();
            }
            Justify => {
                self.editor.set_align(Align::Justify);
                self.after_edit();
            }
            ShowHide => {
                self.invisibles = !self.invisibles;
                self.save_view_prefs();
                self.dirty = true;
            }
            Find | Replace => self.enter_find(),
            SelectAll => {
                self.editor.select_all();
                self.dirty = true;
            }
            ToggleComments => self.toggle_comments(),
            ToggleNotes => self.toggle_notes(),
            PrevComment => self.nav_comment(-1),
            NextComment => self.nav_comment(1),
            NewComment => self.start_comment(),
            DeleteComment => self.delete_comment(),
            ResolveComment => self.resolve_comment(),
            DeleteAllComments => self.delete_all_comments(),
            CycleMarkup => self.set_markup(self.markup.next()),
            PrevRevision => self.navigate_revision(true),
            NextRevision => self.navigate_revision(false),
            AcceptRevision => self.review_current_revision(RevisionAction::Accept),
            RejectRevision => self.review_current_revision(RevisionAction::Reject),
            AcceptAllRevisions => self.request_review_all(RevisionAction::Accept),
            Compare => self.open_compare_dialog(),
            RejectAllRevisions => self.request_review_all(RevisionAction::Reject),
            ReadMode => self.set_page_view(false),
            PrintLayout => self.set_page_view(true),
            DarkMode => {
                self.light_page = !self.light_page;
                self.save_view_prefs();
                self.status = Some(
                    if self.light_page {
                        "Light page"
                    } else {
                        "Dark page"
                    }
                    .to_string(),
                );
                self.dirty = true;
            }
            ToggleRuler => {
                self.show_ruler = !self.show_ruler;
                self.save_view_prefs();
                self.dirty = true;
            }
            ToggleNav => {
                self.show_nav = !self.show_nav;
                self.save_view_prefs();
                self.dirty = true;
            }
            AutoHideRibbon => {
                self.auto_hide_ribbon = !self.auto_hide_ribbon;
                // Enabling auto-hide collapses the ribbon right away, the way
                // Word's "Collapse the Ribbon" hides it on the spot.
                if self.auto_hide_ribbon {
                    self.ribbon_open = false;
                    self.ribbon_focus = ribbon::Focus::None;
                }
                self.save_view_prefs();
                self.dirty = true;
            }
            EditDocument => {
                if self.hf_edit.is_some() {
                    self.exit_hf_edit(true);
                }
            }
            EditHeader => self.enter_hf_edit(true),
            EditFooter => self.enter_hf_edit(false),
            MdRendered => self.set_md_source(false),
            MdSource => self.set_md_source(true),
            ApplyStyle(id) => {
                self.editor.set_para_style(Some(id));
                self.after_edit();
                self.status = Some(format!("Applied style: {id}"));
            }
            StylesDialog => self.open_styles_dialog(),
            Todo(name) => {
                self.status = Some(format!("{name} — not implemented yet"));
                self.dirty = true;
            }
        }
    }

    /// Set print-layout (page) view on/off and persist it.
    fn set_page_view(&mut self, on: bool) {
        // Markdown is a reflowable format with no fixed pages, so print layout
        // doesn't apply — page view is only meaningful for `.docx`.
        if on && self.format == DocFormat::Markdown {
            self.status = Some("Page view isn't available for Markdown.".to_string());
            return;
        }
        if self.page_view != on {
            self.page_view = on;
            self.save_view_prefs();
            self.dirty = true;
        }
    }

    fn draw_ribbon(&self, f: &mut Frame, area: Rect) {
        let mut lines = vec![self.ribbon.render_tabs(self.ribbon_focus)];
        if self.ribbon_open {
            lines.extend(self.ribbon.render_body(self.ribbon_focus));
        }
        f.render_widget(Paragraph::new(Text::from(lines)), area);
    }

    // ---- File backstage ----

    /// Open the full-screen File menu, starting in the current file's folder.
    fn open_backstage(&mut self) {
        let dir = std::path::Path::new(&self.path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        self.backstage = Some(backstage::Backstage::open(dir, self.extensions()));
        self.ribbon_focus = ribbon::Focus::None;
        self.ribbon_open = false;
        self.dirty = true;
    }

    /// Leave the File backstage via a click on the ribbon tab strip. Clicking the
    /// File header closes the panel back to the document; any other tab switches
    /// to it and opens its ribbon.
    fn backstage_tab_click(&mut self, tab: usize) {
        self.backstage = None;
        if self.ribbon.tab_label(tab) == Some("File") {
            self.ribbon_focus = ribbon::Focus::None;
            self.ribbon_open = !self.auto_hide_ribbon;
        } else {
            self.ribbon.set_active(tab);
            self.ribbon_open = true;
            self.ribbon_focus = ribbon::Focus::Tab(tab);
        }
        self.dirty = true;
    }

    /// Act on a [`backstage::BackstageEvent`] returned by the backstage's own
    /// `key`/`mouse` handlers. Shared by `backstage_key` and `bs_mouse`.
    fn apply_backstage_event(&mut self, ev: backstage::BackstageEvent) -> bool {
        use backstage::BackstageEvent;
        self.dirty = true;
        match ev {
            BackstageEvent::None => false,
            BackstageEvent::Close => {
                self.backstage = None;
                // Restore the pinned ribbon (expanded when auto-hide is off).
                self.ribbon_open = !self.auto_hide_ribbon;
                false
            }
            BackstageEvent::New => {
                self.new_document();
                self.backstage = None;
                false
            }
            BackstageEvent::Open(p) => {
                let _ = self.open_path(&p);
                self.backstage = None;
                false
            }
            BackstageEvent::Save => {
                self.save();
                self.backstage = None;
                false
            }
            BackstageEvent::SaveAs { dir, name } => {
                self.commit_save_as(dir, name);
                false
            }
            BackstageEvent::Export => {
                self.export_pdf();
                self.backstage = None;
                false
            }
            // docxy lists no extra exports.
            BackstageEvent::ExportExtra(_) => false,
            BackstageEvent::Exit => {
                self.request_exit();
                self.quit_requested
            }
            // docxy lists no editable Info rows, so this never arrives.
            BackstageEvent::EditInfo(_) => false,
        }
    }

    /// Returns true if the app should quit.
    fn backstage_key(&mut self, key: KeyEvent) -> bool {
        let mut bs = self.backstage.take();
        let ev = bs
            .as_mut()
            .map(|b| b.key(key, self))
            .unwrap_or(backstage::BackstageEvent::None);
        self.backstage = bs;
        self.apply_backstage_event(ev)
    }

    /// Route a left-click inside the File backstage. Row 0 is the ribbon tab
    /// strip (drawn over the backstage) and is handled here directly; every
    /// other row is delegated to `backstage::Backstage::mouse`.
    fn bs_mouse(&mut self, x: u16, y: u16) {
        // Row 0 is the ribbon tab strip. A click on another tab switches to it;
        // a click on File — or anywhere else on the strip (its padding or the
        // hint) — just leaves the panel, so the small File header isn't a
        // pixel-perfect target.
        if y == 0 {
            match self.ribbon.hit(x, 0, false) {
                ribbon::Hit::Tab(i) if self.ribbon.tab_label(i) != Some("File") => {
                    self.backstage_tab_click(i)
                }
                _ => self.backstage_tab_click(0),
            }
            return;
        }
        let mut bs = self.backstage.take();
        let ev = bs
            .as_mut()
            .map(|b| b.mouse(x, y, self))
            .unwrap_or(backstage::BackstageEvent::None);
        self.backstage = bs;
        self.apply_backstage_event(ev);
    }

    /// Write the document to `dir/name`, picking the format from the typed
    /// extension: `.md`/`.markdown` → Markdown, `.docx` → Word, `.html`/`.htm` →
    /// editable HTML (the opened bundle rewrapped, else a new page when this
    /// build has the engine; an existing file there that is not a Word bundle
    /// is refused, never replaced). With no extension the current format's is
    /// added (`.md`, `.docx`, or `.docx.html` for an opened bundle). This is how
    /// a document moves between formats. Makes the new file current and closes
    /// the backstage.
    fn commit_save_as(&mut self, dir: std::path::PathBuf, name: String) {
        if name.is_empty() {
            self.status = Some("Save As — type a file name first.".to_string());
            return;
        }
        // Resolve the target format + ensure the name carries an extension.
        let lower = name.to_ascii_lowercase();
        let known = [".html", ".htm", ".docx", ".md", ".markdown", ".mdown"];
        let (mut fname, target) = if known.iter().any(|e| lower.ends_with(e)) {
            let fmt = format_for(&name);
            (name, fmt)
        } else {
            let mut f = name;
            f.push_str(match self.format {
                DocFormat::Markdown => ".md",
                DocFormat::Docx => ".docx",
                DocFormat::Html => ".docx.html",
            });
            (f, self.format)
        };
        if target.is_docx() != self.format.is_docx()
            && !self.mutation_allowed(protection::MutationKind::PackageMetadata)
        {
            return;
        }
        fname = fname.trim().to_string();
        let path = dir.join(&fname);
        let path_str = path.to_string_lossy().into_owned();
        // A page replaces only a missing file or a Word bundle, never the
        // user's own page (the terminal Save As does not ask about overwrites).
        if target == DocFormat::Html {
            if let Err(e) = htmlbundle::check_html_target(&path) {
                self.status = Some(format!("save failed: {e}"));
                return;
            }
        }
        if target == DocFormat::Html && self.bundle_html.is_none() && !html::can_export() {
            self.status = Some(format!("Save As editable HTML: {}", html::NO_ENGINE));
            return;
        }
        if self.hf_edit.is_some() {
            self.exit_hf_edit(true);
        }
        // Serialize in the target format, then rebind in-memory state to the saved
        // file so format, view and numbering all stay consistent.
        let (bytes, pkg) = match target {
            DocFormat::Markdown => {
                let md = self.current_markdown();
                let pkg = new_markdown_package(from_markdown(&md));
                (md.into_bytes(), pkg)
            }
            DocFormat::Docx | DocFormat::Html => {
                self.reconcile_tracked_comments();
                let docx = if self.format.is_docx() && !self.modified {
                    save_package_preserving_document(&self.pkg)
                } else {
                    self.pkg.document = self.current_document();
                    save_package(&self.pkg)
                };
                let pkg = load_package(&docx).unwrap_or_else(|_| self.pkg.clone());
                if target != DocFormat::Html {
                    (docx, pkg)
                } else {
                    // Keep an opened bundle's engine and UI; a new one embeds
                    // this build's.
                    let page = match &self.bundle_html {
                        Some(old) => htmlbundle::rewrap(old, &docx).map_err(|e| e.to_string()),
                        None => html::export(&htmlbundle::docx_source_name(&fname), &docx),
                    };
                    match page {
                        Ok(page) => (page.into_bytes(), pkg),
                        Err(e) => {
                            self.status = Some(format!("save failed: {e}"));
                            return;
                        }
                    }
                }
            }
        };
        match write_atomic(Path::new(&path), &bytes) {
            Ok(()) => {
                let n = bytes.len();
                self.load_package_state(pkg, path_str.clone());
                if target == DocFormat::Html {
                    self.bundle_html = String::from_utf8(bytes).ok();
                }
                self.backstage = None;
                self.status = Some(format!("Saved {path_str} ({n} bytes)"));
            }
            Err(e) => self.status = Some(format!("save failed: {e}")),
        }
    }

    /// Apply the modal's choice. Returns true if the app should quit.
    /// Act on the shared dialog's outcome. Returns true if the app should quit.
    fn apply_confirm(&mut self, outcome: backstage::ConfirmOutcome<ConfirmAction>) -> bool {
        self.dirty = true;
        match outcome {
            backstage::ConfirmOutcome::Pending => false,
            backstage::ConfirmOutcome::Cancelled => {
                self.confirm = None;
                false
            }
            backstage::ConfirmOutcome::Confirmed(action) => {
                self.confirm = None;
                match action {
                    ConfirmAction::Exit => {
                        self.quit_requested = true;
                        true
                    }
                    ConfirmAction::OverwritePdf(out) => {
                        self.write_pdf(out);
                        false
                    }
                    ConfirmAction::ReviewAll(action) => {
                        self.apply_review_all(action);
                        false
                    }
                    ConfirmAction::Compare { original, revised } => {
                        self.run_compare(&original, &revised, true);
                        false
                    }
                    ConfirmAction::EditAnyway => {
                        self.edit_anyway();
                        false
                    }
                }
            }
        }
    }

    /// Route a key to the Yes/No modal. Returns true if the app should quit.
    fn confirm_key(&mut self, key: KeyEvent) -> bool {
        let Some(c) = self.confirm.as_mut() else {
            return false;
        };
        let outcome = c.key(key);
        self.apply_confirm(outcome)
    }

    /// Route a click to the Yes/No modal. Returns true if the app should quit.
    fn confirm_mouse(&mut self, x: u16, y: u16) -> bool {
        let Some(c) = self.confirm.as_mut() else {
            return false;
        };
        let outcome = c.mouse(x, y);
        self.apply_confirm(outcome)
    }

    /// Replace the open document with one loaded from `path` (Markdown or `.docx`).
    fn open_path(&mut self, path: &std::path::Path) -> Result<(), String> {
        let p = path.display().to_string();
        match load_input(&p) {
            Ok(input) => {
                self.load_package_state(input.pkg, p.clone());
                let warning = input.bundle.as_ref().and_then(|b| b.warning.as_deref());
                let notice = open_notice(warning, input.encoding);
                self.bundle_html = input.bundle.map(|b| b.html);
                self.status = Some(match notice {
                    Some(n) => format!("opened {p} — {n}"),
                    None => format!("opened {p}"),
                });
                Ok(())
            }
            Err(e) => {
                self.status = Some(format!("cannot open {p}: {e}"));
                Err(e)
            }
        }
    }

    fn new_document(&mut self) {
        let pkg = new_package(Document {
            body: vec![Block::Paragraph(docxcore::model::Paragraph::default())],
        });
        self.load_package_state(pkg, "untitled.docx".to_string());
        self.status = Some("new document".to_string());
    }

    /// Start a fresh blank Markdown document (one empty paragraph) in the editor.
    fn new_markdown_document(&mut self) {
        let pkg = new_markdown_package(from_markdown(""));
        self.load_package_state(pkg, "untitled.md".to_string());
        self.status = Some("new Markdown document".to_string());
    }

    /// Keys for the welcome/start screen. Returns true to quit the app.
    fn start_screen_key(&mut self, key: KeyEvent) -> bool {
        match self.start.key(key) {
            backstage::StartEvent::Choose(i) => self.start_choose(i),
            backstage::StartEvent::Quit => true,
            backstage::StartEvent::None => {
                self.dirty = true;
                false
            }
        }
    }

    /// Act on a chosen welcome-screen item. Returns true to quit.
    fn start_choose(&mut self, idx: usize) -> bool {
        self.start_screen = false;
        match idx {
            0 => self.new_document(),
            1 => self.new_markdown_document(),
            2 => self.open_backstage(),
            _ => return true, // Quit
        }
        self.dirty = true;
        false
    }

    fn export_pdf(&mut self) {
        let mut out = std::path::PathBuf::from(&self.path);
        out.set_extension("pdf");
        // Don't clobber an existing PDF silently — ask first.
        if out.exists() {
            let name = out
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| out.display().to_string());
            self.confirm = Some(
                backstage::Confirm::new(
                    format!("{name} already exists. Overwrite it?"),
                    ConfirmAction::OverwritePdf(out),
                    Color::LightBlue,
                )
                .default_no(),
            );
            self.dirty = true;
            return;
        }
        self.write_pdf(out);
    }

    /// PDF layout inputs from the package.
    fn pdf_options(&self) -> PdfOptions {
        let mut opts = PdfOptions::from_package(&self.pkg, self.styles.clone());
        // Committed header/footer edits are already in their parts; one being
        // edited prints its live content.
        if let Some(hf) = &self.hf_edit {
            opts.header_footer
                .insert(hf.part.clone(), Rc::new(self.editor.doc.body.clone()));
        }
        opts
    }

    /// The document PDF export prints: the body, even while a header or footer
    /// is being edited.
    fn pdf_document(&self) -> &Document {
        &self.body_editor().doc
    }

    /// Render the document to a PDF at `out` and report the result in the status
    /// line. Callers handle any overwrite confirmation first.
    fn write_pdf(&mut self, out: std::path::PathBuf) {
        let pdf = to_pdf(self.pdf_document(), &self.pdf_options());
        self.status = match export_atomic(Some(Path::new(&self.path)), &out, &pdf) {
            Ok(()) => Some(format!("exported {}", out.display())),
            Err(e) => Some(format!("export failed: {e}")),
        };
    }

    /// Rebuild all per-document state from a freshly loaded package.
    fn load_package_state(&mut self, mut pkg: Package, path: String) {
        let styles = pkg
            .part("word/styles.xml")
            .map(|b| parse_styles_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        let numbering = pkg
            .part("word/numbering.xml")
            .map(|b| parse_numbering_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        let rels = pkg
            .part("word/_rels/document.xml.rels")
            .map(|b| parse_rels_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        self.even_odd = pkg.has_even_odd();
        self.comments = docxcore::comments::parse_comments(&pkg);
        self.tracked_comments.clear();
        self.notes = docxcore::notes::parse_notes(&pkg);
        self.notes_scroll = 0;
        self.comments_scroll = 0;
        self.comment_sel = 0;
        self.comment_active = false;
        self.doc_protection = pkg.protection();
        self.marked_final = pkg.marked_final();
        self.watermark_state = watermark::State::from_package(&pkg);
        self.doc_page_borders = pkg.has_page_borders();
        let mut doc = std::mem::take(&mut pkg.document);
        with_final_section(&mut doc, &pkg);
        let page_parts = PageState::derive(&pkg, &doc);
        self.headers = page_parts.headers;
        self.footers = page_parts.footers;
        self.title_page = page_parts.title_page;
        self.header_part = page_parts.header_part;
        self.footer_part = page_parts.footer_part;
        self.page_parts_sect = page_parts.sect;
        self.pkg = pkg;
        self.used_comment_ids = used_comment_ids(&self.comments, &doc);
        self.editor = Editor::new(doc);
        self.styles = Rc::new(styles);
        self.numbering = Rc::new(numbering);
        self.rels = rels;
        self.format = format_for(&path);
        self.bundle_html = None;
        // Page view has no meaning for Markdown (no fixed pages).
        if self.format == DocFormat::Markdown {
            self.page_view = false;
        }
        self.md_source = false;
        // A header/footer edit belongs to the document being replaced.
        self.hf_edit = None;
        self.path = path;
        self.modified = false;
        self.scroll = 0;
        self.find = None;
        self.img_cache.clear();
        self.dirty = true;
    }

    /// Refresh watermark labels/scopes after a live header or section change.
    /// Use a temporary package view containing the current editor body when
    /// resolving section headers; `pkg.document` is the last saved baseline.
    fn refresh_watermark_state(&mut self) {
        let mut live = self.pkg.clone();
        live.document = self.editor.doc.clone();
        self.watermark_state = watermark::State::from_package(&live);
    }

    /// The editor holding the document body (parked while a header or footer
    /// is being edited).
    fn body_editor(&self) -> &Editor {
        self.hf_edit.as_ref().map_or(&self.editor, |hf| &hf.body)
    }

    fn body_editor_mut(&mut self) -> &mut Editor {
        self.hf_edit
            .as_mut()
            .map_or(&mut self.editor, |hf| &mut hf.body)
    }

    /// Re-derive the header/footer parts, their content and titlePg from the
    /// body's final sectPr whenever it changed: undo and redo restore that
    /// sectPr, so the view, save and PDF follow them.
    fn sync_page_parts(&mut self) {
        let doc = &self.body_editor().doc;
        let current = doc
            .trailing_section_properties()
            .map_or("", |section| section.raw.as_str());
        if current == self.page_parts_sect {
            return;
        }
        let page_parts = PageState::derive(&self.pkg, doc);
        self.headers = page_parts.headers;
        self.footers = page_parts.footers;
        self.title_page = page_parts.title_page;
        self.header_part = page_parts.header_part;
        self.footer_part = page_parts.footer_part;
        self.page_parts_sect = page_parts.sect;
    }

    fn refresh_watermark_state_if_needed(&mut self) {
        self.sync_page_parts();
        if !self
            .watermark_state
            .matches_document_sections(&self.editor.doc)
        {
            self.refresh_watermark_state();
        }
    }

    /// A compact status-line suffix for document-level notices (protection,
    /// watermark, page borders) — empty when the document has none.
    fn doc_notice(&self) -> String {
        let mut parts = Vec::new();
        if self.marked_final {
            parts.push("Marked as Final".to_string());
        }
        if let Some(p) = self.doc_protection.label() {
            parts.push(format!("Protected: {p}"));
        }
        if let Some(w) = self.watermark_state.label() {
            parts.push(format!("Watermark: {w}"));
        }
        if let Some(bg) = self.pkg.page_background() {
            parts.push(format!("Page color: {}", page_color_name(bg.color)));
        }
        if self.doc_page_borders {
            parts.push("Page border".to_string());
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!("  ·  {}", parts.join(" · "))
        }
    }

    /// The single App-level authorization decision used by interactive,
    /// control, and MCP mutation routes.
    fn authorize_mutation(
        &self,
        mutation: protection::MutationKind,
    ) -> Result<(), protection::ProtectionDenial> {
        if self.marked_final {
            return Err(protection::ProtectionDenial::MarkedFinal);
        }
        // A view whose text is not the document's is not edited; comments
        // hold no text of it.
        if !self.markup.is_editable() && mutation != protection::MutationKind::Comment {
            return Err(protection::ProtectionDenial::DisplayMode);
        }
        protection::authorize(&self.doc_protection, mutation)
    }

    /// Word's Edit Anyway on a document marked as final (#617): editing is
    /// allowed again, and the mark leaves the package so a later save writes
    /// an ordinary document. Not an edit: the document stays unmodified.
    /// Returns whether the document was final.
    pub(crate) fn edit_anyway(&mut self) -> bool {
        if !self.marked_final {
            return false;
        }
        self.marked_final = false;
        self.pkg.clear_marked_final();
        self.status = Some("Editing enabled: the document is no longer marked as final".into());
        self.dirty = true;
        true
    }

    /// Check a requested interactive mutation before it reaches the editor,
    /// history, package, comments, or save-state implementation. A denial
    /// replaces the transient status message, and a document marked as final
    /// also asks Edit Anyway (#617); document state stays untouched.
    fn mutation_allowed(&mut self, mutation: protection::MutationKind) -> bool {
        match self.authorize_mutation(mutation) {
            Ok(()) => true,
            Err(protection::ProtectionDenial::MarkedFinal) => {
                // Word's question, asked where the edit was refused; the
                // refused key stays dropped either way.
                if self.confirm.is_none() {
                    self.confirm = Some(backstage::Confirm::new(
                        MARKED_FINAL_PROMPT,
                        ConfirmAction::EditAnyway,
                        Color::LightBlue,
                    ));
                }
                self.status = Some(protection::ProtectionDenial::MarkedFinal.tui_status());
                self.dirty = true;
                false
            }
            Err(denial) => {
                self.status = Some(denial.tui_status());
                false
            }
        }
    }

    /// Mutating ribbon actions that execute immediately. Actions which only
    /// open a dialog are authorized by the dialog's commit method instead.
    fn ribbon_mutation_kind(act: ribbon::Act) -> Option<protection::MutationKind> {
        use protection::MutationKind;
        use ribbon::Act::*;
        match act {
            PageBreak | InsertTable => Some(MutationKind::Structure),
            PageNumber | ChangeCase => Some(MutationKind::Content),
            Sort => Some(MutationKind::Structure),
            HorizontalLine | Columns | Hyphenation | Bold | Italic | Underline | Strike
            | Subscript | Superscript | GrowFont | ShrinkFont | ClearFormatting
            | IncreaseIndent | DecreaseIndent | FirstLineIndent | HangingIndent | AlignLeft
            | AlignCenter | AlignRight | Justify | ApplyStyle(_) => Some(MutationKind::Formatting),
            _ => None,
        }
    }

    /// Classify direct body/header/footer keys after all modal and Vim routes
    /// have had a chance to consume them.
    fn body_key_mutation_kind(key: &KeyEvent) -> Option<protection::MutationKind> {
        use protection::MutationKind;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Left | KeyCode::Right if alt && shift => None,
            KeyCode::Char('a' | 'A' | 'r' | 'R') if alt && shift => Some(MutationKind::Content),
            KeyCode::Char('f') if alt => None,
            KeyCode::Char(' ') if ctrl && shift => Some(MutationKind::Content),
            KeyCode::Char(
                'b' | 'i' | 'u' | ']' | '[' | '=' | '+' | ' ' | 'm' | 'l' | 'e' | 'r' | 'j',
            ) if ctrl => Some(MutationKind::Formatting),
            KeyCode::Char('z' | 'y') if ctrl => Some(MutationKind::Content),
            KeyCode::F(3) if shift => Some(MutationKind::Content),
            KeyCode::Char(_) if !ctrl => Some(MutationKind::Content),
            KeyCode::Enter | KeyCode::Backspace | KeyCode::Delete => Some(MutationKind::Structure),
            KeyCode::Tab => Some(MutationKind::Content),
            _ => None,
        }
    }

    /// The comments review side panel: each comment's author/date, the quoted
    /// span it anchors to, and its text, scrollable with the wheel.
    /// The next free comment id (max existing + 1).
    fn next_comment_id(&self) -> i32 {
        // An undone comment keeps its id: redo brings it back. Ids whose
        // markers are in the body, a header or a footer are taken too (an
        // orphan's): a new comment sharing one would stay live after its
        // own undo.
        let marked = self.comment_marker_ids_everywhere();
        self.comments
            .iter()
            .map(|c| &c.id)
            .chain(self.tracked_comments.keys())
            .chain(&self.used_comment_ids)
            .chain(&marked)
            .filter_map(|id| id.parse::<i32>().ok())
            .max()
            .unwrap_or(0)
            + 1
    }

    /// Begin a new comment on the selection (prompts for the body in the status bar).
    fn start_comment(&mut self) {
        if !self.editor.has_selection() {
            self.status = Some("Select text to comment on first".to_string());
            self.dirty = true;
            return;
        }
        self.comment_input = Some(String::new());
        self.dirty = true;
    }

    /// Commit the new comment: wrap the selection in markers (one undo step)
    /// and add it to the live panel. Each save writes it to comments.xml
    /// while its markers are in the body, or the header or footer it was
    /// made in ([`App::reconcile_tracked_comments`]).
    fn commit_comment(&mut self) {
        if self
            .comment_input
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            self.comment_input = None;
            self.status = Some("Comment cancelled (empty)".to_string());
            self.dirty = true;
            return;
        }
        if !self.mutation_allowed(protection::MutationKind::Comment) {
            return;
        }
        let text = self.comment_input.take().unwrap_or_default();
        let quoted = self.editor.selection_text();
        let id = self.next_comment_id();
        if !self.editor.add_comment(&id.to_string()) {
            self.status = Some("No selection to comment on".to_string());
            self.dirty = true;
            return;
        }
        let author = self.review_author();
        let comment = docxcore::comments::Comment {
            id: id.to_string(),
            initials: docxcore::comments::initials(&author),
            author,
            date: utc_now_iso(),
            text,
            quoted,
            ..Default::default()
        };
        self.tracked_comments.insert(
            comment.id.clone(),
            TrackedComment {
                comment: comment.clone(),
                raw: None,
                index: self.comments.len(),
            },
        );
        self.used_comment_ids.insert(comment.id.clone());
        self.comments.push(comment);
        self.comment_active = true;
        self.comment_sel = self.comments.len() - 1;
        self.show_comments = true;
        self.after_edit();
        self.status = Some("Comment added".to_string());
    }

    /// Every comment id with a marker in the body, in any header or footer
    /// part (every section's), or in the header or footer being edited,
    /// whose part in `pkg` is stale until the edit is committed.
    fn comment_marker_ids_everywhere(&self) -> std::collections::BTreeSet<String> {
        let mut ids = docxcore::inspect::comment_marker_ids(&self.body_editor().doc);
        let editing = self.hf_edit.as_ref().map(|hf| hf.part.as_str());
        for name in self.pkg.part_names() {
            let hf = name.starts_with("word/header") || name.starts_with("word/footer");
            if hf && name.ends_with(".xml") && Some(name) != editing {
                if let Some(xml) = self.pkg.part_text(name) {
                    ids.extend(docxcore::inspect::comment_marker_ids_in_xml(&xml));
                }
            }
        }
        if self.hf_edit.is_some() {
            ids.extend(docxcore::inspect::comment_marker_ids_in_blocks(
                &self.editor.doc.body,
            ));
        }
        ids
    }

    /// Show a tracked comment only while a marker with its id is in the
    /// body or a header or footer: undo of Add Comment hides it, undo of
    /// Delete Comment shows it again, where it was. Untracked comments
    /// (loaded and never deleted) are never touched.
    fn sync_tracked_comments(&mut self) {
        if self.tracked_comments.is_empty() {
            return;
        }
        let live = self.comment_marker_ids_everywhere();
        let tracked = &self.tracked_comments;
        let selected = self.comments.get(self.comment_sel).map(|c| c.id.clone());
        let before = self.comments.len();
        self.comments
            .retain(|c| !tracked.contains_key(&c.id) || live.contains(&c.id));
        let mut changed = self.comments.len() != before;
        let mut revived: Vec<&TrackedComment> = tracked
            .iter()
            .filter(|(id, _)| live.contains(*id) && !self.comments.iter().any(|x| &x.id == *id))
            .map(|(_, t)| t)
            .collect();
        revived.sort_by_key(|t| t.index);
        for t in revived {
            let at = t.index.min(self.comments.len());
            self.comments.insert(at, t.comment.clone());
            changed = true;
        }
        if changed {
            self.comment_sel = selected
                .and_then(|id| self.comments.iter().position(|c| c.id == id))
                .unwrap_or(self.comment_sel)
                .min(self.comments.len().saturating_sub(1));
            self.comment_active &= !self.comments.is_empty();
        }
    }

    /// Bring `pkg`'s comments.xml in line with the tracked comments before
    /// a save: write each live one it lacks (a deleted loaded one as its
    /// original XML), drop each one with no marker left that it holds.
    /// `pkg` is reloaded from every save, so it may hold one already.
    fn reconcile_tracked_comments(&mut self) {
        self.reconcile_tracked_records();
        self.sync_resolved_to_pkg();
    }

    /// Write each comment's resolved state into `pkg`'s `commentsExtended`
    /// where it differs, and drop the comment parts when none is left.
    fn sync_resolved_to_pkg(&mut self) {
        let stored: std::collections::HashMap<String, bool> =
            docxcore::comments::parse_comments(&self.pkg)
                .into_iter()
                .map(|c| (c.id, c.resolved))
                .collect();
        for c in &self.comments {
            if stored.get(&c.id).is_some_and(|&r| r != c.resolved) {
                self.pkg.set_comment_resolved(&c.id, c.resolved);
            }
        }
        if !self.tracked_comments.is_empty() {
            self.pkg.drop_empty_comment_parts();
        }
    }

    fn reconcile_tracked_records(&mut self) {
        if self.tracked_comments.is_empty() {
            return;
        }
        let live = self.comment_marker_ids_everywhere();
        // Ids as written, from the part as decoded: `03` is not `3`, and a
        // UTF-16 part's comments count (#971).
        let saved: std::collections::HashSet<String> = self.pkg.comment_ids().into_iter().collect();
        for (id, t) in &self.tracked_comments {
            let c = &t.comment;
            match (live.contains(id), saved.contains(id), &t.raw) {
                (true, false, Some(raw)) => self.pkg.insert_comment_xml(raw),
                (true, false, None) => {
                    if let Ok(n) = id.parse::<i32>() {
                        self.pkg
                            .add_comment(n, &c.author, &c.initials, &c.date, &c.text)
                    }
                }
                (false, true, _) => self.pkg.remove_comment_id(id),
                _ => {}
            }
        }
    }

    /// Delete the navigation-selected comment: its markers (one undo step)
    /// and its panel entry. Its record stays, tracked, so the save leaves it
    /// out and an undo brings it back whole (#971); one with markers left
    /// in a header or footer not being edited is removed from `pkg` now,
    /// as before, since nothing could take those markers.
    fn delete_comment(&mut self) {
        if self.comments.is_empty() {
            self.status = Some("No comments to delete".to_string());
            self.dirty = true;
            return;
        }
        if !self.mutation_allowed(protection::MutationKind::Comment) {
            return;
        }
        let idx = self.comment_sel.min(self.comments.len() - 1);
        let c = self.comments.remove(idx);
        // The body's markers, even while a header or footer is being
        // edited (`editor` is then that part's); and that part's own, for
        // a comment anchored there. Each is a step only where it removed.
        self.body_editor_mut().remove_comment_markers(&c.id);
        if self.hf_edit.is_some() {
            self.editor.remove_comment_markers(&c.id);
        }
        if self.comment_marker_ids_everywhere().contains(&c.id) {
            self.tracked_comments.remove(&c.id);
            self.pkg.remove_comment_id(&c.id);
        } else if let Some(t) = self.tracked_comments.get_mut(&c.id) {
            t.index = idx;
            // A comment added this session and saved since is in `pkg`
            // now: keep that XML too.
            if t.raw.is_none() {
                t.raw = self.pkg.comment_xml(&c.id);
            }
        } else {
            let raw = self.pkg.comment_xml(&c.id);
            self.tracked_comments.insert(
                c.id.clone(),
                TrackedComment {
                    comment: c.clone(),
                    raw,
                    index: idx,
                },
            );
        }
        self.comment_sel = idx.min(self.comments.len().saturating_sub(1));
        self.comment_active = !self.comments.is_empty();
        self.after_edit();
        self.status = Some(format!("Deleted comment by {}", c.author));
    }

    /// Resolve the navigation-selected comment, or reopen it when it is
    /// resolved. The state is the comment's own (it follows the record of a
    /// deleted one), and each save writes it to `commentsExtended.xml`.
    fn resolve_comment(&mut self) {
        if self.comments.is_empty() {
            self.status = Some("No comments to resolve".to_string());
            self.dirty = true;
            return;
        }
        if !self.mutation_allowed(protection::MutationKind::Comment) {
            return;
        }
        let id = self.comments[self.comment_sel.min(self.comments.len() - 1)]
            .id
            .clone();
        if let Some(resolved) = self.set_comment_resolved(&id, None) {
            self.status = Some(if resolved {
                "Comment resolved".to_string()
            } else {
                "Comment reopened".to_string()
            });
        }
    }

    /// Set comment `id` resolved (`Some(true)`), reopened (`Some(false)`), or
    /// the other of the two (`None`). The new state, or `None` when no
    /// listed comment has that id. No protection check: callers authorize.
    fn set_comment_resolved(&mut self, id: &str, resolved: Option<bool>) -> Option<bool> {
        let c = self.comments.iter_mut().find(|c| c.id == id)?;
        c.resolved = resolved.unwrap_or(!c.resolved);
        let now = c.resolved;
        if let Some(t) = self.tracked_comments.get_mut(id) {
            t.comment.resolved = now;
        }
        self.modified = true;
        self.dirty = true;
        Some(now)
    }

    /// Delete every comment: all markers (one undo step per editor that
    /// held some), and each comment's record kept, tracked, so an undo brings
    /// back markers and record whole (#971), like [`App::delete_comment`].
    fn delete_all_comments(&mut self) {
        if self.comments.is_empty() && self.comment_marker_ids_everywhere().is_empty() {
            self.status = Some("No comments to delete".to_string());
            self.dirty = true;
            return;
        }
        if !self.mutation_allowed(protection::MutationKind::Comment) {
            return;
        }
        let n = self.remove_all_comments();
        self.status = Some(format!("Deleted all comments ({n})"));
    }

    /// [`App::delete_all_comments`] without the checks: how many listed
    /// comments went. No protection check: callers authorize.
    fn remove_all_comments(&mut self) -> usize {
        self.body_editor_mut().remove_all_comment_markers();
        if self.hf_edit.is_some() {
            self.editor.remove_all_comment_markers();
        }
        let still_marked = self.comment_marker_ids_everywhere();
        let comments = std::mem::take(&mut self.comments);
        for (idx, c) in comments.iter().enumerate() {
            if still_marked.contains(&c.id) {
                // Markers in a header or footer not being edited: as in
                // Delete Comment, nothing could take them, so it goes now.
                self.tracked_comments.remove(&c.id);
                self.pkg.remove_comment_id(&c.id);
            } else if let Some(t) = self.tracked_comments.get_mut(&c.id) {
                t.index = idx;
                t.comment = c.clone();
                if t.raw.is_none() {
                    t.raw = self.pkg.comment_xml(&c.id);
                }
            } else {
                let raw = self.pkg.comment_xml(&c.id);
                self.tracked_comments.insert(
                    c.id.clone(),
                    TrackedComment {
                        comment: c.clone(),
                        raw,
                        index: idx,
                    },
                );
            }
        }
        self.comment_sel = 0;
        self.comment_active = false;
        self.after_edit();
        comments.len()
    }

    fn comment_input_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.comment_input = None;
                self.status = Some("Comment cancelled".to_string());
                self.dirty = true;
            }
            KeyCode::Enter => self.commit_comment(),
            KeyCode::Backspace => {
                if let Some(s) = self.comment_input.as_mut() {
                    s.pop();
                }
                self.dirty = true;
            }
            KeyCode::Char(c) => {
                if let Some(s) = self.comment_input.as_mut() {
                    s.push(c);
                }
                self.dirty = true;
            }
            _ => {}
        }
        false
    }

    /// Move the comment selection by `delta`, reveal it in the panel, and jump the
    /// caret to the comment's anchored text (Review ▸ Previous/Next).
    fn nav_comment(&mut self, delta: i32) {
        if self.comments.is_empty() {
            self.status = Some("No comments".to_string());
            self.dirty = true;
            return;
        }
        self.show_comments = true;
        let n = self.comments.len() as i32;
        // The first Prev/Next lands on the first (or last) comment rather than
        // stepping past it.
        if !self.comment_active {
            self.comment_active = true;
            self.comment_sel = if delta >= 0 { 0 } else { (n - 1) as usize };
        } else {
            self.comment_sel = (self.comment_sel as i32 + delta).rem_euclid(n) as usize;
        }
        // Jump the caret to the first occurrence of the comment's anchored text.
        let quoted = self.comments[self.comment_sel].quoted.clone();
        if !quoted.is_empty() {
            let ms = self.editor.find_all(&quoted, false);
            if let Some(m) = ms.first() {
                self.editor.select_match(m);
                self.follow_caret = true;
            }
        }
        // Scroll the panel so the selected comment's header is visible.
        let inner_w = (self.comments_rect.width as usize).saturating_sub(2).max(4);
        let (_, headers) = self.comment_panel_lines(inner_w);
        if let Some(&h) = headers.get(self.comment_sel) {
            self.comments_scroll = h;
        }
        let who = {
            let a = &self.comments[self.comment_sel].author;
            if a.is_empty() { "Unknown" } else { a.as_str() }
        };
        self.status = Some(format!("Comment {}/{} — {who}", self.comment_sel + 1, n));
        self.dirty = true;
    }

    /// Build the comments-panel lines (wrapped to `inner_w`) plus the line index of
    /// each comment's header, highlighting the Prev/Next-selected comment.
    fn comment_panel_lines(&self, inner_w: usize) -> (Vec<RLine<'static>>, Vec<usize>) {
        let head = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let sel_head = Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let quote = Style::default().fg(Color::Yellow);
        let mut lines: Vec<RLine> = Vec::new();
        let mut headers: Vec<usize> = Vec::new();
        for (i, c) in self.comments.iter().enumerate() {
            if i > 0 {
                lines.push(RLine::raw(""));
            }
            headers.push(lines.len());
            let who = if c.author.is_empty() {
                "Unknown".to_string()
            } else {
                c.author.clone()
            };
            let date = c.date.split('T').next().unwrap_or("").to_string();
            let hstyle = if self.comment_active && i == self.comment_sel {
                sel_head
            } else {
                head
            };
            let mark = if c.resolved { "✓" } else { "▣" };
            let state = if c.resolved { "  (resolved)" } else { "" };
            lines.push(RLine::styled(
                format!("{mark} {who}  {date}{state}"),
                hstyle,
            ));
            if !c.quoted.is_empty() {
                for w in wrap_str(&format!("“{}”", c.quoted), inner_w) {
                    lines.push(RLine::styled(w, quote));
                }
            }
            for para in c.text.split('\n') {
                for w in wrap_str(para, inner_w) {
                    lines.push(RLine::raw(w));
                }
            }
        }
        (lines, headers)
    }

    fn draw_comments_panel(&self, f: &mut Frame, area: Rect) {
        let inner_w = area.width.saturating_sub(2).max(4) as usize;
        let inner_h = area.height.saturating_sub(2).max(1) as usize;
        let (lines, _) = self.comment_panel_lines(inner_w);
        let total = lines.len();
        let scroll = self.comments_scroll.min(total.saturating_sub(inner_h));
        let shown: Vec<RLine> = lines.into_iter().skip(scroll).take(inner_h).collect();
        let title = format!(" Comments ({}) ", self.comments.len());
        f.render_widget(
            Paragraph::new(shown).block(
                RBlock::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(title),
            ),
            area,
        );
        if total > inner_h {
            let mut sb = ScrollbarState::new(total)
                .position(scroll)
                .viewport_content_length(inner_h);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                area.inner(ratatui::layout::Margin {
                    vertical: 1,
                    horizontal: 0,
                }),
                &mut sb,
            );
        }
    }

    /// Build the notes side-panel content (footnotes then endnotes) wrapped to
    /// `inner_w`.
    fn note_panel_lines(&self, inner_w: usize) -> Vec<RLine<'static>> {
        let head = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let mut lines: Vec<RLine> = Vec::new();
        for (i, n) in self.notes.iter().enumerate() {
            if i > 0 {
                lines.push(RLine::raw(""));
            }
            let kind = if n.endnote { "Endnote" } else { "Footnote" };
            lines.push(RLine::styled(format!("{kind} {}", n.id), head));
            for para in n.text.split('\n') {
                for w in wrap_str(para, inner_w) {
                    lines.push(RLine::raw(w));
                }
            }
        }
        lines
    }

    fn draw_notes_panel(&self, f: &mut Frame, area: Rect) {
        let inner_w = area.width.saturating_sub(2).max(4) as usize;
        let inner_h = area.height.saturating_sub(2).max(1) as usize;
        let lines = self.note_panel_lines(inner_w);
        let total = lines.len();
        let scroll = self.notes_scroll.min(total.saturating_sub(inner_h));
        let shown: Vec<RLine> = lines.into_iter().skip(scroll).take(inner_h).collect();
        let f_count = self.notes.iter().filter(|n| !n.endnote).count();
        let e_count = self.notes.len() - f_count;
        let title = format!(" Notes ({f_count} fn / {e_count} en) ");
        f.render_widget(
            Paragraph::new(shown).block(
                RBlock::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(title),
            ),
            area,
        );
        if total > inner_h {
            let mut sb = ScrollbarState::new(total)
                .position(scroll)
                .viewport_content_length(inner_h);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                area.inner(ratatui::layout::Margin {
                    vertical: 1,
                    horizontal: 0,
                }),
                &mut sb,
            );
        }
    }

    /// A column ruler row aligned with the document's left edge.
    fn draw_ruler(&self, f: &mut Frame, area: Rect) {
        let mut s = String::with_capacity(area.width as usize);
        for c in 0..area.width as usize {
            if c % 10 == 0 {
                s.push(char::from_digit(((c / 10) % 10) as u32, 10).unwrap_or('|'));
            } else if c % 5 == 0 {
                s.push('+');
            } else {
                s.push('·');
            }
        }
        f.render_widget(
            Paragraph::new(RLine::styled(
                s,
                Style::default().add_modifier(Modifier::DIM),
            )),
            area,
        );
    }

    /// The navigation (outline) pane: the document's headings, click to jump.
    fn draw_nav_pane(&mut self, f: &mut Frame) {
        let area = self.nav_rect;
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut items: Vec<(String, usize)> = Vec::new();
        for (bi, block) in self.editor.doc.body.iter().enumerate() {
            if let Block::Paragraph(p) = block {
                if let Some(lvl) = p.props.heading_level {
                    let text = p.plain_text().trim().to_string();
                    if text.is_empty() {
                        continue;
                    }
                    let line = self
                        .maps
                        .iter()
                        .position(|m| m.segs.iter().any(|s| s.path.first() == Some(&bi)))
                        .unwrap_or(0);
                    let indent = "  ".repeat(lvl.saturating_sub(1) as usize);
                    items.push((format!("{indent}{text}"), line));
                }
            }
        }
        self.nav_items = items;

        let inner_w = area.width.saturating_sub(2) as usize;
        let inner_h = area.height.saturating_sub(2) as usize;
        let body: Vec<RLine> = if self.nav_items.is_empty() {
            vec![RLine::styled("(no headings)", dim)]
        } else {
            self.nav_items
                .iter()
                .take(inner_h)
                .map(|(t, _)| RLine::raw(fit_width(t, inner_w)))
                .collect()
        };
        f.render_widget(
            Paragraph::new(body).block(
                RBlock::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(" Navigation "),
            ),
            area,
        );
    }

    fn ensure_rendered(&mut self, width: u16) {
        if self.dirty || width != self.rendered_width {
            let opts = self.options(width);
            let shown = self.editor.doc.markup_view(self.markup);
            let rendered = render_with_page_layout(&shown, &opts);
            let mut lines = rendered.lines;
            let mut maps = rendered.maps;
            if !self.markup.is_editable() {
                // The caret stops of a text that is not the document's: none,
                // so a click or a vertical move cannot take an offset from it.
                maps.iter_mut().for_each(|m| *m = LineMap::default());
            }
            let mut images = rendered.images;
            let pages = rendered.pages;
            // While editing a header/footer, show the rest of the page (the parked
            // document body) dimmed and read-only below/above the editable surface,
            // the way Word greys out the body. The body's caret maps are dropped so
            // the caret stays in the header/footer being edited.
            if let Some(hf) = &self.hf_edit {
                let (mut body, _bm, _bi, _bmmd) = render_with_images(&hf.body.doc, &opts);
                for l in &mut body {
                    for s in &mut l.spans {
                        s.style.dim = true;
                        s.style.highlight = false;
                        s.style.color = None;
                    }
                }
                let body_maps: Vec<LineMap> = body.iter().map(|_| LineMap::default()).collect();
                let sep = DocLine {
                    spans: vec![DocSpan {
                        text: "─".repeat(width.max(1) as usize),
                        style: DocStyle {
                            dim: true,
                            ..DocStyle::default()
                        },
                        link: None,
                    }],
                };
                if hf.is_header {
                    // Header (editable) on top, greyed body beneath.
                    lines.push(sep);
                    maps.push(LineMap::default());
                    lines.extend(body);
                    maps.extend(body_maps);
                } else {
                    // Greyed body on top, footer (editable) at the bottom. The
                    // editable surface (and its images) shift down past the body.
                    let shift = body.len() + 1;
                    for ib in &mut images {
                        ib.row += shift;
                    }
                    let mut nl = body;
                    let mut nm = body_maps;
                    nl.push(sep);
                    nm.push(LineMap::default());
                    nl.append(&mut lines);
                    nm.append(&mut maps);
                    lines = nl;
                    maps = nm;
                }
            }
            let watermark_overlays = if self.hf_edit.is_none() {
                watermark::layout(&self.watermark_state, &pages, &lines, &images)
            } else {
                Vec::new()
            };
            self.lines = lines;
            self.maps = maps;
            self.images = images;
            self.watermark_overlays = watermark_overlays;
            self.rendered_width = width;
            self.dirty = false;
        }
    }

    /// Ensure `img_cache[key]` holds a protocol encoding exactly the visible
    /// window `(wtop, wh, w)` (cells, where `wtop` is absolute in the full image)
    /// of the image `rid` scaled to its full height `br`. A split image uses a
    /// distinct `key` per slice so simultaneously-visible slices don't thrash one
    /// cache entry. `None` is cached when the bytes are missing or undecodable
    /// (e.g. WMF/EMF) so the placeholder box stays.
    fn refresh_image(
        &mut self,
        key: &str,
        rid: &str,
        bc: usize,
        br: usize,
        win: (usize, usize, usize),
    ) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let (fw, fh) = {
            let fs = picker.font_size();
            (fs.0 as usize, fs.1 as usize)
        };
        let (wtop, wh, w) = win;
        let rebuild = match self.img_cache.get(key) {
            Some(Some(st)) => st.box_cols != bc || st.box_rows != br,
            Some(None) => return,
            None => true,
        };
        // Encode the cropped window once at its exact cell size (Fit is 1:1 since
        // the crop already matches), so re-emitting at any scroll position is free.
        let encode = |picker: &Picker, src: &image::DynamicImage| -> Option<Protocol> {
            let cropped = src.crop_imm(
                0,
                (wtop * fh) as u32,
                (w * fw).max(1) as u32,
                (wh * fh).max(1) as u32,
            );
            let size = Rect {
                x: 0,
                y: 0,
                width: w as u16,
                height: wh as u16,
            };
            picker.new_protocol(cropped, size, Resize::Fit(None)).ok()
        };
        if rebuild {
            // Decode the source and scale it to the box's full pixel size (once).
            let decoded = self.rels.target(rid).and_then(|t| {
                let name = match t.strip_prefix('/') {
                    Some(r) => r.to_string(),
                    None => format!("word/{}", t.trim_start_matches("./")),
                };
                self.pkg.part(&name)
            });
            let (pw, ph) = ((bc * fw).max(1) as u32, (br * fh).max(1) as u32);
            // Decode raster formats directly; fall back to GDI for WMF/EMF vectors.
            let src = decoded.and_then(|b| {
                image::load_from_memory(b)
                    .ok()
                    .or_else(|| metafile::render(b, pw, ph).map(image::DynamicImage::ImageRgba8))
            });
            let Some(src) = src else {
                self.img_cache.insert(key.to_string(), None);
                return;
            };
            let resized = src.resize_exact(pw, ph, image::imageops::FilterType::Triangle);
            let entry = encode(picker, &resized).map(|proto| ImgState {
                resized,
                box_cols: bc,
                box_rows: br,
                win,
                proto,
            });
            self.img_cache.insert(key.to_string(), entry);
            return;
        }
        if let Some(Some(st)) = self.img_cache.get_mut(key) {
            if st.win != win {
                if let Some(proto) = encode(picker, &st.resized) {
                    st.proto = proto;
                    st.win = win;
                }
            }
        }
    }

    /// Paint each image: real pixels when we can decode and the terminal supports
    /// graphics, otherwise a fallback box (border + caption) — the only time we
    /// draw a border around a borderless picture. Cropped at the viewport edges.
    fn draw_images(&mut self, f: &mut Frame, content: Rect) {
        let has_picker = self.picker.is_some();
        let (scroll, vh) = (self.scroll, self.viewport_h);
        for ib in self.images.clone() {
            // Visible window of the box, in box-relative cells.
            let wtop = scroll.saturating_sub(ib.row);
            let wbot = (scroll + vh).saturating_sub(ib.row).min(ib.rows);
            if wbot <= wtop || ib.col >= content.width as usize {
                continue;
            }
            let wh = wbot - wtop;
            let w = ib.cols.min(content.width as usize - ib.col);
            if w == 0 {
                continue;
            }
            let x = content.x + ib.col as u16;
            let y = content.y + (ib.row + wtop - scroll) as u16;
            let rect = Rect {
                x,
                y,
                width: w as u16,
                height: wh as u16,
            };
            // Try real pixels first: crop the source band for this slice (absolute
            // top within the full image = the slice's offset plus scrolled-away rows).
            let mut drawn = false;
            if has_picker && !ib.rid.is_empty() {
                let key = format!("{}#{}", ib.rid, ib.src_row);
                self.refresh_image(
                    &key,
                    &ib.rid,
                    ib.cols,
                    ib.full_rows,
                    (ib.src_row + wtop, wh, w),
                );
                if let Some(Some(st)) = self.img_cache.get(&key) {
                    f.render_widget(Image::new(&st.proto), rect);
                    drawn = true;
                }
            }
            // A borderless picture we couldn't render falls back to a box so the
            // reader still sees something is there. A bordered picture already has
            // its outline drawn into the text, so nothing extra is needed.
            if !drawn && !ib.bordered {
                draw_fallback_box(f, content, &ib, scroll, &ib.label);
            }
        }
    }

    fn caret_screen(&self) -> Option<(usize, usize)> {
        let c = &self.editor.caret;
        // A caret offset at a soft-wrap boundary matches two adjacent lines (the
        // end of one, the start of the next). Bidi run boundaries can also put
        // multiple logical stops on one terminal column. Collect every match; if
        // a visual hint points at one of them (and is still valid for this caret),
        // trust it so movement continues from the intended screen position.
        // Otherwise resolve to the last (lower) line, matching how a fresh caret
        // reads.
        let mut matches: Vec<(usize, usize)> = Vec::new();
        let hint = self
            .visual_hint
            .as_ref()
            .filter(|(hint_caret, _, _)| hint_caret == c);
        for (i, m) in self.maps.iter().enumerate() {
            let preferred_col = hint.and_then(|(_, row, col)| (*row == i).then_some(*col));
            if let Some(col) = m.col_for_caret(&c.path, c.offset, preferred_col) {
                matches.push((i, col));
            }
        }
        if let Some((_, hint_row, _)) = hint {
            if let Some(m) = matches.iter().find(|(r, _)| r == hint_row) {
                return Some(*m);
            }
        }
        matches.last().copied()
    }

    fn clear_visual_hint(&mut self) {
        self.visual_hint = None;
        self.vertical_col_hint = None;
    }

    fn set_visual_hint(&mut self, row: usize, col: usize) {
        self.visual_hint = Some((self.editor.caret.clone(), row, col));
    }

    fn set_visual_caret(&mut self, row: usize, caret: LineCaret) {
        self.editor.set_caret(Caret::at(caret.path, caret.offset));
        self.set_visual_hint(row, caret.col);
    }

    fn move_visual_horiz(&mut self, right: bool) {
        let Some((row, col)) = self.caret_screen() else {
            self.clear_visual_hint();
            if right {
                self.editor.move_right();
            } else {
                self.editor.move_left();
            }
            return;
        };
        let cur = self.editor.caret.clone();
        if let Some(caret) = self
            .maps
            .get(row)
            .and_then(|map| map.visual_neighbor(&cur.path, cur.offset, Some(col), right))
        {
            self.set_visual_caret(row, caret);
            self.vertical_col_hint = None;
            return;
        }

        let rows: Box<dyn Iterator<Item = usize>> = if right {
            Box::new(row + 1..self.maps.len())
        } else {
            Box::new((0..row).rev())
        };
        for r in rows {
            let edge = self.maps[r].edge_caret(!right);
            if let Some(caret) = edge {
                self.set_visual_caret(r, caret);
                self.vertical_col_hint = None;
                return;
            }
        }
        self.vertical_col_hint = None;
    }

    fn move_visual_line_edge(&mut self, right: bool) {
        let Some((row, _)) = self.caret_screen() else {
            self.clear_visual_hint();
            if right {
                self.editor.move_end();
            } else {
                self.editor.move_home();
            }
            return;
        };
        if let Some(caret) = self.maps.get(row).and_then(|map| map.edge_caret(right)) {
            self.set_visual_caret(row, caret);
            self.vertical_col_hint = None;
        }
    }

    fn move_vert(&mut self, down: bool) {
        let Some((row, col)) = self.caret_screen() else {
            return;
        };
        let target_col = self.vertical_col_hint.unwrap_or(col);
        let rows: Box<dyn Iterator<Item = usize>> = if down {
            Box::new(row + 1..self.maps.len())
        } else {
            Box::new((0..row).rev())
        };
        for r in rows {
            if let Some(caret) = self.maps[r].nearest_caret(target_col) {
                self.set_visual_caret(r, caret);
                // Keep using the original desired column for a run of Up/Down
                // keys even when a short or reordered line snaps the caret.
                self.vertical_col_hint = Some(target_col);
                return;
            }
        }
    }

    /// Tab (`back`: Shift+Tab) with the caret in a table: select the next or
    /// previous cell's content. Tab in the last cell adds a row, which is an
    /// edit and needs structure permission; moving is not.
    fn table_tab_key(&mut self, back: bool) {
        self.clear_visual_hint();
        if back {
            self.editor.table_prev_cell();
            self.dirty = true;
            return;
        }
        if self.editor.table_tab_adds_row() {
            if !self.mutation_allowed(protection::MutationKind::Structure) {
                return;
            }
            self.editor.table_next_cell();
            self.after_edit();
        } else {
            self.editor.table_next_cell();
            self.dirty = true;
        }
    }

    fn after_edit(&mut self) {
        self.modified = true;
        self.dirty = true;
        self.status = None;
        self.clear_visual_hint();
        self.sync_tracked_comments();
        // An edit with the find bar open (a ribbon Accept, a paste) moves the
        // text under its matches: rebuild them so Replace never acts on a
        // stale range. The caret and selection stay where the edit left them.
        if let Some(f) = self.find.as_mut() {
            f.matches = self.editor.find_visible(&f.query, false);
            if f.idx >= f.matches.len() {
                f.idx = 0;
            }
        }
        // The editable body lives outside `pkg`. Any successful body edit can
        // remove, restore, or move a paragraph carrying `w:sectPr`, so refresh
        // the page/header scope before the next overlay layout. Header/footer
        // edits use a temporary editor and refresh when that part is committed.
        if self.hf_edit.is_none() {
            self.refresh_watermark_state_if_needed();
            self.doc_page_borders = self
                .editor
                .sections()
                .iter()
                .any(|s| s.contains("<w:pgBorders"));
        }
    }

    /// Enter focus-editing of the default header (or footer): park the body
    /// editor and point the main editor at the header/footer document.
    fn enter_hf_edit(&mut self, is_header: bool) {
        if !self.mutation_allowed(protection::MutationKind::Content) {
            return;
        }
        if self.hf_edit.is_some() {
            self.exit_hf_edit(true);
        }
        self.sync_page_parts();
        let what = if is_header { "header" } else { "footer" };
        // Resolve the part, creating one from scratch if the document has none.
        let existing = if is_header {
            self.header_part.clone()
        } else {
            self.footer_part.clone()
        };
        let part = match existing {
            Some(p) => p,
            None => {
                match self.edit_final_sect_pr(true, |pkg| pkg.create_hf(is_header, "default")) {
                    Some(p) => {
                        if is_header {
                            self.header_part = Some(p.clone());
                        } else {
                            self.footer_part = Some(p.clone());
                        }
                        self.modified = true;
                        self.status = Some(format!("Created a {what}."));
                        p
                    }
                    None => {
                        self.status = Some(format!("Couldn't create a {what}."));
                        self.dirty = true;
                        return;
                    }
                }
            }
        };
        // Start from the current content, or one empty paragraph if new/empty.
        let src = if is_header {
            &self.headers.default
        } else {
            &self.footers.default
        };
        let body = if src.is_empty() {
            vec![Block::Paragraph(docxcore::model::Paragraph::default())]
        } else {
            src.as_ref().clone()
        };
        let new_editor = Editor::new(Document { body });
        let parked = std::mem::replace(&mut self.editor, new_editor);
        let saved_page_view = self.page_view;
        self.page_view = false;
        self.editor.clear_selection();
        self.hf_edit = Some(HfEdit {
            body: parked,
            is_header,
            part,
            saved_page_view,
        });
        if self.status.is_none() {
            self.status = Some(format!("Editing {what} — Esc/F6/F7 to return"));
        }
        self.dirty = true;
    }

    /// Return from header/footer editing, committing the edits (splice back into
    /// the part and update the print-layout source) when `commit`.
    fn exit_hf_edit(&mut self, commit: bool) {
        let Some(hf) = self.hf_edit.take() else {
            return;
        };
        let edited = std::mem::replace(&mut self.editor, hf.body);
        let blocks = edited.doc.body;
        let changed = if hf.is_header {
            self.headers.default.as_ref() != &blocks
        } else {
            self.footers.default.as_ref() != &blocks
        };
        if commit && changed {
            let tag = if hf.is_header { "w:hdr" } else { "w:ftr" };
            // Links need relationships in the part's own rels; they are
            // computed first (the ids go into the XML) but written only with it.
            let what = if hf.is_header { "header" } else { "footer" };
            let mut blocks = blocks;
            let written = self
                .pkg
                .link_part_hyperlinks(&hf.part, &mut blocks)
                .and_then(|rels| {
                    // Decode the part as the loader does (it may be UTF-16),
                    // and write it back in its own encoding; a part whose
                    // wrapper can't be found is left alone.
                    let xml = self
                        .pkg
                        .part_text(&hf.part)
                        .and_then(|orig| splice_hf(&orig, &blocks, tag))
                        .ok_or(if hf.is_header {
                            "the part isn't readable header XML"
                        } else {
                            "the part isn't readable footer XML"
                        })?;
                    Ok((xml, rels))
                });
            match written {
                Ok((new_xml, rels)) => {
                    self.pkg.set_part_text(&hf.part, &new_xml);
                    if let Some(rels) = rels {
                        self.pkg.apply_part_rels(rels);
                    }
                    let rc = Rc::new(blocks);
                    if hf.is_header {
                        self.headers.default = rc;
                    } else {
                        self.footers.default = rc;
                    }
                    self.refresh_watermark_state();
                    self.modified = true;
                }
                Err(why) => {
                    self.status = Some(format!(
                        "Couldn't write the {what} edit to {}: {why}.",
                        hf.part
                    ));
                }
            }
        }
        self.page_view = hf.saved_page_view;
        // The part's markers are now its stored XML's, or gone with a
        // discarded edit: the panel follows (#971).
        self.sync_tracked_comments();
        self.dirty = true;
    }

    /// Run a package edit of the final section's `w:sectPr` against the body
    /// editor's current copy, then mirror the result back into that editor's
    /// document — the one the page view shows, save writes and PDF export
    /// prints. `undoable` makes the mirror its own undo step; otherwise it
    /// rides on a checkpoint the caller just took.
    fn edit_final_sect_pr<R>(&mut self, undoable: bool, edit: impl FnOnce(&mut Package) -> R) -> R {
        let editor = match &mut self.hf_edit {
            Some(hf) => &mut hf.body,
            None => &mut self.editor,
        };
        if let Some(section) = editor.doc.trailing_section_properties() {
            self.pkg.set_trailing_section(section.clone());
        }
        let out = edit(&mut self.pkg);
        if let Some(section) = self.pkg.document.trailing_section_properties()
            && editor.doc.trailing_section_properties() != Some(section)
        {
            let section = section.clone();
            if undoable {
                editor.set_trailing_section_properties(section);
            } else {
                editor.doc.set_trailing_section_properties(section);
            }
        }
        self.sync_page_parts();
        out
    }

    /// Insert a section break at the caret: content up to here keeps the current
    /// page geometry; the rest of the document becomes a new section with the
    /// given orientation. (Works cleanly when the caret is in the final section.)
    fn insert_section(&mut self, landscape: bool) {
        if self.hf_edit.is_some() {
            return;
        }
        if !self.mutation_allowed(protection::MutationKind::Formatting) {
            return;
        }
        let current = self.edit_final_sect_pr(false, |pkg| pkg.sect_pr().to_string());
        let break_sect = with_page_size(&current);
        if !self.editor.set_caret_section_break(Some(break_sect)) {
            self.status = Some("Can't insert a section break here.".to_string());
            self.dirty = true;
            return;
        }
        // Rides on the section break's undo checkpoint, so Ctrl+Z restores the
        // old final section too.
        self.edit_final_sect_pr(false, |pkg| {
            pkg.set_sect_pr(orient_sectpr(&current, landscape))
        });
        self.refresh_watermark_state();
        self.modified = true;
        self.dirty = true;
        let o = if landscape { "landscape" } else { "portrait" };
        self.status = Some(format!(
            "Inserted a {o} section after the cursor (F2 to view)."
        ));
    }

    fn save(&mut self) {
        // Commit any in-progress header/footer edit first.
        if self.hf_edit.is_some() {
            self.exit_hf_edit(true);
        }
        let path = self.path.clone();
        if self.format == DocFormat::Markdown {
            let md = self.current_markdown();
            self.finish_save(&path, md.as_bytes(), None);
            return;
        }
        self.reconcile_tracked_comments();
        let docx = if self.modified {
            self.pkg.document = self.editor.doc.clone();
            save_package(&self.pkg)
        } else {
            save_package_preserving_document(&self.pkg)
        };
        if self.format == DocFormat::Docx {
            self.finish_save(&path, &docx, Some(&docx));
            return;
        }
        // An opened bundle keeps its engine and UI: only the payload changes.
        // Nothing but a bundle is ever written to an .html path.
        let Some(old) = self.bundle_html.as_deref() else {
            self.status = Some("save failed: the editable HTML this file came from is gone".into());
            return;
        };
        if let Err(e) = htmlbundle::check_html_target(Path::new(&path)) {
            self.status = Some(format!("save failed: {e}"));
            return;
        }
        match htmlbundle::rewrap(old, &docx) {
            Ok(page) => {
                if self.finish_save(&path, page.as_bytes(), Some(&docx)) {
                    self.bundle_html = Some(page);
                }
            }
            Err(e) => self.status = Some(format!("save failed: {e}")),
        }
    }

    /// Write `bytes` to `path` and settle the saved state; `docx` (the package
    /// just serialized) becomes the new base. Returns whether it was written.
    fn finish_save(&mut self, path: &str, bytes: &[u8], docx: Option<&[u8]>) -> bool {
        match write_atomic(Path::new(path), bytes) {
            Ok(()) => {
                if let Some(pkg) = docx.and_then(|d| load_package(d).ok()) {
                    self.pkg = pkg;
                }
                self.modified = false;
                self.status = Some(format!("Saved {} ({} bytes)", path, bytes.len()));
                true
            }
            Err(e) => {
                self.status = Some(format!("save failed: {e}"));
                false
            }
        }
    }

    /// The raw source text of the editor, one paragraph per line. Only meaningful
    /// in Markdown source view, where each line of source is its own paragraph.
    fn source_text(&self) -> String {
        self.editor
            .doc
            .body
            .iter()
            .map(|b| b.plain_text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The current document as Markdown text, regardless of which view is active.
    fn current_markdown(&self) -> String {
        if self.md_source {
            self.source_text()
        } else {
            let markers = compute_markers(&self.editor.doc, &self.numbering);
            to_markdown_with(&self.editor.doc, &markers)
        }
    }

    /// The canonical rendered [`Document`] for the current state — parsing the raw
    /// source first when in Markdown source view, so a Save As to `.docx` always
    /// gets a real document tree.
    fn current_document(&self) -> Document {
        if self.format == DocFormat::Markdown && self.md_source {
            from_markdown(&self.source_text())
        } else {
            self.editor.doc.clone()
        }
    }

    /// Switch a Markdown file between rendered and raw-source editing. Converts the
    /// editor buffer in place (rendered ⇄ Markdown text) so edits in either view
    /// carry over. A no-op for `.docx` or when already in the requested view.
    fn set_md_source(&mut self, source: bool) {
        if self.format != DocFormat::Markdown || self.md_source == source {
            return;
        }
        if self.hf_edit.is_some() {
            self.exit_hf_edit(true);
        }
        let doc = if source {
            // Render the live document to Markdown, then edit it as literal lines.
            let markers = compute_markers(&self.editor.doc, &self.numbering);
            source_lines_to_doc(&to_markdown_with(&self.editor.doc, &markers))
        } else {
            // Re-parse the edited source back into a rendered document.
            from_markdown(&self.source_text())
        };
        self.editor = Editor::new(doc);
        self.md_source = source;
        self.scroll = 0;
        self.follow_caret = true;
        self.dirty = true;
        self.status = Some(
            if source {
                "Markdown source view (edit the raw text)"
            } else {
                "Rendered view"
            }
            .to_string(),
        );
    }

    /// Open the Exit confirmation modal (used by Ctrl+Q and File ▸ Exit).
    fn request_exit(&mut self) {
        self.backstage = None;
        let prompt = if self.modified {
            "Exit docxy? Unsaved changes will be lost."
        } else {
            "Exit docxy?"
        };
        self.confirm = Some(backstage::Confirm::new(
            prompt,
            ConfirmAction::Exit,
            Color::LightBlue,
        ));
        self.dirty = true;
    }

    /// Put text on the OS clipboard and remember it (so a later paste of our own
    /// content can use the styled internal clip instead of plain text).
    fn os_set(&mut self, text: &str) {
        if let Some(cb) = &mut self.os_clip {
            let _ = cb.set_text(text.to_string());
        }
        self.clip_text = Some(text.to_string());
    }

    fn os_get(&mut self) -> Option<String> {
        self.os_clip.as_mut().and_then(|cb| cb.get_text().ok())
    }

    fn do_copy(&mut self) {
        if let Some(c) = self.editor.copy() {
            let text = c.to_text();
            self.clipboard = Some(c);
            self.os_set(&text);
            self.status = Some("Copied".to_string());
        }
    }

    fn do_cut(&mut self) {
        if !self.mutation_allowed(protection::MutationKind::Structure) {
            return;
        }
        if let Some(c) = self.editor.cut() {
            let text = c.to_text();
            self.clipboard = Some(c);
            self.os_set(&text);
            self.after_edit();
            self.status = Some("Cut".to_string());
        }
    }

    fn do_paste(&mut self) {
        let os_text = self.os_get();
        let (clip, keeps_source_formatting) = match os_text {
            // Our own content is still on the clipboard -> paste with full styling.
            Some(t) if Some(&t) == self.clip_text.as_ref() => (self.clipboard.clone(), true),
            // External text -> paste as plain.
            Some(t) => (Some(Clip::from_text(&t)), false),
            // OS clipboard unavailable -> fall back to the internal clip.
            None => (self.clipboard.clone(), true),
        };
        if let Some(c) = clip {
            if !self.paste_allowed(
                protection::MutationKind::Content,
                &c,
                keeps_source_formatting,
            ) {
                return;
            }
            self.editor.paste(&c);
            self.after_edit();
        }
    }

    /// Authorize the semantic insertion plus any formatting carried by a rich
    /// internal clip before a paste path moves the caret or changes the editor.
    fn paste_allowed(
        &mut self,
        mutation: protection::MutationKind,
        clip: &Clip,
        keeps_source_formatting: bool,
    ) -> bool {
        self.mutation_allowed(mutation)
            && (!keeps_source_formatting
                || !clip_has_formatting(clip)
                || self.mutation_allowed(protection::MutationKind::Formatting))
    }

    /// Open the Paste Special dialog, offering the paste formats that make sense
    /// for what is currently on the clipboard.
    fn open_paste_special(&mut self) {
        let os_text = self.os_get();
        let (text, rich) = match os_text {
            // Our own content is still on the board: a richly-formatted clip.
            Some(t) if Some(&t) == self.clip_text.as_ref() => (t, self.clipboard.clone()),
            Some(t) => (t, None),
            None => match &self.clipboard {
                Some(c) => (c.to_text(), Some(c.clone())),
                None => {
                    self.status = Some("Clipboard is empty".to_string());
                    self.dirty = true;
                    return;
                }
            },
        };
        if text.is_empty() && rich.is_none() {
            self.status = Some("Clipboard is empty".to_string());
            self.dirty = true;
            return;
        }
        let mut opts = Vec::new();
        if rich.is_some() {
            opts.push(PasteOpt::KeepSource);
        }
        opts.push(PasteOpt::Merge);
        opts.push(PasteOpt::Unformatted);
        if looks_like_url(&text) {
            opts.push(PasteOpt::Hyperlink);
        }
        let source = if rich.is_some() {
            "Formatted text (docxy selection)"
        } else {
            "Text"
        }
        .to_string();
        self.paste_special = Some(PasteSpecial {
            source,
            text,
            rich,
            opts,
            sel: 0,
        });
        self.dirty = true;
    }

    /// Carry out the highlighted Paste Special option and close the dialog.
    fn apply_paste_special(&mut self) {
        let selection = self.paste_special.as_ref().and_then(|paste| {
            paste
                .opts
                .get(paste.sel)
                .copied()
                .map(|selected| (selected, paste.rich.clone()))
        });
        let Some((selected, rich)) = selection else {
            return;
        };
        let authorization_clip = rich.as_ref().filter(|_| selected == PasteOpt::KeepSource);
        let plain = Clip::default();
        if !self.paste_allowed(
            protection::MutationKind::Content,
            authorization_clip.unwrap_or(&plain),
            authorization_clip.is_some(),
        ) {
            return;
        }
        let Some(ps) = self.paste_special.take() else {
            return;
        };
        let Some(&opt) = ps.opts.get(ps.sel) else {
            return;
        };
        match opt {
            PasteOpt::KeepSource => {
                if let Some(c) = &ps.rich {
                    self.editor.paste(c);
                }
            }
            // insert_str inserts as if typed, so the text adopts the caret's run.
            PasteOpt::Merge => self.editor.insert_str(&ps.text),
            PasteOpt::Unformatted => self.editor.paste(&Clip::from_text(&ps.text)),
            PasteOpt::Hyperlink => {
                let url = ps.text.trim().to_string();
                let link = Inline::Hyperlink(Hyperlink {
                    target: Some(url.clone()),
                    anchor: None,
                    rel_id: None,
                    runs: vec![Run {
                        text: url,
                        props: RunProps::default(),
                    }],
                    ..Hyperlink::default()
                });
                self.editor.paste(&Clip {
                    paras: vec![vec![link]],
                });
            }
        }
        self.after_edit();
        self.status = Some(format!("Pasted ({})", opt.label()));
    }

    fn paste_special_key(&mut self, key: KeyEvent) -> bool {
        let Some(ps) = self.paste_special.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Up | KeyCode::BackTab => {
                ps.sel = ps.sel.saturating_sub(1);
                self.dirty = true;
            }
            KeyCode::Down | KeyCode::Tab => {
                ps.sel = (ps.sel + 1).min(ps.opts.len().saturating_sub(1));
                self.dirty = true;
            }
            KeyCode::Enter => self.apply_paste_special(),
            KeyCode::Esc => {
                self.paste_special = None;
                self.dirty = true;
            }
            _ => {}
        }
        false
    }

    fn paste_special_mouse(&mut self, x: u16, y: u16) {
        let p = Position { x, y };
        // Click an option row to highlight it.
        if let Some(i) = self.ps_rows.iter().position(|r| r.contains(p)) {
            if let Some(ps) = self.paste_special.as_mut() {
                ps.sel = i;
            }
            self.dirty = true;
            return;
        }
        if self.ps_btns[0].contains(p) {
            self.apply_paste_special();
        } else if self.ps_btns[1].contains(p) {
            self.paste_special = None;
            self.dirty = true;
        }
    }

    /// The computed value of `kind`, with a fresh clock for date/time fields.
    fn field_value(&self, kind: FieldKind) -> String {
        let mut ctx = self.field_ctx.clone();
        ctx.now = local_now();
        docxcore::field::eval_field_ctx(kind.instr(), &ctx)
            .unwrap_or_else(|| kind.fallback().to_string())
    }

    /// Build a simple field (`<w:fldSimple>`) inline with its computed value.
    /// Insert a bordered `rows`×`cols` table just after the caret's block, with
    /// the caret landing in the first cell.
    fn insert_table(&mut self, rows: usize, cols: usize) {
        let table = docxcore::table::new_table(
            rows,
            cols,
            docxcore::table::DEFAULT_TEXT_WIDTH,
            docxcore::table::AutoFit::Default,
        );
        let body = &mut self.editor.doc.body;
        let at = self
            .editor
            .caret
            .path
            .first()
            .copied()
            .unwrap_or(0)
            .min(body.len().saturating_sub(1));
        let pos = (at + 1).min(body.len());
        body.insert(pos, Block::Table(table));
        self.editor.clear_selection();
        self.editor.set_caret(Caret::at(vec![pos, 0, 0, 0], 0));
        self.clear_visual_hint();
    }

    fn build_field(&self, kind: FieldKind) -> Inline {
        let text = self.field_value(kind);
        docxcore::field::fld_simple(kind.instr(), &text, &self.editor.caret_props())
    }

    fn apply_insert_field(&mut self) {
        if !self.mutation_allowed(protection::MutationKind::Content) {
            return;
        }
        let Some(d) = self.insert_field.take() else {
            return;
        };
        let Some(&kind) = FieldKind::ALL.get(d.sel) else {
            return;
        };
        let inl = self.build_field(kind);
        self.editor.paste(&Clip {
            paras: vec![vec![inl]],
        });
        self.after_edit();
        self.status = Some(format!("Inserted field: {}", kind.label()));
    }

    fn insert_field_key(&mut self, key: KeyEvent) -> bool {
        let Some(d) = self.insert_field.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Up | KeyCode::BackTab => {
                d.sel = d.sel.saturating_sub(1);
                self.dirty = true;
            }
            KeyCode::Down | KeyCode::Tab => {
                d.sel = (d.sel + 1).min(FieldKind::ALL.len() - 1);
                self.dirty = true;
            }
            KeyCode::Enter => self.apply_insert_field(),
            KeyCode::Esc => {
                self.insert_field = None;
                self.dirty = true;
            }
            _ => {}
        }
        false
    }

    fn insert_field_mouse(&mut self, x: u16, y: u16) {
        let p = Position { x, y };
        if let Some(i) = self.if_rows.iter().position(|r| r.contains(p)) {
            if let Some(d) = self.insert_field.as_mut() {
                d.sel = i;
            }
            self.dirty = true;
            return;
        }
        if self.if_btns[0].contains(p) {
            self.apply_insert_field();
        } else if self.if_btns[1].contains(p) {
            self.insert_field = None;
            self.dirty = true;
        }
    }

    // ---- Compare dialog ----

    /// Who comments and compare revisions are attributed to: the person
    /// reviewing, as Word's user name is, not the document's author property
    /// (#620). That is the OS account name, else [`DEFAULT_AUTHOR`].
    fn review_author(&self) -> String {
        os_user_name().unwrap_or_else(|| DEFAULT_AUTHOR.to_string())
    }

    /// Review ▸ Compare: replace the open document with a new, unsaved
    /// `Compare Result N.docx` (beside the revised file) whose tracked changes
    /// turn `original` into `revised`; neither source file is written. Refuses
    /// while the open document has unsaved changes unless `discard` (the
    /// terminal asks first; the control surface never discards).
    pub(crate) fn compare_paths(
        &mut self,
        original: &str,
        revised: &str,
        discard: bool,
    ) -> Result<CompareSummary, String> {
        // Leave header/footer editing first: committing makes its edits count
        // as unsaved, and its swapped-out body must not outlive the document.
        self.exit_hf_edit(true);
        if self.modified && !discard {
            return Err("unsaved changes; save or reload first".to_string());
        }
        let result = compare_files(original, revised, &self.review_author())?;
        let summary = CompareSummary {
            path: next_compare_result_path(Path::new(revised)),
            insertions: result.insertions,
            deletions: result.deletions,
            skipped: result.skipped,
        };
        self.load_package_state(result.package, summary.path.clone());
        // A new document that exists only in memory until saved.
        self.modified = true;
        self.status = Some(format!(
            "compared: {}",
            compare_summary(summary.insertions, summary.deletions, &summary.skipped)
        ));
        Ok(summary)
    }

    fn run_compare(&mut self, original: &str, revised: &str, discard: bool) {
        if let Err(e) = self.compare_paths(original, revised, discard) {
            self.status = Some(format!("compare failed: {e}"));
        }
        self.dirty = true;
    }

    fn open_compare_dialog(&mut self) {
        // The open document is the natural original when it is a saved .docx.
        let saved_docx = self.format == DocFormat::Docx
            && self.path.to_ascii_lowercase().ends_with(".docx")
            && Path::new(&self.path).is_file();
        let original = if saved_docx {
            self.path.clone()
        } else {
            String::new()
        };
        let field = usize::from(!original.is_empty());
        self.compare_dialog = Some(CompareDialog {
            original,
            revised: String::new(),
            field,
        });
        self.dirty = true;
    }

    fn submit_compare_dialog(&mut self) {
        let Some(d) = self.compare_dialog.as_ref() else {
            return;
        };
        // Paths pasted or dropped from a file manager often arrive quoted.
        let clean = |s: &str| s.trim().trim_matches('"').to_string();
        let (original, revised) = (clean(&d.original), clean(&d.revised));
        self.dirty = true;
        if original.is_empty() || revised.is_empty() {
            self.status = Some("Compare needs an original and a revised .docx".to_string());
            return;
        }
        self.compare_dialog = None;
        self.exit_hf_edit(true);
        if self.modified {
            self.confirm = Some(
                backstage::Confirm::new(
                    "Discard unsaved changes and compare?",
                    ConfirmAction::Compare { original, revised },
                    Color::LightBlue,
                )
                .default_no(),
            );
            return;
        }
        self.run_compare(&original, &revised, false);
    }

    fn compare_dialog_key(&mut self, key: KeyEvent) -> bool {
        let Some(d) = self.compare_dialog.as_mut() else {
            return false;
        };
        self.dirty = true;
        match key.code {
            KeyCode::Up | KeyCode::BackTab => d.field = 0,
            KeyCode::Down | KeyCode::Tab => d.field = 1,
            KeyCode::Backspace => {
                d.focused().pop();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                d.focused().push(c);
            }
            KeyCode::Enter => self.submit_compare_dialog(),
            KeyCode::Esc => self.compare_dialog = None,
            _ => {}
        }
        false
    }

    fn compare_dialog_mouse(&mut self, x: u16, y: u16) {
        let p = Position { x, y };
        self.dirty = true;
        if let Some(i) = self.cd_rows.iter().position(|r| r.contains(p)) {
            if let Some(d) = self.compare_dialog.as_mut() {
                d.field = i;
            }
        } else if self.cd_btns[0].contains(p) {
            self.submit_compare_dialog();
        } else if self.cd_btns[1].contains(p) {
            self.compare_dialog = None;
        }
    }

    // ---- Paragraph dialog (precise indent) ----

    /// Open the Paragraph dialog seeded from the caret paragraph's indents.
    fn open_para_dialog(&mut self) {
        let (left, fl) = self.editor.caret_para_indent();
        let (special, by) = match fl.cmp(&0) {
            std::cmp::Ordering::Greater => (1u8, fl),
            std::cmp::Ordering::Less => (2u8, -fl),
            std::cmp::Ordering::Equal => (0u8, 720), // default 0.5" once a special is picked
        };
        self.para_dialog = Some(ParagraphDialog {
            left,
            special,
            by,
            sel: 0,
        });
        self.dirty = true;
    }

    fn apply_para_dialog(&mut self) {
        if !self.mutation_allowed(protection::MutationKind::Formatting) {
            return;
        }
        let Some(d) = self.para_dialog.take() else {
            return;
        };
        self.editor.set_indent(d.left, d.first_line());
        self.after_edit();
        self.status = Some("Paragraph indent applied".to_string());
    }

    fn para_dialog_key(&mut self, key: KeyEvent) -> bool {
        let Some(d) = self.para_dialog.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Up | KeyCode::BackTab => {
                d.sel = d.sel.saturating_sub(1);
                self.dirty = true;
            }
            KeyCode::Down | KeyCode::Tab => {
                d.sel = (d.sel + 1).min(ParagraphDialog::ROWS - 1);
                self.dirty = true;
            }
            KeyCode::Left => {
                d.adjust(-1);
                self.dirty = true;
            }
            KeyCode::Right => {
                d.adjust(1);
                self.dirty = true;
            }
            KeyCode::Enter => self.apply_para_dialog(),
            KeyCode::Esc => {
                self.para_dialog = None;
                self.dirty = true;
            }
            _ => {}
        }
        false
    }

    fn para_dialog_mouse(&mut self, x: u16, y: u16) {
        let p = Position { x, y };
        if let Some(i) = self.pd_rows.iter().position(|r| r.contains(p)) {
            if let Some(d) = self.para_dialog.as_mut() {
                d.sel = i;
            }
            self.dirty = true;
            return;
        }
        if self.pd_btns[0].contains(p) {
            self.apply_para_dialog();
        } else if self.pd_btns[1].contains(p) {
            self.para_dialog = None;
            self.dirty = true;
        }
    }

    // ---- Apply-Styles dialog ----

    /// Open the Apply-Styles dialog. Lists every paragraph style the document
    /// defines; falls back to the common built-ins if styles.xml is bare.
    fn open_styles_dialog(&mut self) {
        let mut items = self.styles.paragraph_styles();
        if items.is_empty() {
            items = ribbon::STYLE_BUTTONS
                .iter()
                .map(|(label, id)| (id.to_string(), label.to_string()))
                .collect();
        }
        // Start on the caret paragraph's current style, if it's in the list.
        let cur = self.editor.caret_para_style();
        let sel = cur
            .as_deref()
            .and_then(|c| items.iter().position(|(id, _)| id == c))
            .unwrap_or(0);
        self.styles_dialog = Some(StylesDialog { items, sel, top: 0 });
        self.dirty = true;
    }

    fn apply_styles_dialog(&mut self) {
        if !self.mutation_allowed(protection::MutationKind::Formatting) {
            return;
        }
        let Some(d) = self.styles_dialog.take() else {
            return;
        };
        if let Some((id, name)) = d.items.get(d.sel) {
            self.editor.set_para_style(Some(id));
            self.after_edit();
            self.status = Some(format!("Applied style: {name}"));
        }
    }

    fn styles_dialog_key(&mut self, key: KeyEvent) -> bool {
        let Some(d) = self.styles_dialog.as_mut() else {
            return false;
        };
        let n = d.items.len();
        match key.code {
            KeyCode::Up | KeyCode::BackTab => {
                d.sel = d.sel.saturating_sub(1);
                self.dirty = true;
            }
            KeyCode::Down | KeyCode::Tab => {
                d.sel = (d.sel + 1).min(n.saturating_sub(1));
                self.dirty = true;
            }
            KeyCode::Home => {
                d.sel = 0;
                self.dirty = true;
            }
            KeyCode::End => {
                d.sel = n.saturating_sub(1);
                self.dirty = true;
            }
            KeyCode::Enter => self.apply_styles_dialog(),
            KeyCode::Esc => {
                self.styles_dialog = None;
                self.dirty = true;
            }
            _ => {}
        }
        false
    }

    fn styles_dialog_mouse(&mut self, x: u16, y: u16) {
        let p = Position { x, y };
        if let Some(d) = self.styles_dialog.as_mut() {
            // The visible rows map to item indices via the stored top offset.
            if let Some(i) = self.sd_rows.iter().position(|r| r.contains(p)) {
                d.sel = (d.top + i).min(d.items.len().saturating_sub(1));
                self.dirty = true;
                return;
            }
        }
        if self.sd_btns[0].contains(p) {
            self.apply_styles_dialog();
        } else if self.sd_btns[1].contains(p) {
            self.styles_dialog = None;
            self.dirty = true;
        }
    }

    /// Re-read numbering.xml from the package (after a list is created/changed).
    fn reparse_numbering(&mut self) {
        let n = self
            .pkg
            .part("word/numbering.xml")
            .map(|b| parse_numbering_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        self.numbering = Rc::new(n);
    }

    /// Toggle a bullet/numbered list on the selected paragraphs.
    fn apply_list(&mut self, bullet: bool) {
        if !self.mutation_allowed(protection::MutationKind::Formatting) {
            return;
        }
        let num_id = self.pkg.ensure_list(bullet);
        if self.editor.all_in_list(num_id) {
            self.editor.set_list(None);
            self.status = Some("List removed".to_string());
        } else {
            self.editor.set_list(Some(num_id));
            self.status = Some(
                if bullet {
                    "Bulleted list"
                } else {
                    "Numbered list"
                }
                .to_string(),
            );
        }
        self.reparse_numbering();
        self.after_edit();
    }

    /// Toggle a bottom paragraph border on the selected paragraphs.
    fn toggle_para_border(&mut self) {
        if !self.mutation_allowed(protection::MutationKind::Formatting) {
            return;
        }
        use docxcore::model::{BorderKind, ParBorders};
        let has = self.editor.caret_para_props().borders.bottom.is_some();
        let new = if has {
            ParBorders::default()
        } else {
            ParBorders {
                top: None,
                bottom: Some(BorderKind::Single),
            }
        };
        self.editor.set_para_border(new);
        self.after_edit();
        self.status = Some(
            if has {
                "Border removed"
            } else {
                "Bottom border"
            }
            .to_string(),
        );
    }

    fn open_picker(&mut self, kind: PickerKind) {
        // Symbol inserts at the caret and Line Spacing applies to the caret
        // paragraph, so neither needs a selection; the font/colour pickers do.
        let needs_sel = !matches!(
            kind,
            PickerKind::Symbol | PickerKind::LineSpacing | PickerKind::Equation
        );
        if needs_sel && !self.editor.has_selection() {
            self.status = Some(format!("Select text first, then {}", kind.title().trim()));
            self.dirty = true;
            return;
        }
        self.font_picker = Some(FontPicker { kind, sel: 0 });
        self.dirty = true;
    }

    /// Open a Design ▸ Page Background picker. Unlike the font pickers these
    /// act on the whole document, not the selection, and they need a .docx.
    fn open_design_picker(&mut self, kind: PickerKind) {
        if self.format == DocFormat::Markdown {
            self.status = Some(format!(
                "{} needs a .docx (not Markdown)",
                kind.title().trim()
            ));
            self.dirty = true;
            return;
        }
        self.font_picker = Some(FontPicker { kind, sel: 0 });
        self.dirty = true;
    }

    fn apply_picker(&mut self) {
        let Some(kind) = self.font_picker.as_ref().map(|p| p.kind) else {
            return;
        };
        let mutation = if matches!(kind, PickerKind::Symbol | PickerKind::Equation) {
            protection::MutationKind::Content
        } else {
            protection::MutationKind::Formatting
        };
        if !self.mutation_allowed(mutation) {
            return;
        }
        let Some(p) = self.font_picker.take() else {
            return;
        };
        let Some(item) = p.kind.items().get(p.sel).copied() else {
            return;
        };
        match p.kind {
            PickerKind::FontName => self.editor.set_font(item),
            PickerKind::FontSize => {
                if let Ok(pt) = item.parse::<u32>() {
                    self.editor.set_font_size(pt * 2);
                }
            }
            PickerKind::FontColor => self.editor.set_color(color_hex(item)),
            PickerKind::Highlight => self.editor.set_highlight(highlight_name(item)),
            PickerKind::Symbol => self.editor.insert_str(item),
            PickerKind::LineSpacing => {
                if let Some(line) = line_spacing_twips(item) {
                    self.editor.set_line_spacing(line, "auto");
                }
            }
            PickerKind::Equation => {
                if let Some(&(_, latex)) = EQUATIONS.iter().find(|(l, _)| *l == item) {
                    self.editor.insert_equation(latex, false);
                }
            }
            PickerKind::PageColor | PickerKind::Watermark | PickerKind::PageBorders => {
                // The design picks set their own status; the generic one below
                // must not overwrite it.
                return self.apply_design_pick(p.kind, item);
            }
        }
        self.after_edit();
        self.status = Some(format!("{}: {item}", p.kind.title().trim()));
    }

    /// Apply a Design ▸ Page Background pick. Page Color is a package edit
    /// with no undo (as Hyphenation); a watermark's header parts are package
    /// edits and its new header references one undo step; Page Borders
    /// rewrite every section as one undo step. Each edit commits an open
    /// header/footer edit first so its editor cannot write a header part
    /// back over a watermark and the section edits hit the body editor.
    fn apply_design_pick(&mut self, kind: PickerKind, item: &str) {
        match kind {
            PickerKind::PageColor => {
                let rgb = PAGE_COLORS.iter().find(|c| c.0 == item).map(|c| c.1);
                self.set_page_color(rgb);
                self.status = Some(match rgb {
                    Some(_) => format!("Page color: {item}"),
                    None => "Page color: No Color".to_string(),
                });
            }
            PickerKind::Watermark => {
                let spec = WATERMARK_PRESETS
                    .iter()
                    .find(|w| w.0 == item)
                    .map(|w| TextWatermarkSpec::preset(w.1, w.2));
                let result = self.set_text_watermark(spec.as_ref());
                self.status = Some(if result.is_err() {
                    "Could not add the watermark: the document cannot take a header".to_string()
                } else {
                    match spec {
                        Some(_) => format!("Watermark: {item}"),
                        None => "Watermark removed".to_string(),
                    }
                });
            }
            PickerKind::PageBorders => {
                let pb = (item != "None").then(|| box_page_borders(item == "Shadow", None));
                self.set_page_borders(pb.as_ref());
                // after_edit clears the status: report the pick after it.
                self.status = Some(format!("Page borders: {item}"));
            }
            _ => unreachable!("not a Design picker: {kind:?}"),
        }
        self.dirty = true;
    }

    /// Set or remove the page colour (Design ▸ Page Color): the package's
    /// `w:background` and `w:displayBackgroundShape`. A package edit with no
    /// undo. Returns whether anything changed.
    pub(crate) fn set_page_color(&mut self, rgb: Option<u32>) -> bool {
        if self.hf_edit.is_some() {
            self.exit_hf_edit(true);
        }
        let bg = rgb.map(|color| PageBackground {
            color,
            gradient: None,
        });
        let changed = self.pkg.set_page_background(bg.as_ref());
        if changed {
            self.modified = true;
        }
        changed
    }

    /// Write or remove the text watermark in every shown header (Design ▸
    /// Watermark). The header parts are package edits and the new header
    /// references one undo step. Returns whether anything changed; errs when
    /// a watermark was requested but the document cannot take a header. An
    /// Err may follow a partial edit (the removed watermark's header parts
    /// and section references are already gone) — the Err carries `changed`
    /// so callers can report whether one happened.
    pub(crate) fn set_text_watermark(
        &mut self,
        spec: Option<&TextWatermarkSpec>,
    ) -> Result<bool, (bool, String)> {
        if self.hf_edit.is_some() {
            self.exit_hf_edit(true);
        }
        let mut sects = self.editor.sections();
        let before = sects.clone();
        let parts = self.pkg.set_text_watermark(spec, &mut sects);
        let raws: Vec<(usize, String)> = sects
            .into_iter()
            .enumerate()
            .filter(|(k, raw)| *raw != before[*k])
            .collect();
        let refs = self.editor.replace_sections(&raws);
        let changed = parts || refs;
        self.page_parts_sect.clear();
        self.sync_page_parts();
        self.refresh_watermark_state();
        if changed {
            self.modified = true;
        }
        if spec.is_some()
            && self
                .pkg
                .shown_text_watermarks(&self.editor.sections())
                .is_empty()
        {
            return Err((
                changed,
                "could not add the watermark: the document cannot take a header".into(),
            ));
        }
        Ok(changed)
    }

    /// Write or remove page borders on every section (Design ▸ Page Borders)
    /// as one undo step. Returns whether anything changed.
    pub(crate) fn set_page_borders(&mut self, pb: Option<&PageBorders>) -> bool {
        if self.hf_edit.is_some() {
            self.exit_hf_edit(true);
        }
        let n = self.editor.sections().len();
        let changed = self
            .editor
            .edit_sections(&(0..n).collect::<Vec<_>>(), |raw| {
                PageBorders::apply(pb, raw)
            });
        if changed {
            self.after_edit();
        }
        changed
    }

    fn picker_key(&mut self, key: KeyEvent) -> bool {
        let Some(p) = self.font_picker.as_mut() else {
            return false;
        };
        let n = p.kind.items().len();
        match key.code {
            KeyCode::Up | KeyCode::BackTab => {
                p.sel = p.sel.saturating_sub(1);
                self.dirty = true;
            }
            KeyCode::Down | KeyCode::Tab => {
                p.sel = (p.sel + 1).min(n.saturating_sub(1));
                self.dirty = true;
            }
            KeyCode::Enter => self.apply_picker(),
            KeyCode::Esc => {
                self.font_picker = None;
                self.dirty = true;
            }
            _ => {}
        }
        false
    }

    fn picker_mouse(&mut self, x: u16, y: u16) {
        let pos = Position { x, y };
        if let Some(i) = self.fp_rows.iter().position(|r| r.contains(pos)) {
            if let Some(p) = self.font_picker.as_mut() {
                p.sel = i;
            }
            self.dirty = true;
            return;
        }
        if self.fp_btns[0].contains(pos) {
            self.apply_picker();
        } else if self.fp_btns[1].contains(pos) {
            self.font_picker = None;
            self.dirty = true;
        }
    }

    /// The hyperlink target at a given document line and column, if any.
    fn link_at(&self, doc_line: usize, col: usize) -> Option<String> {
        let line = self.lines.get(doc_line)?;
        let mut cum = 0usize;
        for span in &line.spans {
            let w = span.width();
            if col < cum + w {
                return span.link.clone();
            }
            cum += w;
        }
        None
    }

    /// Jump to the bookmark named `anchor` (the target of an internal link).
    /// Scrolls so the paragraph holding its `<w:bookmarkStart w:name=…>` is at
    /// the top of the view.
    fn jump_to_anchor(&mut self, anchor: &str) {
        let needle = format!("w:name=\"{anchor}\"");
        let bi = self
            .editor
            .doc
            .body
            .iter()
            .position(|b| block_has_bookmark(b, &needle));
        if let Some(bi) = bi {
            if let Some(line) = self
                .maps
                .iter()
                .position(|m| m.segs.iter().any(|s| s.path.first() == Some(&bi)))
            {
                self.scroll = line.min(self.lines.len().saturating_sub(1));
                self.follow_caret = false;
                self.dirty = true;
                self.status = Some(format!("Jumped to “{anchor}”."));
                return;
            }
        }
        self.status = Some(format!("Bookmark “{anchor}” not found."));
        self.dirty = true;
    }

    /// If `col` is in the scrollbar gutter (just past the rendered content) and
    /// the document overflows, jump the scroll to the indicated position and
    /// return true. Used so clicking/dragging the bar scrolls instead of selecting.
    fn scrollbar_jump(&mut self, row: usize, col: usize) -> bool {
        if col < self.rendered_width as usize || self.lines.len() <= self.viewport_h {
            return false;
        }
        let max = self.lines.len().saturating_sub(self.viewport_h);
        let span = self.viewport_h.saturating_sub(1).max(1);
        self.scroll = (row * max / span).min(max);
        self.follow_caret = false;
        self.drag_from = None; // this is a scrollbar drag, not a text selection
        self.clear_visual_hint();
        true
    }

    /// The caret at a screen position, if it lands on editable text.
    fn click_caret(&self, row: usize, col: usize) -> Option<LineCaret> {
        let doc_line = self.scroll + row;
        self.maps.get(doc_line)?.nearest_caret(col)
    }

    fn ribbon_click(&mut self, x: u16, y: u16) {
        match self.ribbon.hit(x, y, self.ribbon_open) {
            ribbon::Hit::Tab(i) => {
                if self.ribbon.tab_label(i) == Some("File") {
                    self.open_backstage();
                } else {
                    self.ribbon.set_active(i);
                    self.ribbon_open = true;
                    self.ribbon_focus = ribbon::Focus::Tab(i);
                    self.dirty = true;
                }
            }
            ribbon::Hit::Button(act) => {
                self.run_act(act);
            }
            ribbon::Hit::Outside => {}
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        // The welcome screen owns the whole terminal; handle its clicks here so
        // nothing leaks to the hidden document behind it. Hovering highlights an
        // item, clicking activates it.
        if self.start_screen {
            let ev = self.start.mouse(m.column, m.row);
            if !matches!(ev, backstage::StartEvent::None) {
                self.dirty = true;
            }
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                if let backstage::StartEvent::Choose(i) = ev {
                    // start_choose may quit; the quit flag is read elsewhere.
                    if self.start_choose(i) {
                        self.quit_requested = true;
                    }
                }
            }
            return;
        }
        if self.confirm.is_some() {
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                // quit (if any) propagates via quit_requested in handle_event
                self.confirm_mouse(m.column, m.row);
            }
            return;
        }
        if self.paste_special.is_some() {
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                self.paste_special_mouse(m.column, m.row);
            }
            return;
        }
        if self.insert_field.is_some() {
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                self.insert_field_mouse(m.column, m.row);
            }
            return;
        }
        if self.para_dialog.is_some() {
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                self.para_dialog_mouse(m.column, m.row);
            }
            return;
        }
        if self.compare_dialog.is_some() {
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                self.compare_dialog_mouse(m.column, m.row);
            }
            return;
        }
        if self.styles_dialog.is_some() {
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                self.styles_dialog_mouse(m.column, m.row);
            }
            return;
        }
        if self.font_picker.is_some() {
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                self.picker_mouse(m.column, m.row);
            }
            return;
        }
        if self.backstage.is_some() {
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) => self.bs_mouse(m.column, m.row),
                MouseEventKind::ScrollDown => {
                    if let Some(b) = self.backstage.as_mut() {
                        b.scroll_preview(3);
                    }
                    self.dirty = true;
                }
                MouseEventKind::ScrollUp => {
                    if let Some(b) = self.backstage.as_mut() {
                        b.scroll_preview(-3);
                    }
                    self.dirty = true;
                }
                _ => {}
            }
            return; // backstage handles its own mouse
        }
        // Navigation pane: click a heading to jump to it.
        if self.show_nav
            && m.kind == MouseEventKind::Down(MouseButton::Left)
            && self.nav_rect.contains(Position {
                x: m.column,
                y: m.row,
            })
        {
            let row = m.row.saturating_sub(self.nav_rect.y + 1) as usize; // inside the box border
            if let Some((_, line)) = self.nav_items.get(row) {
                self.scroll = (*line).min(self.lines.len().saturating_sub(1));
                self.follow_caret = false;
                self.dirty = true;
            }
            return;
        }
        let mrow = m.row as usize;
        let col =
            (m.column as usize).saturating_sub(self.doc_x0 as usize) + self.doc_hscroll as usize;
        // A left-click in the ribbon area drives the ribbon. The press, any
        // micro-drag it carries, and its release must ALL be consumed here —
        // otherwise a Drag/Up over the ribbon falls through to the document and
        // drags the text selection off wherever it was (e.g. clicking Bold while
        // a word is selected moves the selection instead of bolding it). Wheel
        // events still fall through so scrolling works anywhere over the ribbon.
        if mrow < self.ribbon_h {
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    self.ribbon_click(m.column, m.row);
                    return;
                }
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => {
                    return;
                }
                _ => {}
            }
        }
        // Ignore left-clicks on the ruler row (between the ribbon and the document).
        if mrow < self.doc_y0 as usize {
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                return;
            }
        }
        let row = mrow.saturating_sub(self.doc_y0 as usize); // row within the document viewport
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Returning to the document hands keyboard control back to the
                // editor. With auto-hide on, it also collapses the on-demand
                // ribbon; otherwise the ribbon stays pinned open.
                if self.ribbon_open {
                    self.ribbon_focus = ribbon::Focus::None;
                    if self.auto_hide_ribbon {
                        self.ribbon_open = false;
                    }
                    self.dirty = true;
                }
                if row >= self.viewport_h {
                    return; // status bar
                }
                if self.scrollbar_jump(row, col) {
                    return; // dragging the scrollbar, not selecting text
                }
                // Clicking places the caret at a visible spot, so never scroll to
                // it — that would yank the view back to the old caret's page when
                // the click lands on a non-editable cell (margin/border/gap).
                self.follow_caret = false;
                let doc_line = self.scroll + row;
                // Position the caret at the click and remember it as the anchor
                // for a possible drag-select.
                if let Some(c) = self.click_caret(row, col) {
                    self.set_visual_caret(doc_line, c);
                    self.editor.clear_selection();
                    self.drag_from = Some(self.editor.caret.clone());
                    self.dirty = true;
                }
                // A clicked link: an internal `#anchor` jumps to its bookmark; an
                // external link is never opened directly (confirm + http/https only).
                if let Some(url) = self.link_at(doc_line, col) {
                    if let Some(anchor) = url.strip_prefix('#') {
                        self.jump_to_anchor(anchor);
                    } else if !url.is_empty() {
                        if safe_url(&url) {
                            self.pending_link = Some(url);
                        } else {
                            self.status = Some(format!("blocked non-web link: {url}"));
                        }
                        self.dirty = true;
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.drag_from.is_none() && self.scrollbar_jump(row, col) {
                    return; // continuing a scrollbar drag
                }
                // Extend a selection from the press point to the dragged-to cell.
                self.follow_caret = false;
                let vh = self.viewport_h.max(1);
                let clamped = row.min(vh - 1);
                if let Some(c) = self.click_caret(clamped, col) {
                    if self.editor.anchor.is_none() {
                        self.editor.anchor = self.drag_from.clone();
                    }
                    self.set_visual_caret(self.scroll + clamped, c);
                    self.dirty = true;
                }
                // Auto-scroll when dragging at the top/bottom edge.
                if row == 0 {
                    self.scroll = self.scroll.saturating_sub(1);
                } else if row >= vh - 1 {
                    let max = self.lines.len().saturating_sub(vh);
                    self.scroll = (self.scroll + 1).min(max);
                }
                self.dirty = true;
            }
            // Shift+wheel scrolls the canvas horizontally (to reach comments
            // that sit beside the page in print layout). Horizontal wheels too.
            MouseEventKind::ScrollRight => {
                self.comments_hscroll += 4;
                self.dirty = true;
            }
            MouseEventKind::ScrollLeft => {
                self.comments_hscroll = self.comments_hscroll.saturating_sub(4);
                self.dirty = true;
            }
            MouseEventKind::ScrollDown
                if m.modifiers.contains(KeyModifiers::SHIFT) && self.show_comments =>
            {
                self.comments_hscroll += 4;
                self.dirty = true;
            }
            MouseEventKind::ScrollUp
                if m.modifiers.contains(KeyModifiers::SHIFT) && self.show_comments =>
            {
                self.comments_hscroll = self.comments_hscroll.saturating_sub(4);
                self.dirty = true;
            }
            MouseEventKind::ScrollDown => {
                // The wheel over the comments panel scrolls the comments.
                if self.comments_rect.contains(Position {
                    x: m.column,
                    y: m.row,
                }) {
                    // a loose cap (draw clamps to the exact content height)
                    let cap = self.comments.len() * 12;
                    self.comments_scroll = (self.comments_scroll + 3).min(cap);
                    self.dirty = true;
                    return;
                }
                if self.notes_rect.contains(Position {
                    x: m.column,
                    y: m.row,
                }) {
                    let cap = self.notes.len() * 12;
                    self.notes_scroll = (self.notes_scroll + 3).min(cap);
                    self.dirty = true;
                    return;
                }
                // Scrolling changes only the visible slice, not the document, so
                // don't mark dirty (that would re-render the whole doc per tick).
                self.follow_caret = false;
                let max = self.lines.len().saturating_sub(self.viewport_h);
                self.scroll = (self.scroll + 3).min(max);
            }
            MouseEventKind::ScrollUp => {
                if self.comments_rect.contains(Position {
                    x: m.column,
                    y: m.row,
                }) {
                    self.comments_scroll = self.comments_scroll.saturating_sub(3);
                    self.dirty = true;
                    return;
                }
                if self.notes_rect.contains(Position {
                    x: m.column,
                    y: m.row,
                }) {
                    self.notes_scroll = self.notes_scroll.saturating_sub(3);
                    self.dirty = true;
                    return;
                }
                self.follow_caret = false;
                self.scroll = self.scroll.saturating_sub(3);
            }
            _ => {}
        }
    }

    fn enter_find(&mut self) {
        self.find = Some(FindState {
            query: String::new(),
            replacement: None,
            editing_replacement: false,
            matches: Vec::new(),
            idx: 0,
        });
        self.status = None;
        self.dirty = true;
    }

    /// Re-run the find bar's search. It searches what the document shows,
    /// so a match can be read-only (in a tracked change, a field's result, …):
    /// see [`docxcore::editor::FoundMatch`].
    fn find_recompute(&mut self) {
        let Some((query, idx0)) = self.find.as_ref().map(|f| (f.query.clone(), f.idx)) else {
            return;
        };
        let matches = self.editor.find_visible(&query, false);
        let idx = if idx0 < matches.len() { idx0 } else { 0 };
        if let Some(m) = matches.get(idx) {
            let m = m.clone();
            self.editor.select_found(&m);
            self.clear_visual_hint();
        } else {
            self.editor.clear_selection();
        }
        if let Some(f) = &mut self.find {
            f.matches = matches;
            f.idx = idx;
        }
        self.dirty = true;
    }

    fn find_step(&mut self, delta: i64) {
        let (len, idx) = match &self.find {
            Some(f) if !f.matches.is_empty() => (f.matches.len(), f.idx),
            _ => return,
        };
        let nidx = (idx as i64 + delta).rem_euclid(len as i64) as usize;
        let m = self.find.as_ref().unwrap().matches[nidx].clone();
        self.editor.select_found(&m);
        if let Some(f) = &mut self.find {
            f.idx = nidx;
        }
        self.dirty = true;
    }

    /// Handle a key while the find/replace bar is open. Never quits.
    fn on_find_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                if let Some(q) = self.find.as_ref().map(|f| f.query.clone()) {
                    if let Some(v) = &mut self.vim {
                        v.last_search = q;
                    }
                }
                self.find = None;
                self.editor.clear_selection();
                self.dirty = true;
            }
            KeyCode::Tab => {
                if let Some(f) = &mut self.find {
                    match f.replacement {
                        None => {
                            f.replacement = Some(String::new());
                            f.editing_replacement = true;
                        }
                        Some(_) => f.editing_replacement = !f.editing_replacement,
                    }
                }
                self.dirty = true;
            }
            KeyCode::Char('f') if ctrl => self.find_step(1),
            KeyCode::Char('a') if ctrl => {
                if let Some((q, Some(repl))) = self
                    .find
                    .as_ref()
                    .map(|f| (f.query.clone(), f.replacement.clone()))
                {
                    if !self.mutation_allowed(protection::MutationKind::Content) {
                        return false;
                    }
                    let (n, read_only) = self.editor.replace_all_visible(&q, &repl, false);
                    self.modified |= n > 0;
                    self.status = Some(if read_only > 0 {
                        format!("Replaced {n}; {read_only} read-only match(es) skipped")
                    } else {
                        format!("Replaced {n}")
                    });
                    self.find_recompute();
                }
            }
            KeyCode::Enter => {
                let is_replace = self
                    .find
                    .as_ref()
                    .map(|f| f.replacement.is_some())
                    .unwrap_or(false);
                if is_replace {
                    if !self.mutation_allowed(protection::MutationKind::Content) {
                        return false;
                    }
                    let (repl, current) = match &self.find {
                        Some(f) => (
                            f.replacement.clone().unwrap_or_default(),
                            f.matches.get(f.idx).cloned(),
                        ),
                        None => (String::new(), None),
                    };
                    // Only an editable match that is still selected is
                    // replaced; a read-only one is skipped. An editable match
                    // the selection has moved off (a click while the bar is
                    // open) is selected again first, not replaced unseen.
                    match current {
                        Some(m) if m.editable && self.editor.selection_is(&m) => {
                            self.editor.replace_current_with(&repl);
                            self.modified = true;
                            self.find_recompute();
                        }
                        Some(m) if !m.editable => {
                            self.status = Some("Read-only match skipped".to_string());
                            self.find_step(1);
                        }
                        Some(m) => {
                            self.editor.select_found(&m);
                            self.dirty = true;
                        }
                        None => {}
                    }
                } else {
                    self.find_step(1);
                }
            }
            KeyCode::Down => self.find_step(1),
            KeyCode::Up => self.find_step(-1),
            KeyCode::Backspace => {
                let mut query_changed = false;
                if let Some(f) = &mut self.find {
                    match &mut f.replacement {
                        Some(repl) if f.editing_replacement => {
                            repl.pop();
                        }
                        _ => {
                            f.query.pop();
                            query_changed = true;
                        }
                    }
                }
                if query_changed {
                    self.find_recompute();
                } else {
                    self.dirty = true;
                }
            }
            KeyCode::Char(c) if !ctrl => {
                let mut query_changed = false;
                if let Some(f) = &mut self.find {
                    match &mut f.replacement {
                        Some(repl) if f.editing_replacement => repl.push(c),
                        _ => {
                            f.query.push(c);
                            query_changed = true;
                        }
                    }
                }
                if query_changed {
                    self.find_recompute();
                } else {
                    self.dirty = true;
                }
            }
            _ => {}
        }
        false
    }

    // ---- vim mode ----

    fn vim_mode(&self) -> Option<VimMode> {
        self.vim.as_ref().map(|v| v.mode)
    }

    fn vim_set_mode(&mut self, m: VimMode) {
        if let Some(v) = &mut self.vim {
            v.mode = m;
            v.reset_pending();
        }
        self.dirty = true;
    }

    fn vim_enter_insert(&mut self) {
        self.vim_set_mode(VimMode::Insert);
    }

    fn vim_to_normal(&mut self) {
        self.editor.clear_selection();
        self.vim_set_mode(VimMode::Normal);
    }

    fn set_clip(&mut self, clip: Option<Clip>, linewise: bool) {
        if let Some(c) = clip {
            let text = c.to_text();
            self.clipboard = Some(c);
            self.os_set(&text);
            if let Some(v) = &mut self.vim {
                v.linewise_clip = linewise;
            }
        }
    }

    fn vim_do_motion(&mut self, motion: char, count: usize) {
        let n = if matches!(motion, '0' | '$' | '^' | 'G') {
            1
        } else {
            count
        };
        for _ in 0..n {
            match motion {
                'h' => self.move_visual_horiz(false),
                'l' => self.move_visual_horiz(true),
                'j' => self.move_vert(true),
                'k' => self.move_vert(false),
                'w' => {
                    self.clear_visual_hint();
                    self.editor.move_word_right();
                }
                'b' => {
                    self.clear_visual_hint();
                    self.editor.move_word_left();
                }
                'e' => {
                    self.clear_visual_hint();
                    self.editor.move_word_end();
                }
                '0' | '^' => self.move_visual_line_edge(false),
                '$' => self.move_visual_line_edge(true),
                'G' => {
                    self.clear_visual_hint();
                    self.editor.move_doc_end();
                }
                _ => {}
            }
        }
        self.dirty = true;
    }

    fn vim_apply_op(&mut self, op: char, linewise: bool) {
        if matches!(op, 'd' | 'c') && !self.mutation_allowed(protection::MutationKind::Structure) {
            return;
        }
        // Charwise visual selection is inclusive of the char under the cursor.
        if !linewise && self.vim_mode() == Some(VimMode::Visual) {
            if let Some((lo, hi)) = self.editor.selection_range() {
                self.editor.anchor = Some(lo);
                self.editor.set_caret(hi);
                self.move_visual_horiz(true);
            }
        }
        match op {
            'd' => {
                let c = self.editor.cut();
                self.set_clip(c, linewise);
                self.after_edit();
                self.vim_set_mode(VimMode::Normal);
            }
            'y' => {
                let c = self.editor.copy();
                if let Some((lo, _)) = self.editor.selection_range() {
                    self.editor.set_caret(lo);
                    self.clear_visual_hint();
                }
                self.editor.clear_selection();
                self.set_clip(c, linewise);
                self.vim_set_mode(VimMode::Normal);
            }
            'c' => {
                let c = self.editor.cut();
                self.set_clip(c, linewise);
                self.after_edit();
                self.vim_enter_insert();
            }
            _ => {}
        }
    }

    fn vim_operator_motion(&mut self, op: char, motion: char, count: usize) {
        let start = self.editor.caret.clone();
        self.editor.clear_selection();
        self.vim_do_motion(motion, count);
        self.editor.anchor = Some(start);
        self.vim_apply_op(op, false);
    }

    fn vim_handle_motion(&mut self, motion: char) {
        let op = self.vim.as_ref().and_then(|v| v.pending_op);
        if matches!(op, Some('d' | 'c'))
            && !self.mutation_allowed(protection::MutationKind::Structure)
        {
            return;
        }
        let count = self.vim.as_mut().map(|v| v.take_count()).unwrap_or(1);
        if let Some(op) = op {
            self.vim_operator_motion(op, motion, count);
            if let Some(v) = &mut self.vim {
                v.pending_op = None;
            }
        } else {
            self.vim_do_motion(motion, count);
        }
    }

    fn vim_paste(&mut self, before: bool) {
        let Some(c) = self.clipboard.clone() else {
            return;
        };
        let linewise = self.vim.as_ref().map(|v| v.linewise_clip).unwrap_or(false);
        let mutation = if linewise {
            protection::MutationKind::Structure
        } else {
            protection::MutationKind::Content
        };
        if !self.paste_allowed(mutation, &c, true) {
            return;
        }
        if linewise {
            if before {
                self.editor.move_home();
                self.editor.paste(&c);
                self.editor.insert_newline();
            } else {
                self.editor.move_end();
                self.editor.insert_newline();
                self.editor.paste(&c);
            }
        } else {
            if !before {
                self.editor.move_right();
            }
            self.editor.paste(&c);
        }
        self.after_edit();
    }

    fn vim_search_next(&mut self, reverse: bool) {
        let q = self
            .vim
            .as_ref()
            .map(|v| v.last_search.clone())
            .unwrap_or_default();
        if q.is_empty() {
            return;
        }
        if let Some(m) = self.editor.find_next(&q, false, reverse) {
            self.editor.select_match(&m);
            self.clear_visual_hint();
            self.dirty = true;
        }
    }

    fn vim_char(&mut self, c: char, ctrl: bool) {
        if ctrl && c == 'r' {
            if !self.mutation_allowed(protection::MutationKind::Content) {
                return;
            }
            let n = self.vim.as_mut().map(|v| v.take_count()).unwrap_or(1);
            let mut changed = false;
            for _ in 0..n {
                changed |= self.editor.redo();
            }
            if changed {
                self.after_edit();
            } else {
                self.dirty = true;
            }
            return;
        }
        let mode = self.vim.as_ref().unwrap().mode;

        // count prefix
        let count_empty = self.vim.as_ref().unwrap().count.is_empty();
        if c.is_ascii_digit() && !(c == '0' && count_empty) {
            if let Some(v) = &mut self.vim {
                v.count.push(c);
            }
            return;
        }
        // g / gg
        if c == 'g' {
            let pg = self.vim.as_ref().unwrap().pending_g;
            if pg {
                self.clear_visual_hint();
                self.editor.move_doc_start();
                if let Some(v) = &mut self.vim {
                    v.pending_g = false;
                    v.count.clear();
                }
                self.dirty = true;
            } else if let Some(v) = &mut self.vim {
                v.pending_g = true;
            }
            return;
        }
        if let Some(v) = &mut self.vim {
            v.pending_g = false;
        }

        // operators
        if matches!(c, 'd' | 'c' | 'y') {
            if mode == VimMode::Visual || mode == VimMode::VisualLine {
                self.vim_apply_op(c, mode == VimMode::VisualLine);
                return;
            }
            let same = self.vim.as_ref().unwrap().pending_op == Some(c);
            if same {
                if matches!(c, 'd' | 'c')
                    && !self.mutation_allowed(protection::MutationKind::Structure)
                {
                    return;
                }
                let count = self.vim.as_mut().unwrap().take_count();
                self.editor.select_lines(count);
                self.vim_apply_op(c, true);
                if let Some(v) = &mut self.vim {
                    v.pending_op = None;
                }
            } else if let Some(v) = &mut self.vim {
                v.pending_op = Some(c);
            }
            return;
        }

        // motions (also operator targets)
        if matches!(
            c,
            'h' | 'l' | 'j' | 'k' | 'w' | 'b' | 'e' | '0' | '$' | '^' | 'G'
        ) {
            self.vim_handle_motion(c);
            return;
        }

        // standalone commands
        match c {
            'i' => self.vim_enter_insert(),
            'a' => {
                self.move_visual_horiz(true);
                self.vim_enter_insert();
            }
            'A' => {
                self.move_visual_line_edge(true);
                self.vim_enter_insert();
            }
            'I' => {
                self.move_visual_line_edge(false);
                self.vim_enter_insert();
            }
            'o' => {
                if !self.mutation_allowed(protection::MutationKind::Structure) {
                    return;
                }
                self.clear_visual_hint();
                self.editor.move_end();
                self.editor.insert_newline();
                self.after_edit();
                self.vim_enter_insert();
            }
            'O' => {
                if !self.mutation_allowed(protection::MutationKind::Structure) {
                    return;
                }
                self.clear_visual_hint();
                self.editor.move_home();
                self.editor.insert_newline();
                self.move_vert(false);
                self.after_edit();
                self.vim_enter_insert();
            }
            'x' => {
                if !self.mutation_allowed(protection::MutationKind::Structure) {
                    return;
                }
                let n = self.vim.as_mut().unwrap().take_count();
                for _ in 0..n {
                    self.editor.delete_forward();
                }
                self.after_edit();
            }
            'D' => {
                if !self.mutation_allowed(protection::MutationKind::Structure) {
                    return;
                }
                let s = self.editor.caret.clone();
                self.move_visual_line_edge(true);
                self.editor.anchor = Some(s);
                let c = self.editor.cut();
                self.set_clip(c, false);
                self.after_edit();
            }
            'p' => self.vim_paste(false),
            'P' => self.vim_paste(true),
            'u' => {
                if !self.mutation_allowed(protection::MutationKind::Content) {
                    return;
                }
                let n = self.vim.as_mut().unwrap().take_count();
                let mut changed = false;
                for _ in 0..n {
                    changed |= self.editor.undo();
                }
                if changed {
                    self.after_edit();
                } else {
                    self.dirty = true;
                }
            }
            'v' => {
                let cur = self.editor.caret.clone();
                self.editor.anchor = Some(cur);
                self.vim_set_mode(VimMode::Visual);
            }
            'V' => {
                self.editor.select_lines(1);
                self.vim_set_mode(VimMode::VisualLine);
            }
            '/' => self.enter_find(),
            'n' => self.vim_search_next(false),
            'N' => self.vim_search_next(true),
            ':' => {
                if let Some(v) = &mut self.vim {
                    v.cmdline = Some(String::new());
                }
                self.dirty = true;
            }
            _ => {
                if let Some(v) = &mut self.vim {
                    v.reset_pending();
                }
            }
        }
    }

    fn on_vim_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.vim_to_normal(),
            KeyCode::Char(c) => self.vim_char(c, ctrl),
            KeyCode::Left => self.vim_handle_motion('h'),
            KeyCode::Right => self.vim_handle_motion('l'),
            KeyCode::Up => self.vim_handle_motion('k'),
            KeyCode::Down => self.vim_handle_motion('j'),
            KeyCode::Home => self.vim_handle_motion('0'),
            KeyCode::End => self.vim_handle_motion('$'),
            _ => {}
        }
        false
    }

    fn on_vim_cmdline(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                if let Some(v) = &mut self.vim {
                    v.cmdline = None;
                }
                self.dirty = true;
            }
            KeyCode::Enter => {
                let cmd = self
                    .vim
                    .as_mut()
                    .and_then(|v| v.cmdline.take())
                    .unwrap_or_default();
                self.dirty = true;
                return self.vim_run_command(&cmd);
            }
            KeyCode::Backspace => {
                if let Some(s) = self.vim.as_mut().and_then(|v| v.cmdline.as_mut()) {
                    s.pop();
                }
                self.dirty = true;
            }
            KeyCode::Char(c) => {
                if let Some(s) = self.vim.as_mut().and_then(|v| v.cmdline.as_mut()) {
                    s.push(c);
                }
                self.dirty = true;
            }
            _ => {}
        }
        false
    }

    fn vim_run_command(&mut self, cmd: &str) -> bool {
        match cmd.trim() {
            "w" => {
                self.save();
                false
            }
            "wq" | "x" => {
                self.save();
                true
            }
            "q" => {
                if self.modified {
                    self.status = Some("unsaved changes (:q! to discard)".to_string());
                    false
                } else {
                    true
                }
            }
            "q!" => true,
            other => {
                self.status = Some(format!("not a command: :{other}"));
                false
            }
        }
    }

    /// Returns true if the app should quit.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        // Keyboard actions should keep the caret on screen; wheel/drag don't.
        self.follow_caret = true;
        // The welcome screen (no file given) owns all keys until dismissed.
        if self.start_screen {
            return self.start_screen_key(key);
        }
        // A modal confirmation owns all keys while open.
        if self.confirm.is_some() {
            return self.confirm_key(key);
        }
        // The Paste Special dialog is modal too.
        if self.paste_special.is_some() {
            return self.paste_special_key(key);
        }
        if self.insert_field.is_some() {
            return self.insert_field_key(key);
        }
        if self.para_dialog.is_some() {
            return self.para_dialog_key(key);
        }
        if self.compare_dialog.is_some() {
            return self.compare_dialog_key(key);
        }
        if self.styles_dialog.is_some() {
            return self.styles_dialog_key(key);
        }
        if self.font_picker.is_some() {
            return self.picker_key(key);
        }
        if self.comment_input.is_some() {
            return self.comment_input_key(key);
        }
        // The File backstage is modal: it owns all keys while open.
        if self.backstage.is_some() {
            return self.backstage_key(key);
        }
        // A link-open confirmation is modal: only an explicit `y` proceeds.
        if let Some(url) = self.pending_link.take() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    open_url(&url);
                    self.status = Some(format!("opened {url}"));
                }
                _ => self.status = Some("link cancelled".to_string()),
            }
            self.dirty = true;
            return false;
        }
        // Header/footer focus-edit: Esc / F6 / F7 returns to the body (committing).
        if self.hf_edit.is_some()
            && matches!(key.code, KeyCode::Esc | KeyCode::F(6) | KeyCode::F(7))
        {
            self.exit_hf_edit(true);
            return false;
        }
        if self.find.is_some() {
            return self.on_find_key(key);
        }
        if self.vim.is_some() {
            let in_cmdline = self.vim.as_ref().unwrap().cmdline.is_some();
            if in_cmdline {
                return self.on_vim_cmdline(key);
            }
            if self.vim_mode() != Some(VimMode::Insert) {
                return self.on_vim_key(key);
            }
            // Insert mode: Esc -> Normal; everything else is normal editing.
            if key.code == KeyCode::Esc {
                self.vim_to_normal();
                return false;
            }
        }
        // While the ribbon has keyboard focus, navigation keys drive it; other
        // keys (Ctrl shortcuts, typing) fall through to normal handling.
        if self.ribbon_focus != ribbon::Focus::None {
            if let Some(quit) = self.ribbon_key(key) {
                return quit;
            }
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // Tab/Shift+Tab in a table move between cells (Word); only Tab in the
        // last cell edits, by adding a row.
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
            && !ctrl
            && !alt
            && self.editor.in_table()
        {
            self.table_tab_key(key.code == KeyCode::BackTab || shift);
            return false;
        }
        if let Some(mutation) = Self::body_key_mutation_kind(&key) {
            if !self.mutation_allowed(mutation) {
                return false;
            }
        }
        match key.code {
            KeyCode::Left if alt && shift => self.navigate_revision(true),
            KeyCode::Right if alt && shift => self.navigate_revision(false),
            KeyCode::Char('a' | 'A') if alt && shift => {
                self.review_current_revision(RevisionAction::Accept)
            }
            KeyCode::Char('r' | 'R') if alt && shift => {
                self.review_current_revision(RevisionAction::Reject)
            }
            // Esc clears a selection but never quits — use Ctrl+Q to quit.
            KeyCode::Esc => {
                if self.editor.has_selection() {
                    self.editor.clear_selection();
                    self.dirty = true;
                }
            }
            KeyCode::Char('f') if alt => self.open_backstage(),
            KeyCode::Char('q') if ctrl => self.request_exit(),
            KeyCode::Char('s') if ctrl => self.save(),
            KeyCode::Char('f') if ctrl => self.enter_find(),
            KeyCode::Char('a') if ctrl => {
                self.editor.select_all();
                self.dirty = true;
            }
            KeyCode::Char('c') if ctrl => self.do_copy(),
            KeyCode::Char('x') if ctrl => self.do_cut(),
            KeyCode::Char('v') if ctrl && alt => self.open_paste_special(),
            KeyCode::Char('v') if ctrl => self.do_paste(),
            KeyCode::Char('b') if ctrl => {
                self.editor.toggle_bold();
                self.after_edit();
            }
            KeyCode::Char('i') if ctrl => {
                self.editor.toggle_italic();
                self.after_edit();
            }
            KeyCode::Char('u') if ctrl => {
                self.editor.toggle_underline();
                self.after_edit();
            }
            // Font shortcuts (Word): grow/shrink, sub/superscript, case, clear.
            KeyCode::Char(']') if ctrl => {
                self.editor.resize_font(2);
                self.after_edit();
            }
            KeyCode::Char('[') if ctrl => {
                self.editor.resize_font(-2);
                self.after_edit();
            }
            KeyCode::Char('=') | KeyCode::Char('+') if ctrl => {
                let tgt = if shift || matches!(key.code, KeyCode::Char('+')) {
                    docxcore::model::VertAlign::Superscript
                } else {
                    docxcore::model::VertAlign::Subscript
                };
                self.editor.toggle_vert_align(tgt);
                self.after_edit();
            }
            KeyCode::F(3) if shift => {
                self.editor.cycle_case();
                self.after_edit();
            }
            KeyCode::Char(' ') if ctrl && shift => {
                // Non-breaking space (Ctrl+Shift+Space), a typesetting staple.
                self.editor.insert_str("\u{00A0}");
                self.after_edit();
            }
            KeyCode::Char(' ') if ctrl => {
                self.editor.clear_run_formatting();
                self.after_edit();
            }
            KeyCode::Char('m') if ctrl => {
                self.editor.change_indent(if shift { -720 } else { 720 });
                self.after_edit();
            }
            KeyCode::Char('l') if ctrl => {
                self.editor.set_align(Align::Left);
                self.after_edit();
            }
            KeyCode::Char('e') if ctrl => {
                self.editor.set_align(Align::Center);
                self.after_edit();
            }
            KeyCode::Char('r') if ctrl => {
                self.editor.set_align(Align::Right);
                self.after_edit();
            }
            KeyCode::Char('j') if ctrl => {
                self.editor.set_align(Align::Justify);
                self.after_edit();
            }
            KeyCode::Char('h') if ctrl => self.enter_find(),
            KeyCode::Char('8') if ctrl && shift => {
                self.invisibles = !self.invisibles;
                self.save_view_prefs();
                self.dirty = true;
            }
            KeyCode::Char('z') if ctrl => {
                if self.editor.undo() {
                    self.after_edit();
                }
            }
            KeyCode::Char('y') if ctrl => {
                if self.editor.redo() {
                    self.after_edit();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                self.editor.insert_char(c);
                self.after_edit();
            }
            KeyCode::Enter => {
                // "---" / "===" / "___" … on a line becomes a horizontal rule.
                let formatting_allowed = self
                    .authorize_mutation(protection::MutationKind::Formatting)
                    .is_ok();
                if formatting_allowed && self.editor.hrule_autoformat() {
                    self.status = Some("Inserted horizontal line".to_string());
                } else {
                    self.editor.insert_newline();
                }
                self.after_edit();
            }
            KeyCode::Backspace => {
                self.editor.backspace();
                self.after_edit();
            }
            KeyCode::Delete => {
                self.editor.delete_forward();
                self.after_edit();
            }
            // Ctrl+Tab types a tab inside a table cell (where the terminal
            // reports it), as in Word.
            KeyCode::Tab if ctrl && self.editor.in_table() => {
                self.editor.insert_tab();
                self.after_edit();
            }
            KeyCode::Tab => {
                self.editor.insert_str("    ");
                self.after_edit();
            }
            KeyCode::Left => {
                self.editor.extend_selection(shift);
                if ctrl {
                    self.clear_visual_hint();
                    self.editor.move_word_left();
                } else {
                    self.move_visual_horiz(false);
                }
                self.dirty = true;
            }
            KeyCode::Right => {
                self.editor.extend_selection(shift);
                if ctrl {
                    self.clear_visual_hint();
                    self.editor.move_word_right();
                } else {
                    self.move_visual_horiz(true);
                }
                self.dirty = true;
            }
            KeyCode::Home => {
                self.editor.extend_selection(shift);
                self.move_visual_line_edge(false);
                self.dirty = true;
            }
            KeyCode::End => {
                self.editor.extend_selection(shift);
                self.move_visual_line_edge(true);
                self.dirty = true;
            }
            KeyCode::Up => {
                self.editor.extend_selection(shift);
                self.move_vert(false);
                self.dirty = true;
            }
            KeyCode::Down => {
                self.editor.extend_selection(shift);
                self.move_vert(true);
                self.dirty = true;
            }
            KeyCode::PageUp => {
                self.editor.extend_selection(shift);
                let n = self.viewport_h.saturating_sub(1).max(1);
                for _ in 0..n {
                    self.move_vert(false);
                }
                self.dirty = true;
            }
            KeyCode::PageDown => {
                self.editor.extend_selection(shift);
                let n = self.viewport_h.saturating_sub(1).max(1);
                for _ in 0..n {
                    self.move_vert(true);
                }
                self.dirty = true;
            }
            KeyCode::F(2) => self.set_page_view(!self.page_view),
            KeyCode::F(3) => {
                self.invisibles = !self.invisibles;
                self.save_view_prefs();
                self.dirty = true;
            }
            KeyCode::F(4) => {
                self.borderless = !self.borderless;
                self.save_view_prefs();
                self.dirty = true;
            }
            KeyCode::F(6) => self.enter_hf_edit(true),
            KeyCode::F(7) => self.enter_hf_edit(false),
            KeyCode::F(8) => self.insert_section(true),
            KeyCode::F(9) => {
                // Focus the ribbon (expanding it); F9 again or Esc leaves.
                self.ribbon_open = true;
                self.ribbon_focus = ribbon::Focus::Tab(self.ribbon.active_tab());
                self.dirty = true;
            }
            _ => {}
        }
        false
    }

    fn draw(&mut self, f: &mut Frame) {
        // The welcome screen overlays everything when launched with no file.
        if self.start_screen {
            f.render_widget(Clear, f.area());
            self.start.draw(f, f.area());
            return;
        }
        // A confirmation modal owns the whole screen — no content behind it.
        if let Some(c) = self.confirm.as_mut() {
            let area = f.area();
            f.render_widget(Clear, area);
            c.draw(f, area);
            return;
        }
        // The File backstage takes over the whole screen.
        if self.backstage.is_some() {
            // `backstagecore::draw` clears the full frame and renders the menu +
            // content below row 0 — draw it first, then paint the ribbon tab
            // strip (File highlighted) over row 0 last so it isn't wiped out.
            let mut bs = self.backstage.take();
            if let Some(b) = bs.as_mut() {
                backstage::draw(f, f.area(), b, self);
            }
            self.backstage = bs;
            // Keep the ribbon tab headers visible: clicking another tab leaves
            // the backstage, and clicking File closes it back to the document —
            // so the panel can be dismissed entirely with the mouse.
            let dim = Style::default().add_modifier(Modifier::DIM);
            let mut tabline = self.ribbon.render_tabs_as(0); // 0 = File
            tabline
                .spans
                .push(RSpan::styled("   (click a tab or Esc to leave)", dim));
            let row0 = Rect {
                x: f.area().x,
                y: f.area().y,
                width: f.area().width,
                height: 1,
            };
            f.render_widget(Paragraph::new(tabline), row0);
            return;
        }
        // The ribbon sits above the document; its height is the collapsed tab
        // strip or the expanded body. Stored so mouse rows can be routed.
        self.ribbon_h = self.ribbon_height();
        // Tell the ribbon which toggles are on (drawn inverted) + the page mode.
        let mut toggles = Vec::new();
        if self.invisibles {
            toggles.push(ribbon::Act::ShowHide);
        }
        if self.show_comments {
            toggles.push(ribbon::Act::ToggleComments);
        }
        if self.show_notes {
            toggles.push(ribbon::Act::ToggleNotes);
        }
        // Read/Print layout applies to `.docx` only; Markdown has no such group.
        if self.format != DocFormat::Markdown {
            toggles.push(if self.page_view {
                ribbon::Act::PrintLayout
            } else {
                ribbon::Act::ReadMode
            });
        }
        if self.show_ruler {
            toggles.push(ribbon::Act::ToggleRuler);
        }
        if self.show_nav {
            toggles.push(ribbon::Act::ToggleNav);
        }
        if self.auto_hide_ribbon {
            toggles.push(ribbon::Act::AutoHideRibbon);
        }
        // The edit-surface switch shows which of body/header/footer is active.
        toggles.push(match &self.hf_edit {
            None => ribbon::Act::EditDocument,
            Some(hf) if hf.is_header => ribbon::Act::EditHeader,
            Some(_) => ribbon::Act::EditFooter,
        });
        // Highlight the Styles button matching the caret paragraph's style.
        if let Some(sid) = self.editor.caret_para_style() {
            if let Some((_, id)) = ribbon::STYLE_BUTTONS.iter().find(|(_, id)| *id == sid) {
                toggles.push(ribbon::Act::ApplyStyle(id));
            }
        }
        // Font toggles reflect the run formatting at the caret.
        let rp = self.editor.caret_props();
        for (on, act) in [
            (rp.bold, ribbon::Act::Bold),
            (rp.italic, ribbon::Act::Italic),
            (rp.underline, ribbon::Act::Underline),
            (rp.strike, ribbon::Act::Strike),
        ] {
            if on {
                toggles.push(act);
            }
        }
        match rp.vert_align {
            docxcore::model::VertAlign::Subscript => toggles.push(ribbon::Act::Subscript),
            docxcore::model::VertAlign::Superscript => toggles.push(ribbon::Act::Superscript),
            docxcore::model::VertAlign::Baseline => {}
        }
        if self.markup != docxcore::markup::MarkupView::All {
            toggles.push(ribbon::Act::CycleMarkup);
        }
        // Markdown files get a contextual View ▸ Markdown group; highlight whichever
        // of Rendered/Source is active.
        let is_md = self.format == DocFormat::Markdown;
        self.ribbon.set_markdown(is_md);
        if is_md {
            toggles.push(if self.md_source {
                ribbon::Act::MdSource
            } else {
                ribbon::Act::MdRendered
            });
        }
        self.ribbon.set_toggles(toggles);
        self.ribbon.set_light_page(self.light_page);
        let chunks = Layout::vertical([
            Constraint::Length(self.ribbon_h as u16),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(f.area());
        self.draw_ribbon(f, chunks[0]);
        let mut full = chunks[1];
        let status = chunks[2];

        // The comments review panel. In read view it docks on the right (the
        // text reflows to fit). In page view the page must NOT shrink, so the
        // panel sits beside the page off-screen and is reached by scrolling the
        // canvas right (Word-style) — handled after the content rect is known.
        self.comments_rect = Rect::default();
        let comments_on = self.show_comments && !self.comments.is_empty();
        let comments_aside = comments_on && self.page_view;
        if comments_on && !comments_aside && full.width > 50 {
            let pw = 40.min(full.width / 2);
            let cols =
                Layout::horizontal([Constraint::Min(10), Constraint::Length(pw)]).split(full);
            full = cols[0];
            self.comments_rect = cols[1];
            self.draw_comments_panel(f, cols[1]);
        }

        // The footnotes/endnotes panel docks on the right, like comments.
        self.notes_rect = Rect::default();
        if self.show_notes && !self.notes.is_empty() && full.width > 50 {
            let pw = 40.min(full.width / 2);
            let cols =
                Layout::horizontal([Constraint::Min(10), Constraint::Length(pw)]).split(full);
            full = cols[0];
            self.notes_rect = cols[1];
            self.draw_notes_panel(f, cols[1]);
        }

        // Navigation (outline) pane on the left.
        self.nav_rect = Rect::default();
        if self.show_nav && full.width > 40 {
            let nw = 26.min(full.width / 3);
            let cols =
                Layout::horizontal([Constraint::Length(nw), Constraint::Min(10)]).split(full);
            self.nav_rect = cols[0];
            full = cols[1];
        }

        // Column ruler at the top of the document area.
        if self.show_ruler && full.height > 2 {
            let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(full);
            self.draw_ruler(f, rows[0]);
            full = rows[1];
        }

        // Reserve a one-column gutter on the right for the vertical scrollbar, so
        // the rendered width stays stable whether or not the bar is shown.
        let gutter = if full.width > 2 { 1 } else { 0 };
        // In aside mode reserve a bottom row for the horizontal scrollbar.
        let hbar = comments_aside && full.height > 3;
        let content = Rect {
            width: full.width - gutter,
            height: full.height - u16::from(hbar),
            ..full
        };
        self.doc_x0 = content.x;
        self.doc_y0 = content.y;

        self.viewport_h = content.height.max(1) as usize;
        self.ensure_rendered(content.width);
        if self.show_nav {
            self.draw_nav_pane(f);
        }

        // Keep the caret in view after keyboard moves, but never fight the user
        // when they're scrolling/dragging the viewport themselves.
        if self.follow_caret {
            if let Some((row, _)) = self.caret_screen() {
                if row < self.scroll {
                    self.scroll = row;
                } else if row >= self.scroll + self.viewport_h {
                    self.scroll = row + 1 - self.viewport_h;
                }
            }
        }
        let max_scroll = self.lines.len().saturating_sub(self.viewport_h);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
        let caret = self.caret_screen();

        let end = (self.scroll + self.viewport_h).min(self.lines.len());
        let visible: &[DocLine] = if self.scroll < self.lines.len() {
            &self.lines[self.scroll..end]
        } else {
            &[]
        };
        // Comments aside: the page keeps its full width; the panel sits beside it
        // at canvas-x = content.width, revealed by scrolling the canvas right.
        let panel_w = if comments_aside {
            40.min(content.width.saturating_sub(20)).max(8)
        } else {
            0
        };
        if comments_aside {
            self.comments_hscroll = self.comments_hscroll.min(panel_w as usize);
        } else {
            self.comments_hscroll = 0;
        }
        self.doc_hscroll = self.comments_hscroll as u16;

        let rlines: Vec<_> = visible.iter().map(doc_line_to_ratatui).collect();
        // A set page colour tints the page sheet in Print Layout (in both
        // terminal themes), with the ink picked by the sheet's luminance; the
        // centering margins and inter-page gaps stay black. Otherwise: light
        // page is black on white. In page view the page sits on a black
        // "desktop" (Word-style) — each line's page region is painted white
        // and the centering margins / inter-page gaps stay black. In
        // continuous view there is no page frame, so the whole content area
        // is white.
        let page_bg = self.page_view.then(|| self.pkg.page_background()).flatten();
        let mut para = if let Some(bg) = page_bg {
            let sheet = rgb_color(bg.color);
            let ink = rgb_color(page_ink(bg.color).0);
            let painted: Vec<_> = rlines
                .into_iter()
                .map(|l| paint_page(l, sheet, ink))
                .collect();
            Paragraph::new(Text::from(painted)).style(Style::default().bg(Color::Black))
        } else if self.light_page {
            if self.page_view {
                let painted: Vec<_> = rlines.into_iter().map(paint_page_on_black).collect();
                Paragraph::new(Text::from(painted)).style(Style::default().bg(Color::Black))
            } else {
                Paragraph::new(Text::from(rlines))
                    .style(Style::default().fg(Color::Black).bg(Color::White))
            }
        } else {
            Paragraph::new(Text::from(rlines))
        };
        if self.doc_hscroll > 0 {
            para = para.scroll((0, self.doc_hscroll));
        }
        f.render_widget(para, content);

        // Paint watermarks as a final, muted screen layer. Because these labels
        // never enter `lines` or `maps`, clicks, the caret, selection, copy,
        // export, and saved OOXML continue to see only real document content.
        self.draw_watermark_overlays(f, content);

        // The comments panel, slid in from the right as the canvas scrolls.
        if comments_aside {
            let h = self.comments_hscroll as u16;
            let panel = Rect {
                x: content.x + content.width.saturating_sub(h),
                y: content.y,
                width: panel_w,
                height: content.height,
            };
            self.comments_rect = panel;
            self.draw_comments_panel(f, panel);
            // Horizontal scrollbar on the reserved bottom row.
            if hbar {
                let canvas = content.width as usize + panel_w as usize;
                let mut sb = ScrollbarState::new(canvas)
                    .position(self.comments_hscroll)
                    .viewport_content_length(content.width as usize);
                f.render_stateful_widget(
                    Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
                        .begin_symbol(None)
                        .end_symbol(None),
                    Rect {
                        x: content.x,
                        y: content.y + content.height,
                        width: content.width,
                        height: 1,
                    },
                    &mut sb,
                );
            }
        }

        // Overlay real image pixels onto the placeholder boxes. Each image is
        // encoded once per visible window and just re-emitted as it moves, so
        // this stays cheap while scrolling (the loop caps the redraw rate).
        self.draw_images(f, content);

        // Vertical scrollbar in the reserved gutter, when the document overflows.
        if gutter == 1 && self.lines.len() > self.viewport_h {
            let mut sb = ScrollbarState::new(self.lines.len())
                .position(self.scroll)
                .viewport_content_length(self.viewport_h);
            let area = Rect {
                x: full.x + content.width,
                y: full.y,
                width: 1,
                height: full.height,
            };
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                area,
                &mut sb,
            );
        }

        // Caret (shifted by the horizontal scroll; hidden if scrolled off-screen).
        if let Some((row, col)) = caret {
            let hs = self.doc_hscroll as usize;
            if row >= self.scroll
                && row < self.scroll + self.viewport_h
                && col >= hs
                && (col - hs) < content.width as usize
            {
                let x = content.x + (col - hs) as u16;
                let y = content.y + (row - self.scroll) as u16;
                f.set_cursor_position(Position { x, y });
            }
        }

        let total_lines = self.lines.len().max(1);
        let (cr, cc) = caret.map(|(r, c)| (r + 1, c + 1)).unwrap_or((0, 0));
        let dirty_mark = if self.modified { "*" } else { " " };
        let surface = match &self.hf_edit {
            Some(hf) if hf.is_header => "[HEADER] ",
            Some(_) => "[FOOTER] ",
            None => "",
        };
        let left = format!(
            " {}{}{}  │  ln {cr} col {cc}  │  {} lines  │  pg:{} marks:{} brd:{} ",
            surface,
            dirty_mark,
            self.path,
            total_lines,
            on_off(self.page_view),
            on_off(self.invisibles),
            on_off(!self.borderless),
        );
        let status_text = if let Some(draft) = &self.comment_input {
            format!(" New comment: {draft}▏   ( Enter = add · Esc = cancel )")
        } else if let Some(url) = &self.pending_link {
            format!(" Open this link?  {url}   ( y = open in browser · any other key = cancel )")
        } else if let Some(f) = &self.find {
            let n = f.matches.len();
            let cur = if n > 0 { f.idx + 1 } else { 0 };
            let read_only = f.matches.get(f.idx).is_some_and(|m| !m.editable);
            let cur = if read_only {
                format!("{cur}/{n} read-only")
            } else {
                format!("{cur}/{n}")
            };
            match &f.replacement {
                None => format!(
                    " Find: {}▏  ({cur})  ·  ↵/↓ next · ↑ prev · Tab→replace · Esc done",
                    f.query
                ),
                Some(repl) => {
                    let (qc, rc) = if f.editing_replacement {
                        ("", "▏")
                    } else {
                        ("▏", "")
                    };
                    format!(
                        " Replace: {}{qc} → {}{rc}  ({cur})  ·  ↵ replace · Ctrl-A all · Tab field · Esc done",
                        f.query, repl
                    )
                }
            }
        } else if let Some(v) = &self.vim {
            if let Some(cmd) = &v.cmdline {
                format!(":{cmd}▏")
            } else {
                let m = match v.mode {
                    VimMode::Normal => "-- NORMAL --",
                    VimMode::Insert => "-- INSERT --",
                    VimMode::Visual => "-- VISUAL --",
                    VimMode::VisualLine => "-- V-LINE --",
                };
                let pending = if v.count.is_empty() && v.pending_op.is_none() {
                    String::new()
                } else {
                    format!(
                        "  {}{}",
                        v.count,
                        v.pending_op.map(|c| c.to_string()).unwrap_or_default()
                    )
                };
                match &self.status {
                    Some(msg) => format!(
                        " {m} {dirty_mark}{}  │ {msg}{}",
                        self.path,
                        self.doc_notice()
                    ),
                    None => format!(
                        " {m}  │ {dirty_mark}{}  ln {cr} col {cc}{pending}{}",
                        self.path,
                        self.doc_notice()
                    ),
                }
            }
        } else {
            match &self.status {
                Some(msg) => format!("{left} │ {msg}{}", self.doc_notice()),
                None => format!(
                    "{left}│ Ctrl-S save · Ctrl-F find · Ctrl-Q quit{}",
                    self.doc_notice()
                ),
            }
        };
        let status_widget =
            Paragraph::new(status_text).style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_widget(status_widget, status);

        // The Paste Special dialog floats on top of the document (so the paste
        // target stays visible behind it).
        if self.paste_special.is_some() {
            self.draw_paste_special(f, f.area());
        }
        if self.insert_field.is_some() {
            self.draw_insert_field(f, f.area());
        }
        if self.para_dialog.is_some() {
            self.draw_para_dialog(f, f.area());
        }
        if self.compare_dialog.is_some() {
            self.draw_compare_dialog(f, f.area());
        }
        if self.styles_dialog.is_some() {
            self.draw_styles_dialog(f, f.area());
        }
        if self.font_picker.is_some() {
            self.draw_picker(f, f.area());
        }
    }

    fn draw_watermark_overlays(&self, f: &mut Frame, content: Rect) {
        if !self.page_view || content.width == 0 || content.height == 0 {
            return;
        }
        let style = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM);
        let hscroll = self.doc_hscroll as usize;
        let buf = f.buffer_mut();
        for overlay in &self.watermark_overlays {
            let Some(view_row) = overlay.row.checked_sub(self.scroll) else {
                continue;
            };
            if view_row >= content.height as usize {
                continue;
            }

            let (view_col, text) = if overlay.col >= hscroll {
                (overlay.col - hscroll, overlay.text.clone())
            } else {
                let (gap, suffix) =
                    watermark::suffix_after_cols(&overlay.text, hscroll - overlay.col);
                (gap, suffix)
            };
            if text.is_empty() || view_col >= content.width as usize {
                continue;
            }
            let max_width = content.width as usize - view_col;
            buf.set_stringn(
                content.x + view_col as u16,
                content.y + view_row as u16,
                text,
                max_width,
                style,
            );
        }
    }

    fn draw_picker(&mut self, f: &mut Frame, area: Rect) {
        let Some(pk) = &self.font_picker else {
            return;
        };
        let items = pk.kind.items();
        let title = pk.kind.title();
        let sel = pk.sel;
        let n = items.len() as u16;
        // Scroll the list so the selection stays visible in a capped window.
        let max_rows = (area.height.saturating_sub(6)).clamp(3, 14);
        let view = n.min(max_rows);
        let top = (sel as u16)
            .saturating_sub(view.saturating_sub(1))
            .min(n - view);
        let inner_h = view + 2; // list + blank + buttons
        let w = 30u16.clamp(20, area.width.saturating_sub(2).max(20));
        let h = (inner_h + 2).min(area.height);
        let rect = Rect {
            x: area.x + area.width.saturating_sub(w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, rect);
        let block = RBlock::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(title);
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        let on_style = Style::default().fg(Color::Black).bg(Color::Cyan);
        self.fp_rows.clear();
        for row in 0..view {
            let i = (top + row) as usize;
            let r = Rect {
                x: inner.x + 1,
                y: inner.y + row,
                width: inner.width.saturating_sub(2),
                height: 1,
            };
            let on = i == sel;
            let label = format!(" {} {}", if on { "▶" } else { " " }, items[i]);
            f.render_widget(
                Paragraph::new(label).style(if on { on_style } else { Style::default() }),
                r,
            );
            self.fp_rows.push(r);
        }

        let (ok, cl) = (" OK ", " Cancel ");
        let (ow, cw) = (ok.len() as u16, cl.len() as u16);
        let total = ow + 2 + cw;
        let bx = inner.x + inner.width.saturating_sub(total) / 2;
        let by = inner.y + inner.height.saturating_sub(1);
        let ok_rect = Rect {
            x: bx,
            y: by,
            width: ow,
            height: 1,
        };
        let cancel_rect = Rect {
            x: bx + ow + 2,
            y: by,
            width: cw,
            height: 1,
        };
        let unsel = Style::default().add_modifier(Modifier::REVERSED);
        f.render_widget(Paragraph::new(ok).style(on_style), ok_rect);
        f.render_widget(Paragraph::new(cl).style(unsel), cancel_rect);
        self.fp_btns = [ok_rect, cancel_rect];
    }

    fn draw_insert_field(&mut self, f: &mut Frame, area: Rect) {
        let Some(d) = &self.insert_field else {
            return;
        };
        let sel = d.sel;
        let n = FieldKind::ALL.len() as u16;
        // "Field:"(1) + options(n) + blank(1) + preview(1) + blank(1) + buttons(1).
        let inner_h = 1 + n + 1 + 1 + 1 + 1;
        let w = 46u16.clamp(28, area.width.saturating_sub(2).max(28));
        let h = (inner_h + 2).min(area.height);
        let rect = Rect {
            x: area.x + area.width.saturating_sub(w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, rect);
        let block = RBlock::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Insert Field ");
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        let line = |row: u16| Rect {
            x: inner.x + 1,
            y: inner.y + row,
            width: inner.width.saturating_sub(2),
            height: 1,
        };
        f.render_widget(Paragraph::new("Field:"), line(0));
        let on_style = Style::default().fg(Color::Black).bg(Color::Cyan);
        self.if_rows.clear();
        for (i, k) in FieldKind::ALL.iter().enumerate() {
            let r = line(1 + i as u16);
            let on = i == sel;
            let label = format!(" {} {}", if on { "▶" } else { " " }, k.label());
            f.render_widget(
                Paragraph::new(label).style(if on { on_style } else { Style::default() }),
                r,
            );
            self.if_rows.push(r);
        }
        // Live preview of the selected field's value.
        let preview = FieldKind::ALL
            .get(sel)
            .map(|k| self.field_value(*k))
            .unwrap_or_default();
        let pv = if preview.trim().is_empty() {
            "(empty)".to_string()
        } else {
            preview
        };
        f.render_widget(
            Paragraph::new(format!("Preview:  {pv}"))
                .style(Style::default().add_modifier(Modifier::DIM)),
            line(1 + n + 1),
        );

        let (il, cl) = (" Insert ", " Cancel ");
        let (iw, cw) = (il.len() as u16, cl.len() as u16);
        let total = iw + 2 + cw;
        let bx = inner.x + inner.width.saturating_sub(total) / 2;
        let by = inner.y + inner.height.saturating_sub(1);
        let insert_rect = Rect {
            x: bx,
            y: by,
            width: iw,
            height: 1,
        };
        let cancel_rect = Rect {
            x: bx + iw + 2,
            y: by,
            width: cw,
            height: 1,
        };
        let unsel = Style::default().add_modifier(Modifier::REVERSED);
        f.render_widget(Paragraph::new(il).style(on_style), insert_rect);
        f.render_widget(Paragraph::new(cl).style(unsel), cancel_rect);
        self.if_btns = [insert_rect, cancel_rect];
    }

    fn draw_para_dialog(&mut self, f: &mut Frame, area: Rect) {
        let Some(d) = &self.para_dialog else {
            return;
        };
        // 3 setting rows + blank + hint + blank + buttons, inside a border.
        let inner_h = ParagraphDialog::ROWS as u16 + 1 + 1 + 1 + 1;
        let w = 44u16.clamp(30, area.width.saturating_sub(2).max(30));
        let h = (inner_h + 2).min(area.height);
        let rect = Rect {
            x: area.x + area.width.saturating_sub(w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, rect);
        let block = RBlock::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Paragraph ");
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        let line = |row: u16| Rect {
            x: inner.x + 1,
            y: inner.y + row,
            width: inner.width.saturating_sub(2),
            height: 1,
        };
        let on_style = Style::default().fg(Color::Black).bg(Color::Cyan);
        let special = ["(none)", "First line", "Hanging"][d.special.min(2) as usize];
        // Each row shows "Label:   ◂ value ▸" so the arrows hint the steppers.
        let rows = [
            ("Left indent", twips_in(d.left)),
            ("Special", special.to_string()),
            ("By", twips_in(d.by)),
        ];
        self.pd_rows.clear();
        for (i, (label, value)) in rows.iter().enumerate() {
            let r = line(i as u16);
            let on = i == d.sel;
            // "By" is irrelevant when no special indent is set — dim it.
            let muted = i == 2 && d.special == 0;
            let arrows = if on {
                format!("◂ {value} ▸")
            } else {
                value.clone()
            };
            let text = format!(
                " {:<13}{:>width$}",
                format!("{label}:"),
                arrows,
                width = inner.width.saturating_sub(16) as usize
            );
            let style = if on {
                on_style
            } else if muted {
                Style::default().add_modifier(Modifier::DIM)
            } else {
                Style::default()
            };
            f.render_widget(Paragraph::new(text).style(style), r);
            self.pd_rows.push(r);
        }
        f.render_widget(
            Paragraph::new("↑↓ row · ←→ adjust · Enter apply")
                .style(Style::default().add_modifier(Modifier::DIM)),
            line(ParagraphDialog::ROWS as u16 + 1),
        );

        let (ol, cl) = (" OK ", " Cancel ");
        let (ow, cw) = (ol.len() as u16, cl.len() as u16);
        let total = ow + 2 + cw;
        let bx = inner.x + inner.width.saturating_sub(total) / 2;
        let by = inner.y + inner.height.saturating_sub(1);
        let ok_rect = Rect {
            x: bx,
            y: by,
            width: ow,
            height: 1,
        };
        let cancel_rect = Rect {
            x: bx + ow + 2,
            y: by,
            width: cw,
            height: 1,
        };
        let unsel = Style::default().add_modifier(Modifier::REVERSED);
        f.render_widget(Paragraph::new(ol).style(on_style), ok_rect);
        f.render_widget(Paragraph::new(cl).style(unsel), cancel_rect);
        self.pd_btns = [ok_rect, cancel_rect];
    }

    fn draw_compare_dialog(&mut self, f: &mut Frame, area: Rect) {
        let Some(d) = &self.compare_dialog else {
            return;
        };
        // 2 path rows + blank + hint + blank + buttons, inside a border.
        let inner_h = 2 + 1 + 1 + 1 + 1;
        let w = 64u16
            .min(area.width.saturating_sub(2))
            .max(30.min(area.width));
        let h = (inner_h + 2).min(area.height);
        let rect = Rect {
            x: area.x + area.width.saturating_sub(w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, rect);
        let block = RBlock::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Compare Documents ");
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        let line = |row: u16| Rect {
            x: inner.x + 1,
            y: inner.y + row,
            width: inner.width.saturating_sub(2),
            height: 1,
        };
        let on_style = Style::default().fg(Color::Black).bg(Color::Cyan);
        self.cd_rows.clear();
        for (i, (label, value)) in [("Original", &d.original), ("Revised", &d.revised)]
            .into_iter()
            .enumerate()
        {
            let r = line(i as u16);
            let on = i == d.field;
            // Show the end of a long path (the file name), plus a caret.
            let room = (r.width as usize).saturating_sub(12);
            let chars: Vec<char> = value.chars().collect();
            let shown: String = chars[chars.len().saturating_sub(room)..].iter().collect();
            let caret = if on { "▏" } else { "" };
            let text = format!(" {:<10}{shown}{caret}", format!("{label}:"));
            let style = if on { on_style } else { Style::default() };
            f.render_widget(Paragraph::new(text).style(style), r);
            self.cd_rows.push(r);
        }
        f.render_widget(
            Paragraph::new("Tab field · type a .docx path · Enter compare")
                .style(Style::default().add_modifier(Modifier::DIM)),
            line(3),
        );

        let (ol, cl) = (" Compare ", " Cancel ");
        let (ow, cw) = (ol.len() as u16, cl.len() as u16);
        let total = ow + 2 + cw;
        let bx = inner.x + inner.width.saturating_sub(total) / 2;
        let by = inner.y + inner.height.saturating_sub(1);
        let ok_rect = Rect {
            x: bx,
            y: by,
            width: ow,
            height: 1,
        };
        let cancel_rect = Rect {
            x: bx + ow + 2,
            y: by,
            width: cw,
            height: 1,
        };
        let unsel = Style::default().add_modifier(Modifier::REVERSED);
        f.render_widget(Paragraph::new(ol).style(on_style), ok_rect);
        f.render_widget(Paragraph::new(cl).style(unsel), cancel_rect);
        self.cd_btns = [ok_rect, cancel_rect];
    }

    fn draw_styles_dialog(&mut self, f: &mut Frame, area: Rect) {
        let n = match &self.styles_dialog {
            Some(d) => d.items.len(),
            None => return,
        };
        let w = 40u16.clamp(24, area.width.saturating_sub(2).max(24));
        // Up to ~16 visible rows, but never taller than the screen (+border+buttons).
        let max_list = 16u16;
        let list_h = max_list.min(area.height.saturating_sub(4)).max(1);
        let h = (list_h + 4).min(area.height); // border(2) + list + blank(1) + buttons(1)
        let rect = Rect {
            x: area.x + area.width.saturating_sub(w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, rect);
        let block = RBlock::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Apply Styles ");
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        let vis = inner.height.saturating_sub(2) as usize; // leave a row for buttons + gap
        let vis = vis.max(1);
        let sel = self.styles_dialog.as_ref().unwrap().sel;
        let top = sel.saturating_sub(vis / 2).min(n.saturating_sub(vis));
        if let Some(d) = self.styles_dialog.as_mut() {
            d.top = top;
        }
        let on_style = Style::default().fg(Color::Black).bg(Color::Cyan);
        self.sd_rows.clear();
        let items = &self.styles_dialog.as_ref().unwrap().items;
        for row in 0..vis {
            let idx = top + row;
            if idx >= n {
                break;
            }
            let r = Rect {
                x: inner.x + 1,
                y: inner.y + row as u16,
                width: inner.width.saturating_sub(2),
                height: 1,
            };
            let (id, name) = &items[idx];
            let on = idx == sel;
            // Show the display name, with the style id dimmed when it differs.
            let label = if name.eq_ignore_ascii_case(id) {
                format!(" {} {}", if on { "▶" } else { " " }, name)
            } else {
                format!(" {} {}  ({id})", if on { "▶" } else { " " }, name)
            };
            f.render_widget(
                Paragraph::new(label).style(if on { on_style } else { Style::default() }),
                r,
            );
            self.sd_rows.push(r);
        }

        let (ol, cl) = (" Apply ", " Cancel ");
        let (ow, cw) = (ol.len() as u16, cl.len() as u16);
        let total = ow + 2 + cw;
        let bx = inner.x + inner.width.saturating_sub(total) / 2;
        let by = inner.y + inner.height.saturating_sub(1);
        let ok_rect = Rect {
            x: bx,
            y: by,
            width: ow,
            height: 1,
        };
        let cancel_rect = Rect {
            x: bx + ow + 2,
            y: by,
            width: cw,
            height: 1,
        };
        let unsel = Style::default().add_modifier(Modifier::REVERSED);
        f.render_widget(Paragraph::new(ol).style(on_style), ok_rect);
        f.render_widget(Paragraph::new(cl).style(unsel), cancel_rect);
        self.sd_btns = [ok_rect, cancel_rect];
    }

    fn draw_paste_special(&mut self, f: &mut Frame, area: Rect) {
        let Some(ps) = &self.paste_special else {
            return;
        };
        let n = ps.opts.len() as u16;
        // source(1) + blank(1) + "As:"(1) + options(n) + blank(1) + result(2) +
        // blank(1) + buttons(1), inside a border.
        let inner_h = 1 + 1 + 1 + n + 1 + 2 + 1 + 1;
        let w = 52u16.clamp(28, area.width.saturating_sub(2).max(28));
        let h = (inner_h + 2).min(area.height);
        let rect = Rect {
            x: area.x + area.width.saturating_sub(w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, rect);
        let block = RBlock::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Paste Special ");
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        let line = |row: u16, height: u16| Rect {
            x: inner.x + 1,
            y: inner.y + row,
            width: inner.width.saturating_sub(2),
            height,
        };
        // Source line.
        f.render_widget(
            Paragraph::new(format!("Source:  {}", ps.source))
                .style(Style::default().add_modifier(Modifier::DIM)),
            line(0, 1),
        );
        f.render_widget(Paragraph::new("As:"), line(2, 1));

        // Option rows.
        let sel = Style::default().fg(Color::Black).bg(Color::Cyan);
        self.ps_rows.clear();
        for (i, opt) in ps.opts.iter().enumerate() {
            let r = line(3 + i as u16, 1);
            let on = i == ps.sel;
            let label = format!(" {} {}", if on { "▶" } else { " " }, opt.label());
            let style = if on { sel } else { Style::default() };
            f.render_widget(Paragraph::new(label).style(style), r);
            self.ps_rows.push(r);
        }

        // Result description for the highlighted option.
        let res_y = 3 + n + 1;
        let result = ps.opts.get(ps.sel).map(|o| o.result()).unwrap_or("");
        f.render_widget(
            Paragraph::new(result)
                .wrap(Wrap { trim: true })
                .style(Style::default().add_modifier(Modifier::DIM)),
            line(res_y, 2),
        );

        // [ Paste ] [ Cancel ] buttons on the bottom row.
        let (pl, cl) = (" Paste ", " Cancel ");
        let (pw, cw) = (pl.len() as u16, cl.len() as u16);
        let total = pw + 2 + cw;
        let bx = inner.x + inner.width.saturating_sub(total) / 2;
        let by = inner.y + inner.height.saturating_sub(1);
        let paste_rect = Rect {
            x: bx,
            y: by,
            width: pw,
            height: 1,
        };
        let cancel_rect = Rect {
            x: bx + pw + 2,
            y: by,
            width: cw,
            height: 1,
        };
        let unsel = Style::default().add_modifier(Modifier::REVERSED);
        f.render_widget(Paragraph::new(pl).style(sel), paste_rect);
        f.render_widget(Paragraph::new(cl).style(unsel), cancel_rect);
        self.ps_btns = [paste_rect, cancel_rect];
    }
}

/// Format-specific content the shared File backstage needs from docxy: only
/// `.docx` files are listed/opened, the Save As default is the current file's
/// name, the preview renders the highlighted `.docx`, the Info pane shows
/// document stats, and the accent matches docxy's ribbon (light blue).
impl backstage::BackstageHost for App {
    fn extensions(&self) -> &'static [&'static str] {
        // `html` lists editable-HTML bundles, whatever they are called; opening
        // an .html that is not one says "not a docxy editable HTML file".
        &["docx", "html"]
    }

    fn default_save_name(&self) -> String {
        std::path::Path::new(&self.path)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled.docx".to_string())
    }

    /// Render a quick preview of the highlighted `.docx`.
    fn preview_lines(&self, path: &std::path::Path, width: usize) -> Vec<String> {
        let w = width.max(8);
        let bytes = if format_for(&path.to_string_lossy()) == DocFormat::Html {
            html::open(&path.to_string_lossy()).ok().map(|o| o.docx)
        } else {
            std::fs::read(path).ok()
        };
        match bytes.and_then(|d| load_package(&d).ok()) {
            Some(pkg) => {
                let styles = pkg
                    .part("word/styles.xml")
                    .map(|b| parse_styles_xml(std::str::from_utf8(b).unwrap_or("")))
                    .unwrap_or_default();
                let opts = RenderOptions {
                    width: w,
                    styles: Rc::new(styles),
                    bidi: Some(bidi::projector()),
                    ..RenderOptions::default()
                };
                docxcore::render::render(&pkg.document, &opts)
                    .iter()
                    .take(120)
                    .map(|l| l.plain())
                    .collect()
            }
            None => vec!["(cannot read this file)".to_string()],
        }
    }

    fn info_lines(&self) -> Vec<ratatui::text::Line<'static>> {
        let text = self.editor.doc.plain_text();
        let words = text.split_whitespace().count();
        let chars = text.chars().filter(|c| !c.is_whitespace()).count();
        let paras = self
            .editor
            .doc
            .body
            .iter()
            .filter(|b| matches!(b, Block::Paragraph(_)))
            .count();
        vec![
            RLine::raw(format!("  File        {}", self.path)),
            RLine::raw(format!(
                "  Modified    {}",
                if self.modified { "yes" } else { "no" }
            )),
            RLine::raw(String::new()),
            RLine::raw(format!("  Paragraphs  {paras}")),
            RLine::raw(format!("  Words       {words}")),
            RLine::raw(format!("  Characters  {chars}")),
        ]
    }

    fn accent(&self) -> Color {
        Color::LightBlue
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

/// Word-wrap `s` to lines of at most `w` columns (by char count). Long words are
/// hard-broken. An empty input yields a single empty line.
fn wrap_str(s: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut len = 0usize;
    for word in s.split_whitespace() {
        let wl = word.chars().count();
        if wl > w {
            // hard-break an over-long word
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
                len = 0;
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                chunk.push(ch);
                if chunk.chars().count() == w {
                    out.push(std::mem::take(&mut chunk));
                }
            }
            if !chunk.is_empty() {
                line = chunk;
                len = line.chars().count();
            }
            continue;
        }
        let extra = if len == 0 { wl } else { wl + 1 };
        if len + extra > w {
            out.push(std::mem::take(&mut line));
            len = 0;
        }
        if len > 0 {
            line.push(' ');
            len += 1;
        }
        line.push_str(word);
        len += wl;
    }
    out.push(line);
    out
}

/// Does a block (recursively, through table cells) hold a `<w:bookmarkStart>`
/// whose raw XML contains `needle` (the `w:name="…"` attribute)?
fn block_has_bookmark(b: &Block, needle: &str) -> bool {
    match b {
        Block::Paragraph(p) => p.content.iter().any(
            |i| matches!(i, Inline::Raw(s) if s.contains("bookmarkStart") && s.contains(needle)),
        ),
        Block::Table(t) => t.rows.iter().any(|r| {
            r.cells
                .iter()
                .any(|c| c.blocks.iter().any(|bb| block_has_bookmark(bb, needle)))
        }),
        Block::SectionProperties(_) | Block::Raw(_) => false,
    }
}

/// Truncate `s` to at most `w` columns, ending with `…` when clipped.
fn fit_width(s: &str, w: usize) -> String {
    if w == 0 {
        return String::new();
    }
    if s.chars().count() <= w {
        return s.to_string();
    }
    let mut out: String = s.chars().take(w - 1).collect();
    out.push('…');
    out
}

/// Persisted view-mode toggles (print layout, invisibles, table borders), so
/// they survive across sessions. Stored as a tiny `key=1/0` file in the user's
/// config directory.
#[derive(Clone, Copy, Default)]
struct ViewPrefs {
    page_view: bool,
    invisibles: bool,
    borderless: bool,
    light_page: bool,
    show_ruler: bool,
    show_nav: bool,
    show_comments: bool,
    show_notes: bool,
    auto_hide_ribbon: bool,
}

impl ViewPrefs {
    fn path() -> Option<std::path::PathBuf> {
        let dir = if cfg!(windows) {
            std::env::var_os("APPDATA").map(std::path::PathBuf::from)
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
                })
        }?;
        Some(dir.join("docxy").join("view.conf"))
    }

    fn parse(text: &str) -> ViewPrefs {
        let mut p = ViewPrefs::default();
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                let on = v.trim() == "1";
                match k.trim() {
                    "page_view" => p.page_view = on,
                    "invisibles" => p.invisibles = on,
                    "borderless" => p.borderless = on,
                    "light_page" => p.light_page = on,
                    "show_ruler" => p.show_ruler = on,
                    "show_nav" => p.show_nav = on,
                    "show_comments" => p.show_comments = on,
                    "show_notes" => p.show_notes = on,
                    "auto_hide_ribbon" => p.auto_hide_ribbon = on,
                    _ => {}
                }
            }
        }
        p
    }

    fn to_conf(self) -> String {
        format!(
            "page_view={}\ninvisibles={}\nborderless={}\nlight_page={}\nshow_ruler={}\nshow_nav={}\nshow_comments={}\nshow_notes={}\nauto_hide_ribbon={}\n",
            self.page_view as u8,
            self.invisibles as u8,
            self.borderless as u8,
            self.light_page as u8,
            self.show_ruler as u8,
            self.show_nav as u8,
            self.show_comments as u8,
            self.show_notes as u8,
            self.auto_hide_ribbon as u8,
        )
    }

    fn load() -> ViewPrefs {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Self::parse(&t))
            .unwrap_or_default()
    }

    fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&path, self.to_conf());
    }
}

/// Draw a dim bordered box with a centered caption for an image we can't render
/// (no graphics support, missing bytes, or an undecodable format such as a
/// formula preview). Each visible cell is placed from the box's absolute
/// geometry, so it scrolls and clips correctly. This is the only case where a
/// borderless picture gets a border.
fn draw_fallback_box(f: &mut Frame, content: Rect, ib: &ImageBox, scroll: usize, label: &str) {
    let (rows, cols) = (ib.rows, ib.cols);
    if rows == 0 || cols == 0 {
        return;
    }
    let dim = Style::default().add_modifier(Modifier::DIM);
    let inner_w = cols.saturating_sub(2);
    let lab: Vec<char> = label.chars().take(inner_w).collect();
    let lab_start = 1 + inner_w.saturating_sub(lab.len()) / 2;
    let label_row = rows / 2;
    let x_end = (content.x + content.width) as usize;
    let y_end = (content.y + content.height) as usize;
    let buf = f.buffer_mut();
    for r in 0..rows {
        let sy = ib.row as isize + r as isize - scroll as isize;
        if sy < 0 {
            continue;
        }
        let sy = content.y as usize + sy as usize;
        if sy >= y_end {
            break;
        }
        for c in 0..cols {
            let sx = content.x as usize + ib.col + c;
            if sx >= x_end {
                break;
            }
            let edge = if r == 0 {
                if c == 0 {
                    '┌'
                } else if c == cols - 1 {
                    '┐'
                } else {
                    '─'
                }
            } else if r == rows - 1 {
                if c == 0 {
                    '└'
                } else if c == cols - 1 {
                    '┘'
                } else {
                    '─'
                }
            } else if c == 0 || c == cols - 1 {
                '│'
            } else {
                ' '
            };
            let ch = if r == label_row && c >= lab_start && c < lab_start + lab.len() {
                lab[c - lab_start]
            } else {
                edge
            };
            if let Some(cell) = buf.cell_mut(Position {
                x: sx as u16,
                y: sy as u16,
            }) {
                cell.set_char(ch).set_style(dim);
            }
        }
    }
}

/// Only plain **internet links** (http/https) are ever opened. Everything else
/// — `file:`, `mailto:`, `javascript:`, `data:`, custom schemes, control
/// characters — is refused, so a link can never invoke a local OS handler or
/// hide a destructive action behind innocent-looking text.
fn safe_url(url: &str) -> bool {
    if url.is_empty() || url.len() > 2048 {
        return false;
    }
    if url.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}') {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Whether the clipboard text looks like a single URL (so Paste Special can offer
/// "Paste as Hyperlink"). A single token with a web/mail scheme or a `www.` host.
fn looks_like_url(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && !t.contains(char::is_whitespace)
        && (t.starts_with("http://")
            || t.starts_with("https://")
            || t.starts_with("mailto:")
            || t.starts_with("www."))
}

/// Whether an internal clip would carry direct character formatting into the
/// destination rather than merely inserting content in the caret's style.
fn clip_has_formatting(clip: &Clip) -> bool {
    fn blocks_have_formatting(blocks: &[Block]) -> bool {
        blocks.iter().any(|block| match block {
            Block::Paragraph(paragraph) => {
                paragraph.props != Default::default()
                    || paragraph.content.iter().any(inline_has_formatting)
            }
            Block::Table(table) => table.rows.iter().any(|row| {
                row.cells
                    .iter()
                    .any(|cell| blocks_have_formatting(&cell.blocks))
            }),
            Block::SectionProperties(_) | Block::Raw(_) => false,
        })
    }

    fn inline_has_formatting(inline: &Inline) -> bool {
        match inline {
            Inline::Run(run) => run.props != RunProps::default(),
            Inline::Hyperlink(link) => {
                link.runs.iter().any(|run| run.props != RunProps::default())
                    || link.content.iter().any(inline_has_formatting)
            }
            // A tab or a break is a run in OOXML and carries its own rPr (#279).
            Inline::Tab(props) | Inline::Break(_, props) => *props != RunProps::default(),
            Inline::Revision { content, .. } => content.iter().any(inline_has_formatting),
            Inline::TextBox { blocks, .. } => blocks_have_formatting(blocks),
            Inline::SmartArt { .. }
            | Inline::Chart { .. }
            | Inline::Equation { .. }
            | Inline::Field { .. }
            | Inline::UnsupportedRevision { .. }
            | Inline::FootnoteRef { .. }
            | Inline::Raw(_) => false,
        }
    }

    clip.paras.iter().flatten().any(inline_has_formatting)
}

/// Open a URL with the OS default handler — **without a shell** (the URL is
/// passed as a direct argument), and only after [`safe_url`] has approved it.
fn open_url(url: &str) {
    use std::process::Command;
    if !safe_url(url) {
        return;
    }
    #[cfg(target_os = "windows")]
    let _ = Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = Command::new("xdg-open").arg(url).spawn();
}

/// The current local date-time for field evaluation (DATE/TIME). On Windows this
/// is the OS local clock; elsewhere it falls back to UTC.
#[cfg(windows)]
fn local_now() -> Option<docxcore::field::DateTime> {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut st = unsafe { std::mem::zeroed::<windows_sys::Win32::Foundation::SYSTEMTIME>() };
    unsafe { GetLocalTime(&mut st) };
    Some(docxcore::field::DateTime {
        year: st.wYear as i32,
        month: st.wMonth as u32,
        day: st.wDay as u32,
        hour: st.wHour as u32,
        min: st.wMinute as u32,
        sec: st.wSecond as u32,
        weekday: st.wDayOfWeek as u32,
    })
}

#[cfg(not(windows))]
fn local_now() -> Option<docxcore::field::DateTime> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(docxcore::field::civil_from_unix(secs))
}

fn map_color(c: DocColor) -> Color {
    match c {
        DocColor::Black => Color::Black,
        DocColor::Red => Color::Red,
        DocColor::Green => Color::Green,
        DocColor::Yellow => Color::Yellow,
        DocColor::Blue => Color::Blue,
        DocColor::Magenta => Color::Magenta,
        DocColor::Cyan => Color::Cyan,
        DocColor::White => Color::Gray,
        DocColor::Gray => Color::DarkGray,
        DocColor::BrightRed => Color::LightRed,
        DocColor::BrightGreen => Color::LightGreen,
        DocColor::BrightYellow => Color::LightYellow,
        DocColor::BrightBlue => Color::LightBlue,
        DocColor::BrightMagenta => Color::LightMagenta,
        DocColor::BrightCyan => Color::LightCyan,
        DocColor::BrightWhite => Color::White,
    }
}

/// The four-sided Box/Shadow page borders the Design picker and the
/// `doc.page-borders` verb write: single 4 (0.5pt) sides 24pt from the page
/// edge, on every page, in front of the sheet, with an optional side colour.
pub(crate) fn box_page_borders(shadow: bool, color: Option<u32>) -> PageBorders {
    let side = || {
        Some(BorderSide {
            style: "single".into(),
            sz: 4,
            space: 24,
            color,
            shadow,
            frame: false,
        })
    };
    PageBorders {
        sides: [side(), side(), side(), side()],
        display: PgBorderDisplay::AllPages,
        offset_from: PgBorderOffset::Page,
        z_order_back: false,
    }
}

/// The text and dimmed-text colours to draw on a page sheet of colour
/// `sheet`: dark ink on a light sheet, light ink on a dark one, as Word
/// draws automatic text. A copy of the suite oracle (`page_ink` in
/// suite/docxy/src/design_tab.rs); painting uses the first element.
fn page_ink(sheet: u32) -> (u32, u32) {
    let [r, g, b] = [16, 8, 0].map(|s| f32::from(((sheet >> s) & 0xFF) as u8) / 255.0);
    // Relative luminance (Rec. 709 weights on the gamma-encoded channels is
    // close enough to pick a side).
    if 0.2126 * r + 0.7152 * g + 0.0722 * b < 0.45 {
        (0xF2F2F2, 0xB0B0B0)
    } else {
        (0x202020, 0x808080)
    }
}

fn rgb_color(rgb: u32) -> Color {
    Color::Rgb(
        ((rgb >> 16) & 0xFF) as u8,
        ((rgb >> 8) & 0xFF) as u8,
        (rgb & 0xFF) as u8,
    )
}

/// Style one page-view line as a sheet-coloured page on a black desktop: the
/// cells before the first non-blank one (the centering margin) are painted
/// black, and the page itself — from the left border to the end of the line —
/// is painted with `sheet`, defaulting the text to `ink` (coloured text keeps
/// its colour). Fully-blank lines (the gaps between pages) become all black.
fn paint_page(line: RLine<'static>, sheet: Color, ink: Color) -> RLine<'static> {
    let paint = |sp: RSpan<'static>| -> RSpan<'static> {
        let mut st = sp.style.bg(sheet);
        if st.fg.is_none() {
            st = st.fg(ink);
        }
        RSpan::styled(sp.content, st)
    };
    let mut in_page = false;
    let mut out: Vec<RSpan<'static>> = Vec::new();
    for span in line.spans {
        if in_page {
            out.push(paint(span));
            continue;
        }
        let text = span.content.into_owned();
        match text.find(|c: char| !c.is_whitespace()) {
            None => out.push(RSpan::styled(text, span.style.bg(Color::Black))),
            Some(i) => {
                if i > 0 {
                    out.push(RSpan::styled(
                        text[..i].to_string(),
                        span.style.bg(Color::Black),
                    ));
                }
                out.push(paint(RSpan::styled(text[i..].to_string(), span.style)));
                in_page = true;
            }
        }
    }
    RLine::from(out)
}

/// Style one page-view line as a white page on a black desktop — the light
/// terminal theme's sheet. See [`paint_page`].
fn paint_page_on_black(line: RLine<'static>) -> RLine<'static> {
    paint_page(line, Color::White, Color::Black)
}

fn doc_line_to_ratatui(line: &DocLine) -> RLine<'static> {
    let spans: Vec<RSpan<'static>> = line
        .spans
        .iter()
        .map(|s| {
            let st = &s.style;
            let mut style = Style::default();
            if st.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if st.italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if st.underline {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            if st.strike {
                style = style.add_modifier(Modifier::CROSSED_OUT);
            }
            if st.dim {
                style = style.add_modifier(Modifier::DIM);
            }
            if st.highlight {
                style = style.add_modifier(Modifier::REVERSED);
            }
            // Black/"automatic" text uses the terminal's default foreground — a
            // document's black text is invisible on a dark terminal otherwise.
            if let Some(c) = st.color {
                if c != DocColor::Black {
                    style = style.fg(map_color(c));
                }
            }
            RSpan::styled(s.text.clone(), style)
        })
        .collect();
    RLine::from(spans)
}

/// Give a body document a final `SectionProperties` when it has none (a New
/// document, or a package whose sectPr lives outside the body), so every undo
/// snapshot carries the final section it was taken with.
fn with_final_section(doc: &mut Document, pkg: &Package) {
    if doc.trailing_section_properties().is_none() {
        doc.set_trailing_section_properties(pkg.final_section());
    }
}

/// Header/footer state derived from a body document's final sectPr.
struct PageState {
    headers: PageParts,
    footers: PageParts,
    title_page: bool,
    header_part: Option<String>,
    footer_part: Option<String>,
    sect: String,
}

impl PageState {
    /// Resolve the final section's header/footer references through the
    /// package's current relationships (a header created since load has a new
    /// one) to parts, and parse their content with each part's own rels.
    fn derive(pkg: &Package, doc: &Document) -> PageState {
        let sect = doc
            .trailing_section_properties()
            .map(|section| section.raw.clone())
            .unwrap_or_default();
        let rels = pkg
            .part("word/_rels/document.xml.rels")
            .map(|b| parse_rels_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        let part = |kind: &str, wtype: &str| hf_part_name(&sect, &rels, kind, wtype);
        let blocks = |kind: &str, wtype: &str| {
            Rc::new(
                part(kind, wtype)
                    .and_then(|name| pkg.header_footer_blocks(&name))
                    .unwrap_or_default(),
            )
        };
        let parts = |kind: &str| PageParts {
            default: blocks(kind, "default"),
            first: blocks(kind, "first"),
            even: blocks(kind, "even"),
        };
        PageState {
            headers: parts("headerReference"),
            footers: parts("footerReference"),
            title_page: flag_on(&sect, "titlePg"),
            header_part: part("headerReference", "default"),
            footer_part: part("footerReference", "default"),
            sect,
        }
    }
}

/// Whether an on/off OOXML element (`<w:tag/>` / `<w:tag w:val="…"/>`) is present
/// and enabled.
fn flag_on(xml: &str, tag: &str) -> bool {
    let needle = format!("<w:{tag}");
    let Some(p) = xml.find(&needle) else {
        return false;
    };
    let end = xml[p..].find('>').map(|e| p + e).unwrap_or(xml.len());
    !matches!(
        docxcore::load::xml_attr_value(&xml[p..end], "w:val").as_deref(),
        Some("false" | "0" | "off")
    )
}

/// A section break's sectPr copied from `sect`, given an explicit US Letter
/// page size when `sect` names none (a New document's empty final section).
fn with_page_size(sect: &str) -> String {
    const LETTER: &str = "<w:pgSz w:w=\"12240\" w:h=\"15840\"/>";
    if sect.contains("<w:pgSz") {
        sect.to_string()
    } else if sect.contains("</w:sectPr>") {
        sect.replacen("</w:sectPr>", &format!("{LETTER}</w:sectPr>"), 1)
    } else {
        format!("<w:sectPr>{LETTER}</w:sectPr>")
    }
}

/// Rewrite a `<w:sectPr>` so its page size is landscape (w>h) or portrait (h>w),
/// setting `w:orient`. Other section properties (margins, header refs) are kept.
fn orient_sectpr(sect: &str, landscape: bool) -> String {
    let g = PageGeom::from_sect_pr(sect);
    let (w, h) = (g.w.max(1), g.h.max(1));
    let (nw, nh) = if landscape {
        (w.max(h), w.min(h))
    } else {
        (w.min(h), w.max(h))
    };
    let orient = if landscape { "landscape" } else { "portrait" };
    let pgsz = format!("<w:pgSz w:w=\"{nw}\" w:h=\"{nh}\" w:orient=\"{orient}\"/>");
    // Empty, or a self-closing `<w:sectPr/>`: nothing to splice into.
    if !sect.contains("</w:sectPr>") {
        return format!("<w:sectPr>{pgsz}</w:sectPr>");
    }
    if let Some(s) = sect.find("<w:pgSz") {
        if let Some(e) = sect[s..].find("/>").map(|x| s + x + 2) {
            return format!("{}{pgsz}{}", &sect[..s], &sect[e..]);
        }
    }
    sect.replacen("</w:sectPr>", &format!("{pgsz}</w:sectPr>"), 1)
}

/// The package part name of a section's header/footer of type `wtype`.
fn hf_part_name(sect: &str, rels: &Relationships, kind: &str, wtype: &str) -> Option<String> {
    let rid = docxcore::load::header_footer_ref_rid(sect, kind, wtype)?;
    let target = rels.target(&rid)?;
    Some(match target.strip_prefix('/') {
        Some(r) => r.to_string(),
        None => format!("word/{}", target.trim_start_matches("./")),
    })
}

/// Replace the inner content of a preserved header/footer part with serialized
/// blocks, keeping the original `<w:hdr …>` wrapper (and its namespaces).
/// `None` when the part has no such wrapper.
fn splice_hf(original: &str, blocks: &[Block], tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let os = original.find(&open)?;
    let ce = original.find(&close)?;
    let inner_start = original[os..].find('>').map(|e| os + e + 1)?;
    if inner_start > ce {
        return None;
    }
    let mut out = String::with_capacity(original.len() + 64);
    out.push_str(&original[..inner_start]);
    out.push_str(&blocks_to_xml(blocks));
    out.push_str(&original[ce..]);
    Some(out)
}

/// Dispatch one terminal event. Returns true if the app should quit.
fn handle_event(app: &mut App, ev: Event) -> bool {
    match ev {
        Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k),
        Event::Mouse(m) => {
            app.on_mouse(m);
            app.quit_requested // a clicked File ▸ Exit quits
        }
        Event::Resize(_, _) => {
            app.dirty = true;
            false
        }
        _ => false,
    }
}

fn open_notice(bundle_warning: Option<&str>, encoding: Option<&str>) -> Option<String> {
    let encoding_notice = encoding.map(|name| format!("{name} Markdown; saves as UTF-8"));
    match (bundle_warning, encoding_notice) {
        (Some(warning), Some(encoding)) => Some(format!("{warning}; {encoding}")),
        (Some(warning), None) => Some(warning.to_string()),
        (None, Some(encoding)) => Some(encoding),
        (None, None) => None,
    }
}

fn startup_app(
    pkg: Package,
    path: &str,
    format: DocFormat,
    bundle: Option<html::Opened>,
    encoding: Option<&str>,
    vim: bool,
) -> App {
    let mut app = App::new(pkg, path, vim);
    app.format = format;
    app.status = open_notice(bundle.as_ref().and_then(|b| b.warning.as_deref()), encoding);
    app.bundle_html = bundle.map(|b| b.html);
    app
}

fn run_tui(
    pkg: Package,
    path: &str,
    format: DocFormat,
    bundle: Option<html::Opened>,
    encoding: Option<&str>,
    vim: bool,
    start: bool,
) -> io::Result<()> {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        default_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = startup_app(pkg, path, format, bundle, encoding, vim);
    // Restore persisted view-mode toggles and enable saving them going forward.
    let prefs = ViewPrefs::load();
    // Page view is a `.docx`-only concept; never restore it for Markdown.
    app.page_view = prefs.page_view && app.format != DocFormat::Markdown;
    app.invisibles = prefs.invisibles;
    app.borderless = prefs.borderless;
    app.light_page = prefs.light_page;
    app.show_ruler = prefs.show_ruler;
    app.show_nav = prefs.show_nav;
    app.show_comments = prefs.show_comments;
    app.show_notes = prefs.show_notes;
    app.auto_hide_ribbon = prefs.auto_hide_ribbon;
    // With auto-hide off the ribbon is pinned, so start it expanded (focus stays
    // in the document); with auto-hide on it starts collapsed to the tab strip.
    app.ribbon_open = !app.auto_hide_ribbon;
    app.start_screen = start;
    app.persist_prefs = true;
    // Detect the terminal's graphics capability (kitty/iTerm2/Sixel); fall back
    // to a half-block renderer if the query fails (e.g. a plain console).
    app.picker =
        Some(Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((8, 16))));
    let mut last_title = String::new();

    // Bring up the agent control surface. Best-effort: if the config directory or
    // the loopback bind fails, the editor runs exactly as before, just without a
    // control channel. `ctl_server` is held for the whole session — its Drop
    // removes the discovery file.
    let ctl_instance = control::instance_name();
    let (ctl_server, ctl_rx) = match control::control_dir() {
        Some(dir) => match ctlcore::serve(&dir, &ctl_instance) {
            Ok((srv, rx)) => (Some(srv), Some(rx)),
            Err(_) => (None, None),
        },
        None => (None, None),
    };

    // One message stream drives the loop: terminal input (read on its own thread
    // so the loop can block cheaply) and control requests. The main thread stays
    // the sole owner of the document, so applying a request needs no locking.
    enum Msg {
        Term(Event),
        Ctl(ctlcore::Request),
    }
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    {
        let tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("docxy-input".into())
            .spawn(move || {
                while let Ok(ev) = event::read() {
                    if tx.send(Msg::Term(ev)).is_err() {
                        break;
                    }
                }
            });
    }
    if let Some(ctl_rx) = ctl_rx {
        let tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("docxy-ctl".into())
            .spawn(move || {
                for req in ctl_rx {
                    if tx.send(Msg::Ctl(req)).is_err() {
                        break;
                    }
                }
            });
    }
    drop(tx); // only the reader/forwarder threads keep the channel open now

    let result = loop {
        // Reflect the file + unsaved state in the terminal window title.
        let title = window_title("docxy", &app.path, app.modified);
        if title != last_title {
            let _ = execute!(io::stdout(), SetTitle(&title));
            last_title = title;
        }
        if let Err(e) = terminal.draw(|f| app.draw(f)) {
            break Err(e);
        }

        // Block until something arrives (no busy polling), then drain anything
        // already queued so a burst — fast scrolling, or a run of agent edits —
        // collapses into a single repaint.
        let mut next = match rx.recv() {
            Ok(m) => Some(m),
            Err(_) => break Ok(()), // every input source is gone
        };
        let mut quit = false;
        while let Some(msg) = next.take() {
            match msg {
                Msg::Term(ev) => {
                    if handle_event(&mut app, ev) {
                        quit = true;
                    }
                }
                Msg::Ctl(req) => match control::dispatch(&mut app, &req.verb, &req.args) {
                    Ok(result) => req.reply_ok(result),
                    Err(e) => req.reply_err(e),
                },
            }
            if quit {
                break;
            }
            next = rx.try_recv().ok();
        }
        if quit {
            break Ok(());
        }
    };
    drop(ctl_server); // remove the discovery file

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxcore::model::{Cell, Row, Table, VMerge};

    fn markdown_temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("docxy-markdown-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn markdown_file(tag: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = markdown_temp(tag).join("input.md");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn remove_markdown_file(path: &std::path::Path) {
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn markdown_load_refuses_undecodable_bytes_without_changing_file() {
        for (tag, bytes, expected) in [
            ("cp1252", b"caf\xE9\n".as_slice(), "not UTF-8 text"),
            (
                "utf16le-no-bom",
                b"#\0 X\0".as_slice(),
                "contains NUL bytes",
            ),
            (
                "utf16be-no-bom",
                b"\0#\0 X".as_slice(),
                "contains NUL bytes",
            ),
            (
                "utf32le",
                b"\xFF\xFE\0\0".as_slice(),
                "contains NUL characters",
            ),
            ("utf16-odd", b"\xFF\xFEA".as_slice(), "invalid UTF-16 text"),
            (
                "utf16-surrogate",
                b"\xFE\xFF\xD8\x00".as_slice(),
                "invalid UTF-16 text",
            ),
        ] {
            let path = markdown_file(tag, bytes);
            let error = load_input(path.to_str().unwrap()).err().unwrap();
            assert!(error.contains(path.to_str().unwrap()), "{error}");
            assert!(error.contains(expected), "{tag}: {error}");
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            remove_markdown_file(&path);
        }
    }

    #[test]
    fn markdown_load_parses_bom_marked_text_without_bom_character() {
        let text = "# Title\n\ncafé\n";
        let mut variants = vec![(
            String::from("utf8"),
            {
                let mut bytes = vec![0xEF, 0xBB, 0xBF];
                bytes.extend_from_slice(text.as_bytes());
                bytes
            },
            None,
        )];
        for (tag, bom, little_endian) in [
            ("utf16le", [0xFF, 0xFE], true),
            ("utf16be", [0xFE, 0xFF], false),
        ] {
            let mut bytes = bom.to_vec();
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&if little_endian {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            variants.push((tag.into(), bytes, Some("UTF-16")));
        }
        for (tag, bytes, encoding) in variants {
            let path = markdown_file(&tag, &bytes);
            let input = load_input(path.to_str().unwrap()).unwrap();
            assert_eq!(input.encoding, encoding);
            let blocks = &input.pkg.document.body;
            assert!(
                matches!(&blocks[0], Block::Paragraph(p) if p.props.heading_level == Some(1) && p.plain_text() == "Title"),
                "{tag}"
            );
            assert!(
                matches!(&blocks[1], Block::Paragraph(p) if p.plain_text() == "café"),
                "{tag}"
            );
            assert!(
                !input.pkg.document.plain_text().contains('\u{feff}'),
                "{tag}"
            );
            remove_markdown_file(&path);
        }
    }

    #[test]
    fn markdown_open_failure_preserves_current_document() {
        let path = markdown_file("open-error", b"caf\xE9\n");
        let mut app = app_with(&["keep this document"]);
        let prior_path = app.path.clone();
        let prior_text = app.editor.doc.plain_text();
        app.open_path(&path).unwrap_err();
        assert_eq!(app.path, prior_path);
        assert_eq!(app.editor.doc.plain_text(), prior_text);
        let expected = format!(
            "cannot open {}: {}: not UTF-8 text",
            path.display(),
            path.display()
        );
        assert!(app.status.as_deref().unwrap().starts_with(&expected));
        assert_eq!(std::fs::read(&path).unwrap(), b"caf\xE9\n");
        remove_markdown_file(&path);
    }

    #[test]
    fn markdown_utf16_notice_appears_on_both_open_paths() {
        let text = "# Title\n";
        let mut bytes = vec![0xFF, 0xFE];
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let path = markdown_file("notice", &bytes);
        let input = load_input(path.to_str().unwrap()).unwrap();
        let app = startup_app(
            input.pkg,
            path.to_str().unwrap(),
            input.format,
            input.bundle,
            input.encoding,
            false,
        );
        assert_eq!(
            app.status.as_deref(),
            Some("UTF-16 Markdown; saves as UTF-8")
        );
        let mut app = app_with(&["before"]);
        app.open_path(&path).unwrap();
        assert_eq!(
            app.status.as_deref(),
            Some(
                format!(
                    "opened {} — UTF-16 Markdown; saves as UTF-8",
                    path.display()
                )
                .as_str()
            )
        );
        remove_markdown_file(&path);

        let path = markdown_file("notice-utf8", b"# Title\n");
        let input = load_input(path.to_str().unwrap()).unwrap();
        let app = startup_app(
            input.pkg,
            path.to_str().unwrap(),
            input.format,
            input.bundle,
            input.encoding,
            false,
        );
        assert!(app.status.is_none());
        let mut app = app_with(&["before"]);
        app.open_path(&path).unwrap();
        assert_eq!(
            app.status.as_deref(),
            Some(format!("opened {}", path.display()).as_str())
        );
        remove_markdown_file(&path);
    }

    #[test]
    fn exports_refuse_source_aliases() {
        let dir = std::env::temp_dir().join(format!("docxy-export-alias-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("note.docx");
        let mut app = app_with(&["keep this document"]);
        let pkg = new_package(app.editor.doc.clone());
        let original = save_package(&pkg);
        std::fs::write(&source, &original).unwrap();
        let alias = dir.join("./note.docx");
        for kind in [
            HeadlessFormat::Pdf,
            HeadlessFormat::Markdown,
            HeadlessFormat::Docx,
        ] {
            assert!(
                convert_headless(
                    &pkg,
                    source.to_str().unwrap(),
                    alias.to_str().unwrap(),
                    kind
                )
                .is_err()
            );
            assert_eq!(std::fs::read(&source).unwrap(), original);
        }
        let pdf = source.with_extension("pdf");
        std::fs::hard_link(&source, &pdf).unwrap();
        app.path = source.to_str().unwrap().into();
        app.export_pdf();
        assert!(app.confirm.is_some());
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.status.as_deref().unwrap().contains("cannot overwrite"));
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(std::fs::read(&pdf).unwrap(), original);
        std::fs::remove_file(pdf).unwrap();
        // The same source remains a valid save destination.
        app.save();
        assert!(load_package(&std::fs::read(&source).unwrap()).is_ok());
        std::fs::remove_file(source).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    use docxcore::model::{
        Block, Document, Hyperlink, Inline, ParProps, Paragraph as MPara, RevisionMetadata, Run,
        RunProps,
    };
    use docxcore::package::{
        HeaderVariant, ProtectionEditMode, ProtectionEnforcement, Watermark, WatermarkHeader,
        WatermarkKind, new_package,
    };
    use ratatui::backend::TestBackend;

    #[test]
    fn window_title_format() {
        assert_eq!(
            window_title("docxy", "/tmp/notes.docx", false),
            "docxy - notes.docx"
        );
        assert_eq!(
            window_title("docxy", "/tmp/notes.docx", true),
            "* docxy - notes.docx"
        );
    }

    #[test]
    fn page_on_black_paints_margin_black_and_page_white() {
        // "   │ hi │" → centering margin black, page region white.
        let line = RLine::from(vec![RSpan::raw("   "), RSpan::raw("│ hi │")]);
        let out = paint_page_on_black(line);
        assert_eq!(
            out.spans[0].style.bg,
            Some(Color::Black),
            "margin not black"
        );
        assert_eq!(
            out.spans.last().unwrap().style.bg,
            Some(Color::White),
            "page not white"
        );
    }

    #[test]
    fn page_on_black_blank_line_is_all_black() {
        // The gap between pages (all whitespace) is fully black.
        let out = paint_page_on_black(RLine::from(vec![RSpan::raw("        ")]));
        assert!(out.spans.iter().all(|s| s.style.bg == Some(Color::Black)));
    }

    #[test]
    fn page_ink_picks_a_side_by_luminance() {
        assert_eq!(page_ink(0xFFFFFF), (0x202020, 0x808080));
        assert_eq!(page_ink(0x000000), (0xF2F2F2, 0xB0B0B0));
        assert_eq!(page_ink(0xFF0000).0, 0xF2F2F2);
        assert_eq!(page_ink(0xFFFF00).0, 0x202020);
    }

    #[test]
    fn page_color_paints_the_print_layout_sheet() {
        let mut app = app_with(&["body"]);
        assert!(app.set_page_color(Some(0xFF0000)));
        app.page_view = true; // light_page stays false: the dark terminal theme
        app.dirty = true;
        let mut term = Terminal::new(TestBackend::new(100, 70)).unwrap();
        term.draw(|frame| app.draw(frame)).unwrap();
        let buf = term.backend().buffer();
        let at = buf
            .content
            .iter()
            .position(|c| c.symbol() == "b")
            .expect("the page shows the body text");
        let cell = &buf.content[at];
        assert_eq!(cell.bg, Color::Rgb(255, 0, 0), "the sheet is red");
        assert_eq!(
            cell.fg,
            Color::Rgb(0xF2, 0xF2, 0xF2),
            "dark sheet, light ink"
        );
        let (_, row) = buf.pos_of(at);
        assert!(
            row > 0,
            "the body sits inside the page frame, not on the first row"
        );
        assert_eq!(
            buf.cell((0, row)).unwrap().bg,
            Color::Black,
            "the centering margin stays black"
        );
    }

    #[test]
    fn page_color_light_sheet_uses_dark_ink() {
        let mut app = app_with(&["body"]);
        assert!(app.set_page_color(Some(0xFFFF00)));
        app.page_view = true;
        app.light_page = true;
        app.dirty = true;
        let mut term = Terminal::new(TestBackend::new(100, 70)).unwrap();
        term.draw(|frame| app.draw(frame)).unwrap();
        let buf = term.backend().buffer();
        let cell = buf
            .content
            .iter()
            .find(|c| c.symbol() == "b")
            .expect("the page shows the body text");
        assert_eq!(cell.bg, Color::Rgb(255, 255, 0), "the sheet is yellow");
        assert_eq!(
            cell.fg,
            Color::Rgb(0x20, 0x20, 0x20),
            "light sheet, dark ink"
        );
    }

    #[test]
    fn page_color_not_painted_outside_print_layout() {
        let mut app = app_with(&["body"]);
        assert!(app.set_page_color(Some(0xFF0000)));
        app.page_view = false; // Read Mode: continuous, no page frame
        app.dirty = true;
        let mut term = Terminal::new(TestBackend::new(100, 70)).unwrap();
        term.draw(|frame| app.draw(frame)).unwrap();
        assert!(
            term.backend()
                .buffer()
                .content
                .iter()
                .all(|c| c.bg != Color::Rgb(255, 0, 0)),
            "read mode must not paint the page colour"
        );
    }

    #[test]
    fn picker_and_page_color_method_write_the_same_document_xml() {
        // The control verb and the ribbon picker share set_page_color, so both
        // paths must produce the same package parts.
        let mut picked = app_with(&["body"]);
        picked.run_act(ribbon::Act::PageColor);
        pick(&mut picked, PickerKind::PageColor, "Red");
        let mut direct = app_with(&["body"]);
        assert!(direct.set_page_color(Some(0xFF0000)));
        for part in ["word/document.xml", "word/settings.xml"] {
            assert_eq!(
                picked.pkg.part_text(part),
                direct.pkg.part_text(part),
                "{part} must not depend on how the colour was set"
            );
        }
    }

    #[test]
    fn view_prefs_round_trip() {
        let p = ViewPrefs {
            page_view: true,
            invisibles: false,
            borderless: true,
            light_page: true,
            show_ruler: false,
            show_nav: true,
            show_comments: true,
            show_notes: true,
            auto_hide_ribbon: true,
        };
        let back = ViewPrefs::parse(&p.to_conf());
        assert_eq!(back.page_view, p.page_view);
        assert_eq!(back.invisibles, p.invisibles);
        assert_eq!(back.borderless, p.borderless);
        assert_eq!(back.light_page, p.light_page);
        assert_eq!(back.show_ruler, p.show_ruler);
        assert_eq!(back.show_nav, p.show_nav);
        assert_eq!(back.show_comments, p.show_comments);
        assert_eq!(back.show_notes, p.show_notes);
        assert_eq!(back.auto_hide_ribbon, p.auto_hide_ribbon);
        // Unknown/blank lines are ignored; missing keys default off.
        let partial = ViewPrefs::parse("invisibles=1\nbogus=1\n");
        assert!(partial.invisibles && !partial.page_view && !partial.borderless);
    }

    fn bidi_opts(width: usize) -> RenderOptions {
        RenderOptions {
            width,
            bidi: Some(bidi::projector()),
            ..RenderOptions::default()
        }
    }

    fn bidi_run(text: &str) -> Inline {
        Inline::Run(Run {
            text: text.to_string(),
            props: RunProps::default(),
        })
    }

    fn bidi_run_with(text: &str, props: RunProps) -> Inline {
        Inline::Run(Run {
            text: text.to_string(),
            props,
        })
    }

    fn bidi_para(text: &str) -> Block {
        Block::Paragraph(MPara {
            props: ParProps::default(),
            content: vec![bidi_run(text)],
        })
    }

    fn bidi_para_with(props: ParProps, content: Vec<Inline>) -> Block {
        Block::Paragraph(MPara { props, content })
    }

    fn bidi_doc(blocks: Vec<Block>) -> Document {
        Document { body: blocks }
    }

    #[test]
    fn bidi_renderer_wraps_body_lines_in_visual_order() {
        let doc = bidi_doc(vec![bidi_para("abc אבג def")]);
        let (lines, maps) = docxcore::render::render_mapped(&doc, &bidi_opts(7));
        let plain: Vec<String> = lines.iter().map(|line| line.plain()).collect();

        assert_eq!(plain, vec!["abc גבא", "def"]);
        assert_eq!(maps[0].segs[0].start, 0);
        assert_eq!(maps[1].segs[0].start, 8);
        assert_eq!(maps[0].segs[0].col_for_offset(0), Some(0));
    }

    #[test]
    fn bidi_renderer_keeps_alignment_separate_from_reordering() {
        let mut right = ParProps::default();
        right.align = Align::Right;
        let right_doc = bidi_doc(vec![bidi_para_with(right, vec![bidi_run("abc אבג")])]);
        let right_line = docxcore::render::render(&right_doc, &bidi_opts(12))[0].plain();
        assert_eq!(right_line, "     abc גבא");

        let mut rtl = ParProps::default();
        rtl.rtl = true;
        let rtl_doc = bidi_doc(vec![bidi_para_with(rtl, vec![bidi_run("שלום")])]);
        let rtl_line = docxcore::render::render(&rtl_doc, &bidi_opts(8))[0].plain();
        assert_eq!(rtl_line, "    םולש");
    }

    #[test]
    fn bidi_renderer_projects_table_cells_and_maps() {
        let mut rtl = ParProps::default();
        rtl.rtl = true;
        let cell = |block: Block| Cell {
            grid_span: 1,
            v_merge: VMerge::None,
            blocks: vec![block],
            ..Cell::default()
        };
        let table = Table {
            grid: vec![100, 100],
            rows: vec![Row {
                cells: vec![
                    cell(bidi_para("abc אבג")),
                    cell(bidi_para_with(rtl, vec![bidi_run("שלום")])),
                ],
                ..Row::default()
            }],
            ..Table::default()
        };
        let doc = bidi_doc(vec![Block::Table(table)]);
        let (lines, maps) = docxcore::render::render_mapped(&doc, &bidi_opts(30));
        let joined = lines
            .iter()
            .map(|line| line.plain())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(joined.contains("abc גבא"), "{joined}");
        assert!(joined.contains("םולש"), "{joined}");
        assert!(maps.iter().any(|map| {
            map.segs
                .iter()
                .any(|seg| seg.path == vec![0, 0, 0, 0] && seg.col_for_offset(0).is_some())
        }));
        assert!(maps.iter().any(|map| {
            map.segs
                .iter()
                .any(|seg| seg.path == vec![0, 0, 1, 0] && seg.col_for_offset(0).is_some())
        }));
    }

    #[test]
    fn bidi_renderer_projects_page_header_footer_and_body() {
        let mut rtl = ParProps::default();
        rtl.rtl = true;
        let opts = RenderOptions {
            width: 50,
            page_view: true,
            headers: PageParts {
                default: Rc::new(vec![bidi_para_with(rtl.clone(), vec![bidi_run("שלום")])]),
                ..PageParts::default()
            },
            footers: PageParts {
                default: Rc::new(vec![bidi_para_with(rtl, vec![bidi_run("אבג")])]),
                ..PageParts::default()
            },
            bidi: Some(bidi::projector()),
            ..RenderOptions::default()
        };
        let doc = bidi_doc(vec![bidi_para("body אבג")]);
        let rendered = render_with_page_layout(&doc, &opts);
        let joined = rendered
            .lines
            .iter()
            .map(|line| line.plain())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(joined.contains("םולש"), "{joined}");
        assert!(joined.contains("גבא"), "{joined}");
        assert!(joined.contains("body גבא"), "{joined}");
    }

    #[test]
    fn bidi_renderer_preserves_link_field_and_revision_spans() {
        let link = Inline::Hyperlink(Hyperlink {
            target: Some("https://x.test/".to_string()),
            runs: vec![Run {
                text: "go אבג".to_string(),
                props: RunProps::default(),
            }],
            ..Hyperlink::default()
        });
        let field = Inline::Field {
            raw: "<w:fldSimple/>".to_string(),
            text: "REF אבג".to_string(),
        };
        let strike = RunProps {
            strike: true,
            ..RunProps::default()
        };
        let revision = Inline::Revision {
            kind: RevisionKind::Delete,
            metadata: RevisionMetadata::default(),
            raw: "<w:del/>".to_string(),
            content: vec![bidi_run_with("old אב", strike)],
            content_changed: false,
        };
        let doc = bidi_doc(vec![
            bidi_para_with(ParProps::default(), vec![link]),
            bidi_para_with(ParProps::default(), vec![field]),
            bidi_para_with(ParProps::default(), vec![revision]),
        ]);
        let lines = docxcore::render::render(&doc, &bidi_opts(30));
        let plain: Vec<String> = lines.iter().map(|line| line.plain()).collect();

        assert_eq!(plain[0], "go גבא");
        assert_eq!(plain[1], "REF גבא");
        assert_eq!(plain[2], "old בא");
        assert!(
            lines[0]
                .spans
                .iter()
                .filter(|span| !span.text.trim().is_empty())
                .all(|span| span.link.as_deref() == Some("https://x.test/"))
        );
        assert!(lines[2].spans.iter().any(|span| span.style.strike));
    }

    #[test]
    fn bidi_renderer_puts_caret_stops_only_around_a_whole_field_642() {
        let field = |text: &str| Inline::Field {
            raw: "<w:fldSimple w:instr=\" PAGE \"/>".to_string(),
            text: text.to_string(),
        };
        let doc = bidi_doc(vec![
            bidi_para_with(
                ParProps::default(),
                vec![bidi_run("Body"), field("Page 1"), bidi_run("x")],
            ),
            bidi_para_with(
                ParProps::default(),
                vec![bidi_run("אבג "), field("12"), bidi_run(" דה")],
            ),
        ]);
        let (lines, maps) = docxcore::render::render_mapped(&doc, &bidi_opts(30));
        assert_eq!(lines[0].plain(), "BodyPage 1x");
        let stops: Vec<(usize, usize)> = maps[0]
            .visual_positions()
            .into_iter()
            .map(|caret| (caret.offset, caret.col))
            .collect();
        assert_eq!(
            stops,
            vec![(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 10), (6, 11)]
        );
        // In right-to-left text the field keeps two stops, one per edge of its
        // whole result, and none inside it.
        let stops: Vec<usize> = maps[1]
            .visual_positions()
            .into_iter()
            .map(|caret| caret.offset)
            .collect();
        let line = lines[1].plain();
        let at = line.find("12").expect("the field's result is drawn");
        let field_cols: Vec<usize> = maps[1]
            .visual_positions()
            .into_iter()
            .filter(|caret| caret.offset == 4 || caret.offset == 5)
            .map(|caret| caret.col)
            .collect();
        assert!(stops.contains(&4) && stops.contains(&5), "{stops:?}");
        let at = line[..at].chars().count();
        assert!(
            field_cols.iter().all(|&col| col <= at || col >= at + 2),
            "no stop inside the field: {field_cols:?} around {at} in {line:?}"
        );
    }

    fn rendered_text(app: &mut App, width: u16) -> String {
        app.ensure_rendered(width);
        app.lines
            .iter()
            .map(|line| line.plain())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn bidi_docx_fixtures_render_visually_and_copy_logically() {
        use crate::test_fixtures::BidiFixture;

        let cases: &[(BidiFixture, &[&str])] = &[
            (BidiFixture::Hebrew, &["םולש"]),
            (BidiFixture::Arabic, &["ابحرم", "123"]),
            (
                BidiFixture::MixedLatinNumbersNeutrals,
                &["abc 123 גבא, def?"],
            ),
            (BidiFixture::ExplicitRunOverride, &["בא", "cba"]),
            (BidiFixture::StyleDerived, &["םולש", "בא", "12"]),
            (BidiFixture::List, &["טירפ", "123"]),
            (BidiFixture::Table, &["cell 45 גבא", "םולש"]),
            (BidiFixture::Header, &["body גבא"]),
            (BidiFixture::TrackedRevisions, &["שדח", "ןשי", "123", "45"]),
        ];

        for (fixture, visual_fragments) in cases {
            let mut app = App::new(fixture.package(), fixture.name(), false);
            app.os_clip = None;
            let visual = rendered_text(&mut app, 80);
            for fragment in *visual_fragments {
                assert!(
                    visual.contains(fragment),
                    "{} rendered without expected visual fragment {fragment:?}: {visual}",
                    fixture.name()
                );
            }
            assert!(!visual.contains('\u{202e}'), "RLO leaked into visual text");
            assert!(!visual.contains('\u{202c}'), "PDF leaked into visual text");

            let plain_export = app.editor.doc.plain_text();
            for token in fixture.logical_text().split_whitespace() {
                assert!(
                    plain_export.contains(token),
                    "{} text export lost logical token {token:?}: {plain_export:?}",
                    fixture.name()
                );
            }

            app.editor.select_all();
            app.do_copy();
            let copied = app.clip_text.as_deref().unwrap_or("");
            if *fixture != BidiFixture::TrackedRevisions {
                for token in fixture.logical_text().split_whitespace() {
                    assert!(
                        copied.contains(token),
                        "{} copy lost logical token {token:?}: {copied:?}",
                        fixture.name()
                    );
                }
            }
        }
    }

    #[test]
    fn bidi_docx_fixtures_save_reload_with_same_visual_output() {
        use crate::test_fixtures::BidiFixture;

        for fixture in BidiFixture::ALL {
            let mut original = App::new(fixture.package(), fixture.name(), false);
            original.os_clip = None;
            let before = rendered_text(&mut original, 80);
            let saved = save_package(&original.pkg);
            let package = load_package(&saved)
                .unwrap_or_else(|error| panic!("reload {}: {error:?}", fixture.name()));
            let mut reloaded = App::new(package, fixture.name(), false);
            reloaded.os_clip = None;
            let after = rendered_text(&mut reloaded, 80);

            assert_eq!(after, before, "{}", fixture.name());
        }
    }

    #[test]
    fn bidi_fixture_render_evidence_captures_page_and_non_page_views() {
        use crate::test_fixtures::BidiFixture;

        let artifact_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-artifacts/docxy");
        std::fs::create_dir_all(&artifact_dir).expect("create bidi artifact directory");

        let mut non_page = App::new(
            BidiFixture::MixedLatinNumbersNeutrals.package(),
            "bidi-mixed.docx",
            false,
        );
        non_page.os_clip = None;
        let non_page_capture = rendered_text(&mut non_page, 80);
        assert!(non_page_capture.contains("abc 123 גבא, def?"));
        let non_page_artifact = artifact_dir.join("bidi-non-page-view.txt");
        std::fs::write(&non_page_artifact, &non_page_capture)
            .expect("write bidi non-page view capture");
        eprintln!("wrote {}", non_page_artifact.display());

        let mut page = App::new(BidiFixture::Header.package(), "bidi-header.docx", false);
        page.os_clip = None;
        page.page_view = true;
        page.light_page = true;
        page.dirty = true;
        let mut terminal = Terminal::new(TestBackend::new(100, 70)).unwrap();
        terminal.draw(|frame| page.draw(frame)).unwrap();
        let page_capture = format!("{:?}", terminal.backend().buffer());
        assert!(page_capture.contains("body גבא"), "{page_capture}");
        assert!(page_capture.contains("תרתוכ"), "{page_capture}");
        let page_artifact = artifact_dir.join("bidi-page-view.txt");
        std::fs::write(&page_artifact, &page_capture).expect("write bidi page-view capture");
        eprintln!("wrote {}", page_artifact.display());
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }
    fn alt_shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT | KeyModifiers::SHIFT)
    }

    #[test]
    fn format_for_picks_markdown_by_extension() {
        assert_eq!(format_for("notes.md"), DocFormat::Markdown);
        assert_eq!(format_for("README.MARKDOWN"), DocFormat::Markdown);
        assert_eq!(format_for("report.docx"), DocFormat::Docx);
        assert_eq!(format_for("plain.txt"), DocFormat::Docx);
    }

    #[test]
    fn markdown_file_opens_rendered_and_round_trips() {
        let src = "# Title\n\nSome **bold** text.\n\n- a\n- b";
        let app = App::new(new_markdown_package(from_markdown(src)), "doc.md", false);
        assert_eq!(app.format, DocFormat::Markdown);
        assert!(!app.md_source, "opens in rendered view");
        // The first block is a level-1 heading.
        match &app.editor.doc.body[0] {
            Block::Paragraph(p) => assert_eq!(p.props.heading_level, Some(1)),
            _ => panic!("expected heading paragraph"),
        }
        // Saving regenerates Markdown (heading, bold, bullets all present).
        let md = app.current_markdown();
        assert!(md.contains("# Title"), "{md}");
        assert!(md.contains("**bold**"), "{md}");
        assert!(md.contains("- a"), "{md}");
    }

    #[test]
    fn markdown_source_toggle_converts_both_ways() {
        let mut app = App::new(
            new_markdown_package(from_markdown("# Hi\n\nbody")),
            "x.md",
            false,
        );
        // Switch to source view: the buffer becomes literal Markdown lines.
        app.set_md_source(true);
        assert!(app.md_source);
        assert!(app.source_text().contains("# Hi"));
        // current_document() re-parses the source back to a heading.
        match &app.current_document().body[0] {
            Block::Paragraph(p) => assert_eq!(p.props.heading_level, Some(1)),
            _ => panic!("expected heading"),
        }
        // Back to rendered: the editor holds the parsed tree again.
        app.set_md_source(false);
        assert!(!app.md_source);
        match &app.editor.doc.body[0] {
            Block::Paragraph(p) => assert_eq!(p.props.heading_level, Some(1)),
            _ => panic!("expected heading"),
        }
    }

    #[test]
    fn start_screen_actions() {
        // New Word document.
        let mut app = app_with(&["x"]);
        app.start_screen = true;
        assert!(!app.start_choose(0));
        assert!(!app.start_screen);
        assert_eq!(app.format, DocFormat::Docx);
        assert!(app.path.ends_with(".docx"));

        // New Markdown document.
        let mut app = app_with(&["x"]);
        app.start_screen = true;
        assert!(!app.start_choose(1));
        assert_eq!(app.format, DocFormat::Markdown);
        assert!(app.path.ends_with(".md"));

        // Open → drops into the File backstage.
        let mut app = app_with(&["x"]);
        app.start_screen = true;
        assert!(!app.start_choose(2));
        assert!(app.backstage.is_some());

        // Quit.
        let mut app = app_with(&["x"]);
        app.start_screen = true;
        assert!(app.start_choose(3), "Quit returns true");
    }

    #[test]
    fn start_screen_navigation_wraps_and_digits_pick() {
        // backstagecore::Start wraps at the ends: Up on the first item lands on
        // the last (there are 4 items: indices 0..=3).
        let mut app = app_with(&["x"]);
        app.start_screen = true;
        app.on_key(KeyEvent::from(KeyCode::Up)); // first → wraps to last
        assert_eq!(app.start.sel(), 3);
        app.on_key(KeyEvent::from(KeyCode::Down)); // last → wraps to first
        assert_eq!(app.start.sel(), 0);
        app.on_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(app.start.sel(), 1);
        // A digit selects and activates: '2' → New Markdown.
        assert!(!app.on_key(KeyEvent::from(KeyCode::Char('2'))));
        assert_eq!(app.format, DocFormat::Markdown);
        assert!(!app.start_screen);
    }

    #[test]
    fn start_screen_mouse_hovers_and_clicks() {
        let mut app = app_with(&["x"]);
        app.start_screen = true;
        // Draw once so `backstagecore::Start` records the real click rects for
        // an 80x24 frame: item rows sit at inner.y + i, inner.y = 9 here.
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let base_y = 9u16;
        // Hovering over the Markdown row (index 1) highlights it without activating.
        app.on_mouse(mouse(MouseEventKind::Moved, 20, base_y + 1));
        assert_eq!(app.start.sel(), 1);
        assert!(app.start_screen, "hover must not leave the welcome screen");
        // Clicking the Open row (index 2) activates it → File backstage.
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            20,
            base_y + 2,
        ));
        assert!(!app.start_screen);
        assert!(app.backstage.is_some());

        // Clicking Quit (index 3) sets the quit flag.
        let mut app = app_with(&["x"]);
        app.start_screen = true;
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            20,
            base_y + 3,
        ));
        assert!(app.quit_requested, "clicking Quit requests shutdown");
    }

    #[test]
    fn page_view_is_unavailable_for_markdown() {
        let body = vec![Block::Paragraph(docxcore::model::Paragraph::default())];
        let mut app = App::new(new_markdown_package(Document { body }), "a.md", false);
        // Requesting page view is ignored for Markdown.
        app.set_page_view(true);
        assert!(!app.page_view, "Markdown must not enter page view");
        // Even a doc that was in page view drops it when a .md is loaded.
        app.page_view = true;
        let body = vec![Block::Paragraph(docxcore::model::Paragraph::default())];
        app.load_package_state(new_package(Document { body }), "x.md".to_string());
        assert!(!app.page_view);
    }

    #[test]
    fn page_layout_reports_multi_page_section_scope() {
        let first = MPara {
            props: ParProps {
                section_break: Some("<w:sectPr/>".to_string()),
                ..ParProps::default()
            },
            content: vec![
                Inline::Run(Run {
                    text: "first page".to_string(),
                    props: RunProps::default(),
                }),
                Inline::Break(BreakKind::Page, RunProps::default()),
                Inline::Run(Run {
                    text: "second page".to_string(),
                    props: RunProps::default(),
                }),
            ],
        };
        let second = MPara {
            props: ParProps::default(),
            content: vec![Inline::Run(Run {
                text: "second section".to_string(),
                props: RunProps::default(),
            })],
        };
        let doc = Document {
            body: vec![Block::Paragraph(first), Block::Paragraph(second)],
        };
        let opts = RenderOptions {
            width: 80,
            page_view: true,
            ..RenderOptions::default()
        };

        let rendered = render_with_page_layout(&doc, &opts);
        let (lines, maps, pages) = (rendered.lines, rendered.maps, rendered.pages);

        assert_eq!(lines.len(), maps.len());
        assert_eq!(pages.len(), 3, "expected two pages then one: {pages:?}");
        assert_eq!(
            pages
                .iter()
                .map(|page| (
                    page.section_index,
                    page.section_page_index,
                    page.document_page_index
                ))
                .collect::<Vec<_>>(),
            vec![(0, 0, 0), (0, 1, 1), (1, 0, 2)]
        );
        assert!(pages.iter().all(|page| {
            page.rows >= 3 && page.cols >= 3 && page.row + page.rows <= lines.len()
        }));
    }

    #[test]
    fn page_watermark_is_visible_but_absent_from_document_and_interaction_state() {
        let mut app = app_with(&["body text"]);
        app.watermark_state = watermark::State::new(
            vec![Watermark {
                kind: WatermarkKind::Text("機密 SECRET".to_string()),
                header: WatermarkHeader {
                    section_index: 0,
                    variant: HeaderVariant::Default,
                    relationship_id: "rIdWatermark".to_string(),
                    part_name: "word/header1.xml".to_string(),
                    inherited: false,
                },
            }],
            vec![false],
            false,
        );
        app.page_view = true;
        app.light_page = true;
        app.dirty = true;
        let document_before = app.editor.doc.clone();

        let mut term = Terminal::new(TestBackend::new(100, 70)).unwrap();
        term.draw(|frame| app.draw(frame)).unwrap();

        let screen = format!("{:?}", term.backend().buffer());
        assert!(
            screen.contains("[Watermark: 機密 SECRET]"),
            "watermark overlay missing from page view: {screen}"
        );
        assert!(
            app.lines
                .iter()
                .all(|line| !line.plain().contains("機密 SECRET")),
            "overlay text must not enter rendered document lines"
        );
        assert_eq!(app.editor.doc, document_before);
        assert!(app.maps.iter().any(LineMap::is_editable));
        assert!(
            app.maps
                .iter()
                .flat_map(|map| &map.segs)
                .all(|seg| seg.path.first() == Some(&0)),
            "overlay must not add a caret/hit-testing segment"
        );

        app.editor.select_all();
        app.do_copy();
        assert_eq!(app.clip_text.as_deref(), Some("body text"));

        app.page_view = false;
        app.dirty = true;
        app.ensure_rendered(99);
        assert!(app.watermark_overlays.is_empty());
        assert!(
            app.lines
                .iter()
                .all(|line| !line.plain().contains("機密 SECRET"))
        );
    }

    #[test]
    fn watermark_renderer_handles_tiny_viewports_and_plain_documents() {
        let mut plain = app_with(&["plain"]);
        plain.page_view = true;
        plain.dirty = true;
        plain.ensure_rendered(7);
        assert!(plain.watermark_overlays.is_empty());

        let mut app = app_with(&["body"]);
        app.watermark_state = watermark::State::new(
            vec![Watermark {
                kind: WatermarkKind::Text("超長い機密透かし".to_string()),
                header: WatermarkHeader {
                    section_index: 0,
                    variant: HeaderVariant::Default,
                    relationship_id: "rIdWatermark".to_string(),
                    part_name: "word/header1.xml".to_string(),
                    inherited: false,
                },
            }],
            vec![false],
            false,
        );
        app.page_view = true;
        app.dirty = true;
        let mut term = Terminal::new(TestBackend::new(8, 4)).unwrap();

        term.draw(|frame| app.draw(frame)).unwrap();

        assert!(!app.watermark_overlays.is_empty());
        assert_eq!(term.backend().buffer().area.width, 8);
        assert_eq!(term.backend().buffer().area.height, 4);
    }

    #[test]
    fn package_watermark_fixtures_drive_overlay_state_and_sandboxed_capture() {
        use crate::test_fixtures::WatermarkFixture;

        let mut text = App::new(
            WatermarkFixture::Text.package(),
            "text-watermark.docx",
            false,
        );
        text.os_clip = None;
        text.page_view = true;
        text.light_page = true;
        text.dirty = true;
        let mut terminal = Terminal::new(TestBackend::new(100, 70)).unwrap();
        terminal.draw(|frame| text.draw(frame)).unwrap();
        let capture = format!("{:?}", terminal.backend().buffer());

        assert!(capture.contains("[Watermark: CONFIDENTIAL & REVIEW]"));
        assert!(capture.contains("Fixture body"));
        assert_eq!(text.watermark_overlays.len(), 1);
        assert!(
            text.lines
                .iter()
                .all(|line| !line.plain().contains("CONFIDENTIAL & REVIEW"))
        );
        text.editor.select_all();
        text.do_copy();
        assert_eq!(text.clip_text.as_deref(), Some("Fixture body"));

        // The TestBackend capture is deterministic and sandboxed under target;
        // it gives manual evidence without OCR or a golden-image dependency.
        let artifact_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/test-artifacts/docxy");
        std::fs::create_dir_all(&artifact_dir).expect("create watermark artifact directory");
        let artifact = artifact_dir.join("watermark-page-view.txt");
        std::fs::write(&artifact, &capture).expect("write watermark page-view capture");
        eprintln!("wrote {}", artifact.display());

        let mut picture = App::new(
            WatermarkFixture::Picture.package(),
            "picture-watermark.docx",
            false,
        );
        picture.page_view = true;
        picture.dirty = true;
        picture.ensure_rendered(99);
        assert_eq!(picture.watermark_overlays.len(), 1);
        assert!(
            picture.watermark_overlays[0]
                .text
                .contains("picture preview unavailable")
        );

        let mut inherited = App::new(
            WatermarkFixture::InheritedText.package(),
            "inherited-watermark.docx",
            false,
        );
        inherited.page_view = true;
        inherited.dirty = true;
        inherited.ensure_rendered(99);
        assert_eq!(inherited.watermark_overlays.len(), 2);
        assert!(
            inherited
                .watermark_overlays
                .iter()
                .all(|overlay| overlay.text.contains("INHERITED DRAFT"))
        );
    }

    #[test]
    fn doc_notice_reports_surfaced_features() {
        let body = vec![Block::Paragraph(docxcore::model::Paragraph::default())];
        let mut app = App::new(new_package(Document { body }), "a.docx", false);
        // A plain document shows nothing.
        assert_eq!(app.doc_notice(), "");
        // Each surfaced feature appears in the notice.
        app.doc_protection.enforcement = docxcore::package::ProtectionEnforcement::Enforced;
        app.doc_protection.edit_mode = Some(docxcore::package::ProtectionEditMode::ReadOnly);
        app.watermark_state = watermark::State::new(
            vec![Watermark {
                kind: WatermarkKind::Text("CONFIDENTIAL".to_string()),
                header: WatermarkHeader {
                    section_index: 0,
                    variant: HeaderVariant::Default,
                    relationship_id: "rIdWatermark".to_string(),
                    part_name: "word/header1.xml".to_string(),
                    inherited: false,
                },
            }],
            vec![false],
            false,
        );
        app.doc_page_borders = true;
        let n = app.doc_notice();
        assert!(n.contains("Protected: read-only"), "{n}");
        assert!(n.contains("Watermark: CONFIDENTIAL"), "{n}");
        assert!(n.contains("Page border"), "{n}");
    }

    #[test]
    fn transient_status_does_not_hide_advisory_protection_notice() {
        let mut app = App::new(
            crate::test_fixtures::ProtectionFixture::Advisory.package(),
            "advisory.docx",
            false,
        );
        app.status = Some("opened advisory.docx".to_string());
        let mut terminal = Terminal::new(TestBackend::new(140, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let screen = format!("{:?}", terminal.backend().buffer());
        assert!(screen.contains("opened advisory.docx"), "{screen}");
        assert!(
            screen.contains("Protected: read-only (recommended)"),
            "{screen}"
        );
    }

    #[test]
    fn watermark_scope_refreshes_after_section_edit_and_undo() {
        let mut app = App::new(
            crate::test_fixtures::WatermarkFixture::InheritedText.package(),
            "inherited.docx",
            false,
        );
        assert_eq!(app.watermark_state.mark_count(), 2);

        assert!(app.editor.set_caret_section_break(None));
        app.after_edit();
        assert_eq!(app.watermark_state.mark_count(), 0);

        assert!(app.editor.undo());
        app.after_edit();
        assert_eq!(app.watermark_state.mark_count(), 2);
    }

    #[test]
    fn vim_history_refreshes_watermark_section_scope() {
        let mut app = App::new(
            crate::test_fixtures::WatermarkFixture::InheritedText.package(),
            "inherited.docx",
            false,
        );
        app.vim = Some(VimState::new());
        assert_eq!(app.watermark_state.mark_count(), 2);

        assert!(app.editor.set_caret_section_break(None));
        app.after_edit();
        assert_eq!(app.watermark_state.mark_count(), 0);

        app.on_key(key(KeyCode::Char('u')));
        assert_eq!(app.watermark_state.mark_count(), 2);

        app.on_key(ctrl(KeyCode::Char('r')));
        assert_eq!(app.watermark_state.mark_count(), 0);
    }

    #[test]
    fn denied_ribbon_keyboard_and_mouse_actions_preserve_renderer_state() {
        let mut app = app_with(&["format me"]);
        app.editor.select_all();
        protect(&mut app, ProtectionEditMode::Unrestricted, true);
        app.ribbon_open = true;

        let bold_index = (0..128)
            .find(|&index| {
                matches!(
                    app.ribbon.focus_act(ribbon::Focus::Button(index)),
                    Some((ribbon::Act::Bold, _))
                )
            })
            .expect("Bold ribbon button");
        app.ribbon_focus = ribbon::Focus::Button(bold_index);
        app.dirty = false;
        app.status = None;
        app.ribbon_key(key(KeyCode::Enter));
        assert!(!app.dirty);
        assert!(
            app.status
                .as_deref()
                .is_some_and(|status| status.contains("formatting is locked"))
        );

        let mut terminal = Terminal::new(TestBackend::new(140, 24)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let (x, y) = (0..ribbon::EXPANDED_H)
            .flat_map(|y| (0..140).map(move |x| (x, y)))
            .find(|&(x, y)| {
                matches!(
                    app.ribbon.hit(x, y, true),
                    ribbon::Hit::Button(ribbon::Act::Bold)
                )
            })
            .expect("Bold ribbon hit target");
        app.dirty = false;
        app.status = None;
        app.ribbon_click(x, y);
        assert!(!app.dirty);
        assert!(
            app.status
                .as_deref()
                .is_some_and(|status| status.contains("formatting is locked"))
        );
    }

    #[test]
    fn denied_dialog_keyboard_and_mouse_commits_preserve_renderer_state() {
        let mut app = app_with(&["format me"]);
        protect(&mut app, ProtectionEditMode::Unrestricted, true);

        app.open_para_dialog();
        app.dirty = false;
        app.status = None;
        app.para_dialog_key(key(KeyCode::Enter));
        assert!(!app.dirty);
        assert!(app.para_dialog.is_some());

        app.pd_btns[0] = Rect::new(3, 3, 4, 1);
        app.dirty = false;
        app.status = None;
        app.para_dialog_mouse(3, 3);
        assert!(!app.dirty);
        assert!(app.para_dialog.is_some());

        app.editor.select_all();
        app.open_picker(PickerKind::FontColor);
        app.fp_btns[0] = Rect::new(5, 5, 4, 1);
        app.dirty = false;
        app.status = None;
        app.picker_mouse(5, 5);
        assert!(!app.dirty);
        assert!(app.font_picker.is_some());

        protect(&mut app, ProtectionEditMode::ReadOnly, false);
        app.insert_field = Some(InsertFieldDialog { sel: 0 });
        app.dirty = false;
        app.status = None;
        app.insert_field_key(key(KeyCode::Enter));
        assert!(!app.dirty);
        assert!(app.insert_field.is_some());

        app.comment_input = Some("note".to_string());
        app.dirty = false;
        app.status = None;
        app.comment_input_key(key(KeyCode::Enter));
        assert!(!app.dirty);
        assert_eq!(app.comment_input.as_deref(), Some("note"));
    }

    #[test]
    fn app_authorization_uses_structured_protection_not_the_display_label() {
        let body = vec![Block::Paragraph(docxcore::model::Paragraph::default())];
        let mut app = App::new(new_package(Document { body }), "a.docx", false);
        app.doc_protection.enforcement = docxcore::package::ProtectionEnforcement::Enforced;
        app.doc_protection.edit_mode = Some(docxcore::package::ProtectionEditMode::Comments);

        assert_eq!(
            app.authorize_mutation(protection::MutationKind::Comment),
            Ok(())
        );
        let denial = app
            .authorize_mutation(protection::MutationKind::Content)
            .unwrap_err();
        assert_eq!(denial.code(), "comments_only");
        assert_eq!(
            denial.control_error(),
            "protection_denied:comments_only: only comment edits are allowed"
        );
        assert_eq!(
            denial.tui_status(),
            "Edit blocked: only comment edits are allowed."
        );
    }

    #[test]
    fn source_toggle_is_noop_for_docx() {
        let body = vec![Block::Paragraph(docxcore::model::Paragraph::default())];
        let mut app = App::new(new_package(Document { body }), "a.docx", false);
        app.set_md_source(true);
        assert!(!app.md_source, "docx never enters source view");
    }

    #[test]
    fn view_tab_shows_markdown_group_only_for_md() {
        let mut r = ribbon::Ribbon::home();
        r.set_active(6); // View
        // For .docx: Read/Print Layout present, no Markdown switch.
        assert!(r.has_act(ribbon::Act::PrintLayout));
        assert!(!r.has_act(ribbon::Act::MdRendered));
        // For Markdown: the page-view group is gone, replaced by Rendered/Source.
        r.set_markdown(true);
        assert!(
            !r.has_act(ribbon::Act::ReadMode),
            "Read Mode should be hidden"
        );
        assert!(
            !r.has_act(ribbon::Act::PrintLayout),
            "Print Layout should be hidden"
        );
        assert!(r.has_act(ribbon::Act::MdRendered));
        assert!(r.has_act(ribbon::Act::MdSource));
    }

    #[test]
    fn home_tab_trims_unsupported_buttons_for_markdown() {
        let mut r = ribbon::Ribbon::home();
        r.set_active(1); // Home
        // .docx exposes the full Font/Paragraph controls.
        assert!(r.has_act(ribbon::Act::FontColor));
        assert!(r.has_act(ribbon::Act::Underline));
        assert!(r.has_act(ribbon::Act::AlignCenter));
        // Markdown keeps only what it can express.
        r.set_markdown(true);
        for gone in [
            ribbon::Act::FontColor,
            ribbon::Act::Underline,
            ribbon::Act::Highlight,
            ribbon::Act::Subscript,
            ribbon::Act::AlignCenter,
            ribbon::Act::IncreaseIndent,
        ] {
            assert!(!r.has_act(gone), "{gone:?} should be hidden for Markdown");
        }
        for kept in [
            ribbon::Act::Bold,
            ribbon::Act::Italic,
            ribbon::Act::Strike,
            ribbon::Act::Bullets,
            ribbon::Act::Numbering,
        ] {
            assert!(r.has_act(kept), "{kept:?} should remain for Markdown");
        }
    }

    #[test]
    fn orient_sectpr_swaps_dimensions_and_keeps_other_props() {
        let portrait =
            "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgMar w:top=\"1440\"/></w:sectPr>";
        let land = orient_sectpr(portrait, true);
        assert!(
            land.contains("w:w=\"15840\"") && land.contains("w:h=\"12240\""),
            "{land}"
        );
        assert!(land.contains("w:orient=\"landscape\""));
        assert!(land.contains("pgMar"), "other props dropped: {land}");
        let port = orient_sectpr(&land, false);
        assert!(
            port.contains("w:w=\"12240\"") && port.contains("w:h=\"15840\""),
            "{port}"
        );
        assert!(port.contains("w:orient=\"portrait\""));
    }

    #[test]
    fn insert_landscape_section_persists() {
        let mut app = app_with(&["first", "second"]);
        app.insert_section(true); // caret is in the first paragraph
        app.pkg.document = app.editor.doc.clone();
        let bytes = save_package(&app.pkg);
        let re = load_package(&bytes).expect("reload");
        assert!(
            re.sect_pr().contains("w:orient=\"landscape\""),
            "trailing not landscape: {}",
            re.sect_pr()
        );
        match &re.document.body[0] {
            Block::Paragraph(p) => {
                assert!(p.props.section_break.is_some(), "section break not saved")
            }
            _ => panic!("first block not a paragraph"),
        }
    }

    #[test]
    fn splice_hf_preserves_wrapper_and_replaces_content() {
        // Editing a header must keep the original <w:hdr> wrapper (namespaces!)
        // and re-parse cleanly.
        let orig = "<?xml version=\"1.0\"?><w:hdr xmlns:w=\"x\" xmlns:v=\"y\">\
            <w:p><w:r><w:t>old text</w:t></w:r></w:p></w:hdr>";
        let blocks = vec![MPara {
            props: ParProps::default(),
            content: vec![Inline::Run(Run {
                text: "new text".to_string(),
                props: RunProps::default(),
            })],
        }]
        .into_iter()
        .map(Block::Paragraph)
        .collect::<Vec<_>>();
        let out = splice_hf(orig, &blocks, "w:hdr").expect("wrapper found");
        assert!(out.starts_with("<?xml"));
        assert!(out.contains("xmlns:v=\"y\""), "namespaces lost: {out}");
        assert!(
            out.contains("new text") && !out.contains("old text"),
            "{out}"
        );
        assert!(out.ends_with("</w:hdr>"));
        // And it re-parses to the new content.
        let parsed = parse_header_footer(&out, &Relationships::default());
        assert_eq!(parsed.len(), 1);
        assert!(matches!(&parsed[0], Block::Paragraph(p) if p.plain_text() == "new text"));
    }

    fn app_with(paras: &[&str]) -> App {
        let body = paras
            .iter()
            .map(|t| {
                Block::Paragraph(MPara {
                    props: ParProps::default(),
                    content: vec![Inline::Run(Run {
                        text: t.to_string(),
                        props: RunProps::default(),
                    })],
                })
            })
            .collect();
        let mut app = App::new(new_package(Document { body }), "test.docx", false);
        app.os_clip = None; // don't touch the real OS clipboard in tests
        app
    }

    fn app_with_blocks(body: Vec<Block>) -> App {
        let mut app = App::new(new_package(Document { body }), "test.docx", false);
        app.os_clip = None;
        app
    }

    fn rtl_app(text: &str) -> App {
        let mut rtl = ParProps::default();
        rtl.rtl = true;
        app_with_blocks(vec![bidi_para_with(rtl, vec![bidi_run(text)])])
    }

    fn app_with_revisions() -> App {
        let metadata = |id: &str, author: &str| RevisionMetadata {
            id: Some(id.to_string()),
            author: Some(author.to_string()),
            date: Some("2026-08-29T10:00:00Z".to_string()),
            ..RevisionMetadata::default()
        };
        let content = vec![
            Inline::Revision {
                kind: RevisionKind::Insert,
                metadata: metadata("51", "Ada"),
                raw: "<w:ins/>".to_string(),
                content: vec![Inline::Run(Run {
                    text: "inserted".to_string(),
                    props: RunProps::default(),
                })],
                content_changed: false,
            },
            Inline::UnsupportedRevision {
                kind: UnsupportedRevisionKind::MoveToRangeStart,
                metadata: metadata("52", "Grace"),
                raw: "<w:moveToRangeStart/>".to_string(),
            },
            Inline::Revision {
                kind: RevisionKind::Delete,
                metadata: metadata("53", "Linus"),
                raw: "<w:del/>".to_string(),
                content: vec![Inline::Run(Run {
                    text: "deleted".to_string(),
                    props: RunProps::default(),
                })],
                content_changed: false,
            },
        ];
        let mut app = App::new(
            new_package(Document {
                body: vec![Block::Paragraph(MPara {
                    props: ParProps::default(),
                    content,
                })],
            }),
            "review.docx",
            false,
        );
        app.os_clip = None;
        app
    }

    fn fixture_app(fixture: crate::test_fixtures::ProtectionFixture) -> App {
        let mut app = App::new(
            fixture.package(),
            &format!("{}.docx", fixture.name()),
            false,
        );
        app.os_clip = None;
        app
    }

    fn fixture_package_snapshot(app: &App) -> Vec<(String, Vec<u8>)> {
        app.pkg
            .part_names()
            .into_iter()
            .map(|name| (name.to_string(), app.pkg.part(name).unwrap().to_vec()))
            .collect()
    }

    fn protect(app: &mut App, mode: ProtectionEditMode, formatting_locked: bool) {
        app.doc_protection = Protection {
            enforcement: ProtectionEnforcement::Enforced,
            edit_mode: Some(mode),
            formatting_locked,
            ..Protection::default()
        };
    }

    fn unprotect(app: &mut App) {
        app.doc_protection = Protection::default();
    }

    #[test]
    fn denied_keys_preserve_document_history_package_and_save_state() {
        let mut app = app_with(&["alpha"]);
        app.editor.move_end();
        app.on_key(key(KeyCode::Char('b')));
        app.on_key(ctrl(KeyCode::Char('z')));
        assert_eq!(first_line(&app), "alpha");

        // Treat the current undo state as saved, with a pending redo. A rejected
        // key must not dirty it, alter package metadata, or clear that redo.
        app.modified = false;
        app.dirty = false;
        let doc = app.editor.doc.clone();
        let caret = app.editor.caret.clone();
        let sect_pr = app.pkg.sect_pr().to_string();
        let settings = app.pkg.part("word/settings.xml").map(|part| part.to_vec());
        protect(&mut app, ProtectionEditMode::ReadOnly, false);

        app.on_key(key(KeyCode::Char('X')));
        assert_eq!(app.editor.doc, doc);
        assert_eq!(app.editor.caret, caret);
        assert_eq!(app.pkg.sect_pr(), sect_pr);
        assert_eq!(
            app.pkg.part("word/settings.xml").map(|part| part.to_vec()),
            settings
        );
        assert!(!app.modified, "denial must not change save state");
        assert!(!app.dirty, "denial must not invalidate document layout");
        assert_eq!(
            app.status.as_deref(),
            Some("Edit blocked: the document is protected read-only.")
        );

        unprotect(&mut app);
        assert!(app.editor.redo(), "denial cleared the existing redo entry");
        assert_eq!(first_line(&app), "alphab");

        let mut clean = app_with(&["clean"]);
        protect(&mut clean, ProtectionEditMode::ReadOnly, false);
        clean.on_key(key(KeyCode::Enter));
        unprotect(&mut clean);
        assert!(!clean.editor.undo(), "denial pushed an undo checkpoint");
    }

    #[test]
    fn protection_package_fixtures_drive_tui_policy_status_and_history() {
        use crate::test_fixtures::ProtectionFixture;

        let denied = [
            (
                ProtectionFixture::ReadOnly,
                "Edit blocked: the document is protected read-only.",
            ),
            (
                ProtectionFixture::PasswordWrite,
                "Edit blocked: the document is protected read-only.",
            ),
            (
                ProtectionFixture::Forms,
                "Edit blocked: form-fields-only editing is not supported; use Word to edit form fields.",
            ),
            (
                ProtectionFixture::TrackedChanges,
                "Edit blocked: tracked-only editing is not supported; use Word to create tracked changes.",
            ),
            (
                ProtectionFixture::Unknown,
                "Edit blocked: the document uses unsupported protection mode 'producerSpecific'.",
            ),
        ];
        for (fixture, expected_status) in denied {
            let mut app = fixture_app(fixture);
            let fixture_protection = app.doc_protection.clone();

            // Seed a redo entry while temporarily unrestricted. The fixture's
            // denied attempt must neither push history nor clear that entry.
            app.doc_protection = Protection::default();
            app.editor.move_end();
            app.on_key(key(KeyCode::Char('!')));
            app.on_key(ctrl(KeyCode::Char('z')));
            app.doc_protection = fixture_protection;
            app.modified = false;
            app.dirty = false;
            app.status = None;

            let document = app.editor.doc.clone();
            let caret = app.editor.caret.clone();
            let package = fixture_package_snapshot(&app);
            app.on_key(key(KeyCode::Char('X')));

            assert_eq!(app.editor.doc, document, "{fixture:?}");
            assert_eq!(app.editor.caret, caret, "{fixture:?}");
            assert_eq!(fixture_package_snapshot(&app), package, "{fixture:?}");
            assert!(!app.modified, "{fixture:?} changed save state");
            assert!(!app.dirty, "{fixture:?} dirtied layout state");
            assert_eq!(app.status.as_deref(), Some(expected_status));

            app.doc_protection = Protection::default();
            assert!(app.editor.redo(), "{fixture:?} cleared pending redo");
        }

        let mut comments = fixture_app(ProtectionFixture::Comments);
        comments.editor.select_all();
        comments.run_act(ribbon::Act::NewComment);
        for c in "fixture note".chars() {
            comments.on_key(key(KeyCode::Char(c)));
        }
        comments.on_key(key(KeyCode::Enter));
        assert_eq!(comments.comments.len(), 1);
        // A new comment reaches comments.xml when a save reconciles (#620).
        comments.reconcile_tracked_comments();
        assert!(comments.pkg.part("word/comments.xml").is_some());
        comments.modified = false;
        comments.dirty = false;
        let commented = comments.editor.doc.clone();
        comments.on_key(key(KeyCode::Char('X')));
        assert_eq!(comments.editor.doc, commented);
        assert!(!comments.modified);
        assert!(!comments.dirty);
        assert_eq!(
            comments.status.as_deref(),
            Some("Edit blocked: only comment edits are allowed.")
        );

        let mut formatting = fixture_app(ProtectionFixture::FormattingOnly);
        formatting.editor.select_all();
        formatting.dirty = false;
        let unformatted = formatting.editor.doc.clone();
        formatting.on_key(ctrl(KeyCode::Char('b')));
        assert_eq!(formatting.editor.doc, unformatted);
        assert!(!formatting.modified);
        assert!(!formatting.dirty);
        assert_eq!(
            formatting.status.as_deref(),
            Some("Edit blocked: document formatting is locked.")
        );
        let mut formatting_content = fixture_app(ProtectionFixture::FormattingOnly);
        formatting_content.editor.move_end();
        formatting_content.on_key(key(KeyCode::Char('!')));
        assert_eq!(first_line(&formatting_content), "Fixture body!");
        assert!(formatting_content.modified);

        for fixture in [ProtectionFixture::Unrestricted, ProtectionFixture::Advisory] {
            let mut app = fixture_app(fixture);
            if fixture == ProtectionFixture::Advisory {
                assert!(
                    app.doc_notice()
                        .contains("Protected: read-only (recommended)")
                );
            }
            app.editor.move_end();
            app.on_key(key(KeyCode::Char('!')));
            assert_eq!(first_line(&app), "Fixture body!", "{fixture:?}");
            assert!(app.modified, "{fixture:?}");
        }
    }

    #[test]
    fn review_navigation_status_and_shortcuts_include_kind_and_metadata() {
        let mut app = app_with_revisions();
        app.run_act(ribbon::Act::NextRevision);
        let status = app.status.as_deref().unwrap();
        assert!(status.contains("Change 2/3"), "{status}");
        assert!(
            status.contains("unsupported move-to range start"),
            "{status}"
        );
        assert!(status.contains("id 52"), "{status}");
        assert!(status.contains("author Grace"), "{status}");
        assert!(status.contains("date 2026-08-29T10:00:00Z"), "{status}");

        app.on_key(alt_shift(KeyCode::Left));
        let current = app.editor.current_revision().unwrap();
        assert_eq!(current.address.target.0, 1);
        assert!(app.status.as_deref().unwrap().contains("Change 1/3"));

        let before = app.editor.doc.clone();
        app.on_key(alt_shift(KeyCode::Char('A')));
        assert!(app.modified);
        assert_eq!(app.editor.revision_locations().len(), 2);
        let status = app.status.as_deref().unwrap();
        assert!(status.contains("Accepted insertion (target 1)"), "{status}");
        assert!(app.editor.undo());
        assert_eq!(app.editor.doc, before);
    }

    #[test]
    fn review_all_ribbon_actions_use_default_no_confirmation_and_one_undo() {
        let mut app = app_with_revisions();
        let before = app.editor.doc.clone();
        app.run_act(ribbon::Act::AcceptAllRevisions);
        let confirm = app.confirm.as_ref().expect("accept all confirmation");
        assert!(
            !confirm.yes_selected(),
            "destructive review defaulted to Yes"
        );
        assert!(confirm.prompt().contains("Accept all 3 tracked changes"));
        app.on_key(key(KeyCode::Enter));
        assert!(app.confirm.is_none());
        assert_eq!(app.editor.doc, before, "default No changed the document");
        assert!(!app.modified);

        app.run_act(ribbon::Act::RejectAllRevisions);
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.modified);
        assert_eq!(app.editor.revision_locations().len(), 1);
        let status = app.status.as_deref().unwrap();
        assert!(
            status.contains("Rejected 2 of 3 tracked changes"),
            "{status}"
        );
        assert!(status.contains("1 unsupported"), "{status}");
        assert!(app.editor.undo());
        assert_eq!(app.editor.doc, before);
        assert!(!app.editor.undo(), "review-all pushed multiple checkpoints");
    }

    #[test]
    fn unsupported_review_action_is_reported_without_mutation() {
        let mut app = app_with_revisions();
        app.run_act(ribbon::Act::NextRevision);
        let before = app.editor.doc.clone();
        app.run_act(ribbon::Act::RejectRevision);
        assert_eq!(app.editor.doc, before);
        assert!(!app.modified);
        let status = app.status.as_deref().unwrap();
        assert!(
            status.contains("unsupported (move-to range start)"),
            "{status}"
        );
        assert!(status.contains("left untouched"), "{status}");
    }

    #[test]
    fn tracked_only_protection_blocks_review_and_untracked_tui_edits() {
        let mut app = app_with_revisions();
        protect(&mut app, ProtectionEditMode::TrackedChanges, false);
        let before = app.editor.doc.clone();

        app.run_act(ribbon::Act::AcceptRevision);
        assert_eq!(app.editor.doc, before);
        assert!(!app.modified);
        assert_eq!(
            app.status.as_deref(),
            Some(
                "Edit blocked: tracked-only editing is not supported; use Word to create tracked changes."
            )
        );

        app.status = None;
        app.run_act(ribbon::Act::AcceptAllRevisions);
        assert!(app.confirm.is_none(), "denied review opened a confirmation");
        assert_eq!(app.editor.doc, before);

        app.status = None;
        app.on_key(key(KeyCode::Char('X')));
        assert_eq!(app.editor.doc, before);
        assert!(!app.modified);
        assert!(!app.editor.undo());
        assert_eq!(
            app.status.as_deref(),
            Some(
                "Edit blocked: tracked-only editing is not supported; use Word to create tracked changes."
            )
        );
    }

    #[test]
    fn tui_save_preserves_protection_and_watermark_fixture_metadata() {
        use crate::test_fixtures::{ProtectionFixture, WatermarkFixture};

        let fixtures = ProtectionFixture::ALL
            .into_iter()
            .map(|fixture| (fixture.name(), fixture.package()))
            .chain(
                WatermarkFixture::ALL
                    .into_iter()
                    .map(|fixture| (fixture.name(), fixture.package())),
            );
        for (name, package) in fixtures {
            let expected_protection = package.protection();
            let expected_watermarks = package.watermarks();
            let expected_document_xml = package
                .part("word/document.xml")
                .expect("fixture main document")
                .to_vec();
            let preserved = package
                .part_names()
                .into_iter()
                .filter(|part| *part != "word/document.xml")
                .map(|part| (part.to_string(), package.part(part).unwrap().to_vec()))
                .collect::<Vec<_>>();
            let path =
                std::env::temp_dir().join(format!("docxy-{name}-{}-save.docx", std::process::id()));
            let mut app = App::new(package, &path.to_string_lossy(), false);
            app.save();
            assert!(!app.modified, "{name}");

            let bytes = std::fs::read(&path).expect("read saved fixture");
            std::fs::remove_file(&path).expect("remove saved fixture");
            let reloaded = load_package(&bytes).expect("reload TUI-saved fixture");
            assert_eq!(reloaded.protection(), expected_protection, "{name}");
            assert_eq!(reloaded.watermarks(), expected_watermarks, "{name}");
            assert_eq!(
                reloaded.part("word/document.xml"),
                Some(expected_document_xml.as_slice()),
                "{name}: untouched saves must preserve the main document verbatim"
            );
            for (part, expected) in &preserved {
                assert_eq!(
                    reloaded.part(part),
                    Some(expected.as_slice()),
                    "{name}:{part}"
                );
            }
        }
    }

    #[test]
    fn protected_load_does_not_recompute_cached_field_content() {
        use crate::test_fixtures::ProtectionFixture;

        let field_document = Document {
            body: vec![Block::Paragraph(docxcore::model::Paragraph {
                props: Default::default(),
                content: vec![Inline::Field {
                    raw: r#"<w:fldSimple w:instr="= 2+2"><w:r><w:t>OLD</w:t></w:r></w:fldSimple>"#
                        .to_string(),
                    text: "OLD".to_string(),
                }],
            })],
        };
        let mut protected = ProtectionFixture::ReadOnly.package();
        protected.document = field_document.clone();
        let protected_app = App::new(protected, "protected.docx", false);
        let doc = &protected_app.editor.doc;
        assert_eq!(
            doc.body[..doc.content_block_count()],
            field_document.body[..]
        );

        let mut unrestricted = ProtectionFixture::Unrestricted.package();
        unrestricted.document = field_document;
        let unrestricted_app = App::new(unrestricted, "unrestricted.docx", false);
        assert_eq!(first_line(&unrestricted_app), "4");
    }

    #[test]
    fn protected_documents_keep_navigation_selection_copy_and_find_available() {
        let mut app = app_with(&["alpha beta"]);
        protect(&mut app, ProtectionEditMode::ReadOnly, false);
        let doc = app.editor.doc.clone();

        app.editor.move_home();
        app.on_key(key(KeyCode::Right));
        assert_eq!(app.editor.caret.offset, 1);
        app.on_key(ctrl(KeyCode::Char('a')));
        assert!(app.editor.has_selection());
        app.on_key(ctrl(KeyCode::Char('c')));
        assert_eq!(app.status.as_deref(), Some("Copied"));
        app.on_key(ctrl(KeyCode::Char('f')));
        assert!(app.find.is_some());
        app.on_key(key(KeyCode::Char('a')));
        assert!(!app.find.as_ref().unwrap().matches.is_empty());
        assert!(app.current_markdown().contains("alpha beta"));

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let pdf = std::env::temp_dir().join(format!("docxy-protected-export-{nonce}.pdf"));
        app.write_pdf(pdf.clone());
        assert!(std::fs::metadata(&pdf).unwrap().len() > 0);
        std::fs::remove_file(pdf).unwrap();
        assert_eq!(app.editor.doc, doc);
        assert!(!app.modified);
    }

    #[test]
    fn formatting_lock_gates_ribbon_and_dialog_commits_but_allows_content() {
        let mut app = app_with(&["format me"]);
        app.editor.select_all();
        protect(&mut app, ProtectionEditMode::Unrestricted, true);
        let doc = app.editor.doc.clone();
        let numbering = app.pkg.part("word/numbering.xml").map(|part| part.to_vec());

        app.run_act(ribbon::Act::Bold);
        app.run_act(ribbon::Act::Bullets);
        assert_eq!(app.editor.doc, doc);
        assert_eq!(
            app.pkg.part("word/numbering.xml").map(|part| part.to_vec()),
            numbering,
            "a denied list must not create numbering metadata"
        );
        assert!(!app.modified);
        assert_eq!(
            app.status.as_deref(),
            Some("Edit blocked: document formatting is locked.")
        );

        app.run_act(ribbon::Act::ParagraphDialog);
        assert!(app.para_dialog.is_some());
        app.on_key(key(KeyCode::Enter));
        assert!(
            app.para_dialog.is_some(),
            "denial must not consume the dialog"
        );
        assert_eq!(app.editor.doc, doc);
        app.on_key(key(KeyCode::Esc));

        // A content-insertion dialog remains usable under formatting-only
        // protection.
        app.run_act(ribbon::Act::InsertField);
        app.on_key(key(KeyCode::Enter));
        assert!(app.modified);
        assert!(app.editor.doc.body.iter().any(|block| matches!(block,
            Block::Paragraph(p) if p.content.iter().any(|inline| matches!(inline, Inline::Field { .. }))
        )));
    }

    #[test]
    fn formatting_lock_allows_structural_paragraph_sorting() {
        let mut app = app_with(&["zebra", "alpha"]);
        app.editor.select_all();
        protect(&mut app, ProtectionEditMode::Unrestricted, true);

        app.run_act(ribbon::Act::Sort);

        let doc = &app.editor.doc;
        let paragraphs = doc.body[..doc.content_block_count()]
            .iter()
            .map(Block::plain_text)
            .collect::<Vec<_>>();
        assert_eq!(paragraphs, ["alpha", "zebra"]);
        assert!(app.modified);
    }

    #[test]
    fn formatting_lock_skips_hrule_autoformat_but_keeps_newline_editing() {
        let mut app = app_with(&["---"]);
        app.editor.move_end();
        protect(&mut app, ProtectionEditMode::Unrestricted, true);

        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.editor.doc.content_block_count(), 2);
        assert_eq!(first_line(&app), "---");
        assert!(
            matches!(&app.editor.doc.body[0], Block::Paragraph(p) if p.props.borders.bottom.is_none())
        );
        assert!(app.modified);
    }

    #[test]
    fn comments_only_allows_comment_commits_and_denies_unrelated_edits() {
        let mut app = app_with(&["review me"]);
        protect(&mut app, ProtectionEditMode::Comments, false);
        app.editor.select_all();
        app.run_act(ribbon::Act::NewComment);
        for c in "note".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.comments.len(), 1);
        // A new comment reaches comments.xml when a save reconciles (#620).
        app.reconcile_tracked_comments();
        assert!(app.pkg.part("word/comments.xml").is_some());
        assert!(app.modified);

        app.modified = false;
        let commented = app.editor.doc.clone();
        app.on_key(key(KeyCode::Char('X')));
        assert_eq!(app.editor.doc, commented);
        assert!(!app.modified);
        assert_eq!(
            app.status.as_deref(),
            Some("Edit blocked: only comment edits are allowed.")
        );
    }

    #[test]
    fn vim_operators_are_gated_before_motion_and_allowed_by_formatting_only_mode() {
        let mut denied = vim_app(&["abc"]);
        denied.editor.move_home();
        protect(&mut denied, ProtectionEditMode::ReadOnly, false);
        let doc = denied.editor.doc.clone();
        let caret = denied.editor.caret.clone();
        denied.dirty = false;
        denied.on_key(key(KeyCode::Char('x')));
        assert_eq!(denied.editor.doc, doc);
        assert_eq!(denied.editor.caret, caret);
        assert!(!denied.modified);
        assert!(!denied.dirty);

        denied.on_key(key(KeyCode::Char('d')));
        denied.on_key(key(KeyCode::Char('l')));
        assert_eq!(denied.editor.doc, doc);
        assert_eq!(
            denied.editor.caret, caret,
            "denied operator moved the caret"
        );

        let mut allowed = vim_app(&["abc"]);
        allowed.editor.move_home();
        protect(&mut allowed, ProtectionEditMode::Unrestricted, true);
        allowed.on_key(key(KeyCode::Char('x')));
        assert_eq!(first_line(&allowed), "bc");
        assert!(allowed.modified);
    }

    #[test]
    fn formatting_lock_denies_rich_vim_pastes_before_moving_the_caret() {
        let bold_run = Run {
            text: "rich".to_string(),
            props: RunProps {
                bold: true,
                ..RunProps::default()
            },
        };
        let mut charwise = vim_app(&["dest"]);
        charwise.clipboard = Some(Clip {
            paras: vec![vec![Inline::Run(bold_run.clone())]],
        });
        protect(&mut charwise, ProtectionEditMode::Unrestricted, true);
        let charwise_doc = charwise.editor.doc.clone();
        let charwise_caret = charwise.editor.caret.clone();

        charwise.on_key(key(KeyCode::Char('p')));

        assert_eq!(charwise.editor.doc, charwise_doc);
        assert_eq!(charwise.editor.caret, charwise_caret);
        assert!(!charwise.modified);
        assert_eq!(
            charwise.status.as_deref(),
            Some("Edit blocked: document formatting is locked.")
        );

        let mut linewise = vim_app(&["dest"]);
        linewise.clipboard = Some(Clip {
            paras: vec![vec![Inline::TextBox {
                raw: String::new(),
                blocks: vec![Block::Paragraph(docxcore::model::Paragraph {
                    props: Default::default(),
                    content: vec![Inline::Run(bold_run)],
                })],
            }]],
        });
        linewise.vim.as_mut().unwrap().linewise_clip = true;
        protect(&mut linewise, ProtectionEditMode::Unrestricted, true);
        let linewise_doc = linewise.editor.doc.clone();
        let linewise_caret = linewise.editor.caret.clone();

        linewise.on_key(key(KeyCode::Char('P')));

        assert_eq!(linewise.editor.doc, linewise_doc);
        assert_eq!(linewise.editor.caret, linewise_caret);
        assert!(!linewise.modified);
        assert_eq!(
            linewise.status.as_deref(),
            Some("Edit blocked: document formatting is locked.")
        );
    }

    #[test]
    fn header_edits_are_denied_before_part_creation_and_allowed_when_conforming() {
        let mut denied = app_with(&["body"]);
        protect(&mut denied, ProtectionEditMode::ReadOnly, false);
        let sect_pr = denied.pkg.sect_pr().to_string();
        denied.run_act(ribbon::Act::EditHeader);
        assert!(denied.hf_edit.is_none());
        assert!(denied.header_part.is_none());
        assert_eq!(denied.pkg.sect_pr(), sect_pr);
        assert!(!denied.modified);

        let mut allowed = app_with(&["body"]);
        protect(&mut allowed, ProtectionEditMode::Unrestricted, true);
        allowed.run_act(ribbon::Act::EditHeader);
        assert!(allowed.hf_edit.is_some());
        allowed.on_key(key(KeyCode::Char('H')));
        allowed.editor.select_all();
        allowed.on_key(ctrl(KeyCode::Char('b')));
        assert_eq!(first_line(&allowed), "H");
        assert!(!run0(&allowed).props.bold);
        allowed.on_key(key(KeyCode::F(6)));
        assert!(allowed.hf_edit.is_none());
        let part = allowed.header_part.as_deref().expect("header part created");
        let xml = String::from_utf8_lossy(allowed.pkg.part(part).unwrap());
        assert!(xml.contains(">H<"), "header edit was not committed: {xml}");

        // Re-entering only to attempt a denied formatting change must not
        // rewrite the part or mark the file modified on exit.
        let part = part.to_string();
        let before = allowed.pkg.part(&part).unwrap().to_vec();
        allowed.modified = false;
        allowed.run_act(ribbon::Act::EditHeader);
        allowed.editor.select_all();
        allowed.on_key(ctrl(KeyCode::Char('b')));
        allowed.on_key(key(KeyCode::F(6)));
        assert!(!allowed.modified);
        assert_eq!(allowed.pkg.part(&part).unwrap(), before);
    }

    #[test]
    fn splice_hf_without_the_wrapper_is_none() {
        let blocks = vec![Block::Paragraph(MPara::default())];
        assert_eq!(splice_hf("<w:ftr/>", &blocks, "w:hdr"), None);
        assert_eq!(splice_hf("not xml", &blocks, "w:hdr"), None);
        assert_eq!(splice_hf("</w:hdr><w:hdr>", &blocks, "w:hdr"), None);
    }

    /// An app whose header part (created by a first header edit typing
    /// `first`) is then rewritten as `bytes`.
    fn app_with_header_part(bytes: impl FnOnce(&str) -> Vec<u8>) -> (App, String) {
        let mut app = app_with(&["body"]);
        app.run_act(ribbon::Act::EditHeader);
        for c in "first".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::F(6)));
        let part = app.header_part.clone().expect("header part created");
        let xml = app.pkg.part_text(&part).unwrap();
        assert!(app.pkg.set_part(&part, bytes(&xml)));
        (app, part)
    }

    fn utf16le_bom(text: &str) -> Vec<u8> {
        let mut out = vec![0xff, 0xfe];
        for unit in text.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out
    }

    #[test]
    fn header_edit_on_utf16_part_survives_commit_and_save() {
        let (mut app, part) = app_with_header_part(|xml| {
            utf16le_bom(&xml.replacen("encoding=\"UTF-8\"", "encoding=\"UTF-16\"", 1))
        });
        app.run_act(ribbon::Act::EditHeader);
        app.editor.select_all();
        for c in "second".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::F(6)));
        assert!(app.hf_edit.is_none());

        let bytes = app.pkg.part(&part).unwrap();
        assert!(
            bytes.starts_with(&[0xff, 0xfe, b'<', 0]),
            "still UTF-16LE with a BOM"
        );
        let xml = app.pkg.part_text(&part).expect("decodes");
        assert!(xml.contains("encoding=\"UTF-16\""), "{xml}");
        assert!(
            xml.contains("<w:hdr ") && xml.contains("xmlns:w="),
            "wrapper kept: {xml}"
        );
        assert!(
            xml.contains(">second<") && !xml.contains(">first<"),
            "{xml}"
        );

        // The saved file reads back with the edit, and the PDF prints it.
        let saved = load_package(&save_package(&app.pkg)).expect("reload");
        let blocks = saved.header_footer_blocks(&part).expect("header blocks");
        assert_eq!(blocks[0].plain_text(), "second");
        let opts = PdfOptions::from_package(&saved, app.styles.clone());
        let pdf = String::from_utf8_lossy(&to_pdf(&saved.document, &opts)).into_owned();
        assert!(pdf.contains("(second) Tj"), "{pdf}");
    }

    #[test]
    fn header_edit_with_missing_wrapper_reports_and_changes_nothing() {
        let (mut app, part) = app_with_header_part(|_| b"<junk/>".to_vec());
        let before = app.headers.default.clone();
        app.run_act(ribbon::Act::EditHeader);
        app.editor.select_all();
        app.on_key(key(KeyCode::Char('x')));
        app.on_key(key(KeyCode::F(6)));
        assert!(app.hf_edit.is_none());
        assert_eq!(app.pkg.part(&part).unwrap(), b"<junk/>", "part untouched");
        assert_eq!(
            app.headers.default, before,
            "the page view keeps the old header"
        );
        let status = app.status.clone().unwrap_or_default();
        assert!(
            status.contains("Couldn't write the header edit"),
            "{status}"
        );
    }

    #[test]
    fn header_edit_links_get_header_relationships() {
        use docxcore::model::Hyperlink;
        let hl = |url: &str, rel_id: Option<&str>, raw: Option<String>| {
            Inline::Hyperlink(Hyperlink {
                target: Some(url.to_string()),
                rel_id: rel_id.map(str::to_string),
                runs: vec![Run {
                    text: url.to_string(),
                    props: RunProps::default(),
                }],
                raw,
                ..Hyperlink::default()
            })
        };
        let mut app = app_with(&["body"]);
        app.run_act(ribbon::Act::EditHeader);
        let part = app.header_part.clone().expect("header part created");
        // Paste Special > Hyperlink (no id) and a Keep Source paste of a body
        // link (the body's id, with its preserved markup).
        let pasted = r#"<w:hyperlink r:id="rId42" w:tooltip="tip"><w:r><w:t>https://b.example/</w:t></w:r></w:hyperlink>"#;
        app.editor.doc.body = vec![Block::Paragraph(MPara {
            props: ParProps::default(),
            content: vec![
                hl("https://a.example/", None, None),
                hl(
                    "https://b.example/",
                    Some("rId42"),
                    Some(pasted.to_string()),
                ),
            ],
        })];
        app.on_key(key(KeyCode::F(6)));
        assert!(app.hf_edit.is_none());

        let rels_name = part.replace("word/", "word/_rels/") + ".rels";
        let rels = parse_rels_xml(&app.pkg.part_text(&rels_name).expect("header rels"));
        let xml = app.pkg.part_text(&part).unwrap();
        assert!(!xml.contains("rId42"), "{xml}");
        assert!(xml.contains(r#"w:tooltip="tip""#), "{xml}");

        // Saved and reopened, both links keep their URLs, and print as links.
        let saved = load_package(&save_package(&app.pkg)).expect("reload");
        let blocks = saved.header_footer_blocks(&part).expect("header blocks");
        let Block::Paragraph(p) = &blocks[0] else {
            panic!("{blocks:?}")
        };
        let targets: Vec<_> = p
            .content
            .iter()
            .filter_map(|inl| match inl {
                Inline::Hyperlink(h) => {
                    let id = h.rel_id.as_deref().expect("an id");
                    assert_eq!(rels.target(id), h.target.as_deref());
                    h.target.clone()
                }
                _ => None,
            })
            .collect();
        assert_eq!(targets, ["https://a.example/", "https://b.example/"]);
        let opts = PdfOptions::from_package(&saved, app.styles.clone());
        let pdf = String::from_utf8_lossy(&to_pdf(&saved.document, &opts)).into_owned();
        assert!(pdf.contains("/URI (https://a.example/)"), "{pdf}");
        assert!(pdf.contains("/URI (https://b.example/)"));
    }

    #[test]
    fn header_edit_link_with_unreadable_header_rels_writes_nothing() {
        let link_body = |url: &str| {
            vec![Block::Paragraph(MPara {
                props: ParProps::default(),
                content: vec![Inline::Hyperlink(docxcore::model::Hyperlink {
                    target: Some(url.to_string()),
                    runs: vec![Run {
                        text: url.to_string(),
                        props: RunProps::default(),
                    }],
                    ..docxcore::model::Hyperlink::default()
                })],
            })]
        };
        // A first link creates the header's rels, which then turn unreadable.
        let mut app = app_with(&["body"]);
        app.run_act(ribbon::Act::EditHeader);
        let part = app.header_part.clone().expect("header part created");
        app.editor.doc.body = link_body("https://a.example/");
        app.on_key(key(KeyCode::F(6)));
        let rels_name = part.replace("word/", "word/_rels/") + ".rels";
        assert!(
            app.pkg
                .set_part(&rels_name, b"<NotRelationships/>".to_vec())
        );
        let header_before = app.pkg.part(&part).unwrap().to_vec();
        let shown_before = app.headers.default.clone();
        app.run_act(ribbon::Act::EditHeader);
        app.editor.doc.body = link_body("https://b.example/");
        app.on_key(key(KeyCode::F(6)));
        assert!(app.hf_edit.is_none());
        assert_eq!(
            app.pkg.part(&part).unwrap(),
            header_before,
            "header untouched"
        );
        assert_eq!(app.pkg.part(&rels_name).unwrap(), b"<NotRelationships/>");
        assert_eq!(app.headers.default, shown_before);
        let status = app.status.clone().unwrap_or_default();
        assert!(
            status.contains("Couldn't write the header edit")
                && status.contains("Relationships root"),
            "{status}"
        );
    }

    #[test]
    fn pdf_export_prints_the_header_the_page_view_shows() {
        let mut app = app_with(&["body"]);
        app.run_act(ribbon::Act::EditHeader);
        app.on_key(key(KeyCode::Char('H')));
        app.on_key(key(KeyCode::F(6)));
        assert!(app.hf_edit.is_none());
        let pdf =
            String::from_utf8_lossy(&to_pdf(&app.editor.doc, &app.pdf_options())).into_owned();
        assert!(
            pdf.contains("(H) Tj"),
            "committed header missing from the PDF"
        );
    }

    /// An app on a Word-style document: the body ends with its own `w:sectPr`.
    fn app_with_trailing_sect_pr() -> App {
        app_with_blocks(vec![
            Block::Paragraph(MPara {
                props: ParProps::default(),
                content: vec![Inline::Run(Run {
                    text: "body".to_string(),
                    props: RunProps::default(),
                })],
            }),
            Block::SectionProperties(docxcore::model::SectionProperties {
                raw: r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/></w:sectPr>"#.to_string(),
                property_change: None,
            }),
        ])
    }

    #[test]
    fn pdf_export_prints_a_header_created_on_a_document_with_its_own_sect_pr() {
        let mut app = app_with_trailing_sect_pr();
        assert!(app.editor.doc.trailing_section_properties().is_some());
        app.run_act(ribbon::Act::EditHeader);
        app.on_key(key(KeyCode::Char('H')));
        app.on_key(key(KeyCode::F(6)));
        let pdf =
            String::from_utf8_lossy(&to_pdf(&app.editor.doc, &app.pdf_options())).into_owned();
        assert!(
            pdf.contains("(H) Tj"),
            "the new header is missing from the PDF"
        );
    }

    #[test]
    fn a_header_created_on_a_document_with_its_own_sect_pr_survives_save() {
        let mut app = app_with_trailing_sect_pr();
        let dir = std::env::temp_dir().join(format!("docxy-hf-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        app.path = dir.join("t.docx").to_string_lossy().into_owned();
        app.run_act(ribbon::Act::EditHeader);
        app.on_key(key(KeyCode::Char('H')));
        app.on_key(key(KeyCode::F(6)));
        let raw = &app.editor.doc.trailing_section_properties().unwrap().raw;
        assert!(
            raw.contains("headerReference"),
            "mirrored into the editor: {raw}"
        );
        app.save();
        let bytes = std::fs::read(dir.join("t.docx")).expect("saved");
        let reloaded = docxcore::package::load_package(&bytes).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            reloaded.sect_pr().contains("headerReference"),
            "{}",
            reloaded.sect_pr()
        );
    }

    #[test]
    fn columns_on_a_document_with_its_own_sect_pr_survive_save() {
        let mut app = app_with_trailing_sect_pr();
        let dir = std::env::temp_dir().join(format!("docxy-cols-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        app.path = dir.join("t.docx").to_string_lossy().into_owned();
        app.run_act(ribbon::Act::Columns);
        app.save();
        let bytes = std::fs::read(dir.join("t.docx")).expect("saved");
        let reloaded = docxcore::package::load_package(&bytes).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            reloaded.sect_pr().contains("w:num=\"2\""),
            "{}",
            reloaded.sect_pr()
        );
    }

    /// Set the open picker's selection to `label` and apply it.
    fn pick(app: &mut App, kind: PickerKind, label: &str) {
        assert_eq!(
            app.font_picker.as_ref().map(|p| p.kind),
            Some(kind),
            "{label}: no {kind:?} picker is open"
        );
        let idx = kind
            .items()
            .iter()
            .position(|&s| s == label)
            .unwrap_or_else(|| panic!("{label} not in the {kind:?} items"));
        app.font_picker.as_mut().unwrap().sel = idx;
        app.apply_picker();
    }

    #[test]
    fn design_picker_items_match_their_tables() {
        let mut colors: Vec<&str> = PAGE_COLORS.iter().map(|c| c.0).collect();
        colors.push("No Color");
        assert_eq!(PAGE_COLOR_ITEMS, colors.as_slice());
        let mut marks: Vec<&str> = WATERMARK_PRESETS.iter().map(|w| w.0).collect();
        marks.push("Remove Watermark");
        assert_eq!(WATERMARK_ITEMS, marks.as_slice());
        assert_eq!(PAGE_BORDER_ITEMS, &["None", "Box", "Shadow"]);
    }

    #[test]
    fn page_color_pick_sets_background_and_survives_save() {
        let mut app = app_with(&["body"]);
        app.run_act(ribbon::Act::PageColor);
        pick(&mut app, PickerKind::PageColor, "Red");
        assert_eq!(app.pkg.page_background().map(|b| b.color), Some(0xFF0000));
        assert!(app.pkg.has_display_background_shape());
        assert!(app.modified);
        let reloaded = save_and_reload(&mut app, "design-page-color");
        assert_eq!(reloaded.page_background().map(|b| b.color), Some(0xFF0000));
        assert!(reloaded.has_display_background_shape());
        // No Color removes the background and the display flag again.
        app.run_act(ribbon::Act::PageColor);
        pick(&mut app, PickerKind::PageColor, "No Color");
        assert!(app.pkg.page_background().is_none());
        assert!(!app.pkg.has_display_background_shape());
        // The status names the picked label even when two colours share an RGB
        // ("Orange" and "Gold, Accent 4" are both 0xFFC000).
        app.run_act(ribbon::Act::PageColor);
        pick(&mut app, PickerKind::PageColor, "Orange");
        assert_eq!(app.status.as_deref(), Some("Page color: Orange"));
    }

    #[test]
    fn watermark_pick_adds_header_watermark_and_round_trips() {
        let mut app = app_with(&["body"]); // no header at all
        app.run_act(ribbon::Act::Watermark);
        pick(&mut app, PickerKind::Watermark, "DRAFT 1");
        let marks = app.pkg.shown_text_watermarks(&app.editor.sections());
        assert_eq!(marks.len(), 1, "{marks:?}");
        assert_eq!(marks[0].text, "DRAFT");
        assert!((marks[0].rotation - 315.0).abs() < 0.5, "{marks:?}");
        assert!(app.watermark_state.label().is_some());
        assert!(app.doc_notice().contains("Watermark"));
        assert!(app.modified);
        let reloaded = save_and_reload(&mut app, "design-watermark");
        let app2 = App::new(reloaded, "t.docx", false);
        let marks = app2.pkg.shown_text_watermarks(&app2.editor.sections());
        assert_eq!(marks.len(), 1, "{marks:?}");
        assert_eq!(marks[0].text, "DRAFT");
        // Remove Watermark strips it from the shown headers.
        app.run_act(ribbon::Act::Watermark);
        pick(&mut app, PickerKind::Watermark, "Remove Watermark");
        assert!(
            app.pkg
                .shown_text_watermarks(&app.editor.sections())
                .is_empty()
        );
    }

    #[test]
    fn watermark_pick_commits_open_header_edit_first() {
        let mut app = app_with(&["body"]);
        app.run_act(ribbon::Act::EditHeader);
        app.editor.insert_str("H");
        app.run_act(ribbon::Act::Watermark);
        pick(&mut app, PickerKind::Watermark, "SAMPLE 2");
        assert!(app.hf_edit.is_none(), "the open header edit was committed");
        let part = app.header_part.clone().expect("a header part");
        let xml = app.pkg.part_text(&part).expect("header part text");
        assert!(xml.contains(">H<"), "{xml}");
        assert!(xml.contains("SAMPLE"), "{xml}");
        let reloaded = save_and_reload(&mut app, "design-wm-header");
        let app2 = App::new(reloaded, "t.docx", false);
        let part = app2.header_part.clone().expect("a header part");
        let xml = app2.pkg.part_text(&part).expect("header part text");
        assert!(xml.contains(">H<"), "{xml}");
        assert!(xml.contains("SAMPLE"), "{xml}");
    }

    #[test]
    fn page_borders_pick_applies_to_every_section_and_undoes() {
        let first = MPara {
            props: ParProps {
                section_break: Some("<w:sectPr/>".to_string()),
                ..ParProps::default()
            },
            content: vec![Inline::Run(Run {
                text: "one".to_string(),
                props: RunProps::default(),
            })],
        };
        let second = MPara {
            props: ParProps::default(),
            content: vec![Inline::Run(Run {
                text: "two".to_string(),
                props: RunProps::default(),
            })],
        };
        let mut app = app_with_blocks(vec![Block::Paragraph(first), Block::Paragraph(second)]);
        app.run_act(ribbon::Act::PageBorders);
        pick(&mut app, PickerKind::PageBorders, "Box");
        let sects = app.editor.sections();
        assert_eq!(sects.len(), 2, "{sects:?}");
        for s in &sects {
            let pb = PageBorders::parse(s)
                .unwrap_or_else(|| panic!("page borders in every section: {s}"));
            assert_eq!(pb.offset_from, PgBorderOffset::Page);
            for side in &pb.sides {
                let side = side.as_ref().expect("all four sides set");
                assert_eq!(side.style, "single");
                assert_eq!(side.sz, 4);
                assert_eq!(side.space, 24);
                assert!(side.color.is_none());
                assert!(!side.shadow);
            }
        }
        assert!(app.doc_page_borders);
        // One undo step restores every section.
        app.on_key(ctrl(KeyCode::Char('z')));
        assert!(
            !app.editor
                .sections()
                .iter()
                .any(|s| s.contains("<w:pgBorders"))
        );
        assert!(!app.doc_page_borders);
        // Shadow writes w:shadow on every side and survives save.
        app.run_act(ribbon::Act::PageBorders);
        pick(&mut app, PickerKind::PageBorders, "Shadow");
        assert!(
            app.editor
                .sections()
                .iter()
                .all(|s| s.contains("w:shadow=\"1\""))
        );
        let reloaded = save_and_reload(&mut app, "design-borders");
        assert!(reloaded.sect_pr().contains("w:shadow=\"1\""));
        // None removes the borders again.
        app.run_act(ribbon::Act::PageBorders);
        pick(&mut app, PickerKind::PageBorders, "None");
        assert!(
            app.editor
                .sections()
                .iter()
                .all(|s| PageBorders::parse(s).is_none())
        );
        assert!(!app.doc_page_borders);
    }

    #[test]
    fn design_commands_refuse_markdown() {
        let body = vec![Block::Paragraph(docxcore::model::Paragraph::default())];
        let mut app = App::new(new_markdown_package(Document { body }), "a.md", false);
        for act in [
            ribbon::Act::PageColor,
            ribbon::Act::Watermark,
            ribbon::Act::PageBorders,
        ] {
            app.run_act(act);
            assert!(
                app.font_picker.is_none(),
                "{act:?} opened a picker for Markdown"
            );
            let status = app
                .status
                .take()
                .unwrap_or_else(|| panic!("status for {act:?}"));
            assert!(status.contains(".docx"), "{status}");
        }
    }

    #[test]
    fn design_picks_respect_protection() {
        let mut app = app_with(&["body"]);
        protect(&mut app, ProtectionEditMode::ReadOnly, false);
        for act in [
            ribbon::Act::PageColor,
            ribbon::Act::Watermark,
            ribbon::Act::PageBorders,
        ] {
            // Opening the picker is allowed; applying is denied.
            app.run_act(act);
            assert!(app.font_picker.is_some(), "{act:?} did not open its picker");
            app.apply_picker(); // sel 0 = the picker's first item
            assert!(app.font_picker.is_some(), "{act:?} applied under read-only");
        }
        assert!(app.pkg.page_background().is_none());
        assert!(
            app.pkg
                .shown_text_watermarks(&app.editor.sections())
                .is_empty()
        );
        assert!(
            !app.editor
                .sections()
                .iter()
                .any(|s| s.contains("<w:pgBorders"))
        );
        assert!(!app.modified);
    }

    #[test]
    fn inserting_a_section_mirrors_the_final_sect_pr_and_undo_restores_it() {
        let mut app = app_with_trailing_sect_pr();
        let before = app.editor.doc.trailing_section_properties().cloned();
        app.insert_section(true);
        let raw = &app.editor.doc.trailing_section_properties().unwrap().raw;
        assert!(raw.contains("landscape"), "{raw}");
        assert!(app.editor.undo());
        assert_eq!(
            app.editor.doc.trailing_section_properties().cloned(),
            before
        );
    }

    #[test]
    fn toggling_columns_mirrors_into_the_editor_as_an_undo_step() {
        let mut app = app_with_trailing_sect_pr();
        let before = app.editor.doc.trailing_section_properties().cloned();
        app.run_act(ribbon::Act::Columns);
        let raw = &app.editor.doc.trailing_section_properties().unwrap().raw;
        assert!(raw.contains(r#"w:num="2""#), "{raw}");
        assert!(app.editor.undo());
        assert_eq!(
            app.editor.doc.trailing_section_properties().cloned(),
            before
        );
    }

    /// Save `app` to a temp file and load the result back.
    fn save_and_reload(app: &mut App, tag: &str) -> Package {
        let dir = std::env::temp_dir().join(format!("docxy-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        app.path = dir.join("t.docx").to_string_lossy().into_owned();
        app.modified = true;
        app.save();
        let bytes = std::fs::read(dir.join("t.docx")).expect("saved");
        let _ = std::fs::remove_dir_all(&dir);
        docxcore::package::load_package(&bytes).unwrap()
    }

    fn app_pdf(app: &App) -> String {
        String::from_utf8_lossy(&to_pdf(app.pdf_document(), &app.pdf_options())).into_owned()
    }

    fn final_sect(app: &App) -> String {
        app.editor
            .doc
            .trailing_section_properties()
            .map(|s| s.raw.clone())
            .unwrap_or_default()
    }

    #[test]
    fn undoing_header_creation_removes_the_header_everywhere_and_redo_restores_it() {
        let mut app = app_with_trailing_sect_pr();
        app.run_act(ribbon::Act::EditHeader);
        app.on_key(key(KeyCode::Char('H')));
        app.on_key(key(KeyCode::F(6)));
        assert!(app.headers.default.iter().any(|b| b.plain_text() == "H"));

        app.on_key(ctrl(KeyCode::Char('z')));
        assert!(!final_sect(&app).contains("headerReference"));
        assert!(app.headers.default.is_empty(), "the view drops the header");
        assert!(app.header_part.is_none());
        assert!(!app_pdf(&app).contains("(H) Tj"));
        let saved = save_and_reload(&mut app, "hf-undo");
        assert!(!saved.sect_pr().contains("headerReference"));

        app.on_key(ctrl(KeyCode::Char('y')));
        assert!(app.headers.default.iter().any(|b| b.plain_text() == "H"));
        assert!(app.header_part.is_some());
        assert!(app_pdf(&app).contains("(H) Tj"));
        let saved = save_and_reload(&mut app, "hf-redo");
        assert!(saved.sect_pr().contains("headerReference"));
    }

    #[test]
    fn a_header_can_be_created_again_after_undoing_its_creation() {
        let mut app = app_with_trailing_sect_pr();
        app.run_act(ribbon::Act::EditHeader);
        app.on_key(key(KeyCode::Char('H')));
        app.on_key(key(KeyCode::F(6)));
        let first_part = app.header_part.clone().unwrap();
        app.on_key(ctrl(KeyCode::Char('z')));

        app.run_act(ribbon::Act::EditHeader);
        app.on_key(key(KeyCode::Char('J')));
        app.on_key(key(KeyCode::F(6)));
        let part = app.header_part.clone().expect("linked again");
        assert_ne!(
            part, first_part,
            "a fresh part; the undone one stays orphaned"
        );
        assert!(final_sect(&app).contains("headerReference"));
        assert!(app_pdf(&app).contains("(J) Tj"));
        let saved = save_and_reload(&mut app, "hf-again");
        assert!(saved.sect_pr().contains("headerReference"));
    }

    #[test]
    fn undoing_columns_on_a_document_without_its_own_sect_pr() {
        let paras: Vec<String> = (0..60).map(|i| format!("l{i}")).collect();
        let refs: Vec<&str> = paras.iter().map(String::as_str).collect();
        let mut app = app_with(&refs);
        let count = |pdf: &str| pdf.matches("/Type /Page /Parent").count();
        assert_eq!(count(&app_pdf(&app)), 2, "one column needs two pages");
        app.run_act(ribbon::Act::Columns);
        assert_eq!(count(&app_pdf(&app)), 1, "two columns fit one page");
        app.on_key(ctrl(KeyCode::Char('z')));
        assert!(!final_sect(&app).contains(r#"w:num="2""#));
        assert_eq!(count(&app_pdf(&app)), 2, "single column again");
        let saved = save_and_reload(&mut app, "cols-undo");
        assert!(
            !saved.sect_pr().contains(r#"w:num="2""#),
            "{}",
            saved.sect_pr()
        );
        app.run_act(ribbon::Act::Columns);
        assert!(
            final_sect(&app).contains(r#"w:num="2""#),
            "{}",
            final_sect(&app)
        );
    }

    #[test]
    fn undoing_a_section_insert_on_a_document_without_its_own_sect_pr() {
        let mut app = app_with(&["first", "second"]);
        app.insert_section(true);
        assert!(app_pdf(&app).contains("/MediaBox [0 0 792.00 612.00]"));
        app.on_key(ctrl(KeyCode::Char('z')));
        assert!(!final_sect(&app).contains("landscape"));
        let pdf = app_pdf(&app);
        assert!(!pdf.contains("792.00 612.00"), "portrait only");
        let saved = save_and_reload(&mut app, "sect-undo");
        assert!(
            !saved.sect_pr().contains("landscape"),
            "{}",
            saved.sect_pr()
        );
    }

    #[test]
    fn pdf_export_while_editing_a_header_prints_the_body_and_the_live_header() {
        let mut app = app_with(&["body"]);
        app.run_act(ribbon::Act::EditHeader);
        for c in "Live".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert!(app.hf_edit.is_some());
        let pdf = app_pdf(&app);
        assert!(pdf.contains("(body) Tj"), "the body is printed");
        assert!(pdf.contains("(Live) Tj"), "with the header being edited");
    }

    #[test]
    fn header_hyperlinks_resolve_through_the_headers_own_relationships() {
        const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
        const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
        let content_types = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#;
        let root_rels = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/officeDocument" Target="word/document.xml"/></Relationships>"#
        );
        // rId1 means one thing to the document and another to the header.
        let document_rels = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/hyperlink" Target="https://wrong.example/" TargetMode="External"/><Relationship Id="rIdH" Type="{REL}/header" Target="header1.xml"/></Relationships>"#
        );
        let header_rels = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{REL}/hyperlink" Target="https://example.com/" TargetMode="External"/></Relationships>"#
        );
        let document = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="{W}" xmlns:r="{REL}"><w:body><w:p><w:r><w:t>body</w:t></w:r></w:p><w:sectPr><w:headerReference w:type="default" r:id="rIdH"/></w:sectPr></w:body></w:document>"#
        );
        let header = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><w:hdr xmlns:w="{W}" xmlns:r="{REL}"><w:p><w:hyperlink r:id="rId1"><w:r><w:t>site</w:t></w:r></w:hyperlink></w:p></w:hdr>"#
        );
        let bytes = docxcore::zipwrite::write_zip(&[
            (
                "[Content_Types].xml".to_string(),
                content_types.as_bytes().to_vec(),
            ),
            ("_rels/.rels".to_string(), root_rels.into_bytes()),
            ("word/document.xml".to_string(), document.into_bytes()),
            (
                "word/_rels/document.xml.rels".to_string(),
                document_rels.into_bytes(),
            ),
            ("word/header1.xml".to_string(), header.into_bytes()),
            (
                "word/_rels/header1.xml.rels".to_string(),
                header_rels.into_bytes(),
            ),
        ]);
        let app = App::new(load_package(&bytes).unwrap(), "links.docx", false);
        let pdf = app_pdf(&app);
        assert!(
            pdf.contains("/URI (https://example.com/)"),
            "PDF link target"
        );
        assert!(!pdf.contains("wrong.example"));
        // The page view's copy resolves the same way.
        let Some(Block::Paragraph(p)) = app.headers.default.first() else {
            panic!("header paragraph");
        };
        let Some(Inline::Hyperlink(h)) = p.content.first() else {
            panic!("header hyperlink: {:?}", p.content);
        };
        assert_eq!(h.target.as_deref(), Some("https://example.com/"));
    }

    #[test]
    fn a_section_break_on_a_new_document_names_its_page_size() {
        let mut app = app_with(&["first", "second"]);
        app.insert_section(true);
        let Some(Block::Paragraph(p)) = app.editor.doc.body.first() else {
            panic!("first paragraph");
        };
        let sect = p.props.section_break.as_deref().expect("a section break");
        assert!(
            sect.contains(r#"<w:pgSz w:w="12240" w:h="15840"/>"#),
            "{sect}"
        );
        assert_eq!(
            with_page_size("<w:sectPr/>"),
            format!(
                "<w:sectPr>{}</w:sectPr>",
                r#"<w:pgSz w:w="12240" w:h="15840"/>"#
            )
        );
        assert!(orient_sectpr("<w:sectPr/>", true).contains(r#"w:orient="landscape""#));
    }

    #[test]
    fn protected_find_replace_and_cross_format_save_as_are_rejected_preflight() {
        let mut app = app_with(&["x y x"]);
        protect(&mut app, ProtectionEditMode::ReadOnly, false);
        app.on_key(ctrl(KeyCode::Char('f')));
        app.on_key(key(KeyCode::Char('x')));
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Char('Z')));
        app.on_key(ctrl(KeyCode::Char('a')));
        assert_eq!(first_line(&app), "x y x");
        assert!(!app.modified);

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("docxy-protected-save-as-{nonce}"));
        let target = dir.join("blocked.md");
        app.commit_save_as(dir, "blocked.md".to_string());
        assert!(!target.exists(), "denied Save As wrote an output file");
        assert_eq!(app.path, "test.docx");
        assert_eq!(app.format, DocFormat::Docx);
        assert!(!app.modified);
        assert_eq!(
            app.status.as_deref(),
            Some("Edit blocked: the document is protected read-only.")
        );
    }

    #[test]
    fn browsing_the_backstage_never_touches_the_document() {
        let mut app = app_with(&["original text"]);
        app.open_backstage();
        // Navigate the file list (which updates the preview) and back out.
        for _ in 0..6 {
            app.on_key(key(KeyCode::Down));
        }
        app.on_key(key(KeyCode::Up));
        app.on_key(key(KeyCode::Esc));
        assert!(app.backstage.is_none());
        // The open document is untouched — preview must not replace it.
        assert_eq!(first_line(&app), "original text");
    }

    #[test]
    fn save_as_writes_to_typed_name_and_retargets() {
        let tmp = std::env::temp_dir().join("docxy_save_as");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let mut app = app_with(&["hello world"]);
        app.path = tmp.join("orig.docx").to_string_lossy().into_owned();
        app.open_backstage();
        // pick Save As from the menu and activate it
        if let Some(b) = app.backstage.as_mut() {
            b.item = backstage::Item::SaveAs;
        }
        app.on_key(key(KeyCode::Enter));
        assert_eq!(
            app.backstage.as_ref().unwrap().pane,
            backstage::Pane::SaveAs
        );
        // it prefilled the current basename
        assert_eq!(app.backstage.as_ref().unwrap().name_input, "orig.docx");
        // retype a new name and save (drop the extension to check it's added)
        if let Some(b) = app.backstage.as_mut() {
            b.name_input = "copy".to_string();
        }
        app.on_key(key(KeyCode::Enter));
        let out = tmp.join("copy.docx");
        assert!(out.exists(), "save-as did not write the file");
        // the app is now editing the new file and the dialog closed
        assert!(app.backstage.is_none());
        assert!(app.path.ends_with("copy.docx"));
        assert!(!app.modified);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn save_as_converts_docx_to_markdown_and_back() {
        let tmp = std::env::temp_dir().join("docxy_md_convert");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // A .docx with a heading, exported to Markdown via Save As.
        let body = vec![Block::Paragraph(docxcore::model::Paragraph {
            props: docxcore::model::ParProps {
                heading_level: Some(1),
                style_id: Some("Heading1".to_string()),
                ..Default::default()
            },
            content: vec![docxcore::model::Inline::Run(docxcore::model::Run {
                text: "My Title".to_string(),
                props: Default::default(),
            })],
        })];
        let mut app = App::new(new_package(Document { body }), "src.docx", false);
        app.backstage = Some(backstage::Backstage::open(tmp.clone(), app.extensions()));
        if let Some(b) = app.backstage.as_mut() {
            b.name_input = "out.md".to_string();
        }
        let (dir, name) = {
            let b = app.backstage.as_ref().unwrap();
            (b.dir.clone(), b.name_input.clone())
        };
        app.commit_save_as(dir, name);
        let md_path = tmp.join("out.md");
        assert!(md_path.exists(), "markdown file not written");
        let md = std::fs::read_to_string(&md_path).unwrap();
        assert!(md.contains("# My Title"), "{md}");
        // The app rebound to the Markdown file.
        assert_eq!(app.format, DocFormat::Markdown);

        // Now reload that .md from disk and export it back to .docx.
        let Input {
            pkg, format: fmt, ..
        } = load_input(&md_path.to_string_lossy()).expect("load .md");
        assert_eq!(fmt, DocFormat::Markdown);
        let mut app2 = App::new(pkg, &md_path.to_string_lossy(), false);
        app2.backstage = Some(backstage::Backstage::open(tmp.clone(), app2.extensions()));
        if let Some(b) = app2.backstage.as_mut() {
            b.name_input = "roundtrip.docx".to_string();
        }
        let (dir, name) = {
            let b = app2.backstage.as_ref().unwrap();
            (b.dir.clone(), b.name_input.clone())
        };
        app2.commit_save_as(dir, name);
        let docx_path = tmp.join("roundtrip.docx");
        assert!(docx_path.exists(), "docx not written");
        let back = load_input(&docx_path.to_string_lossy())
            .expect("load .docx")
            .pkg;
        match &back.document.body[0] {
            Block::Paragraph(p) => assert_eq!(p.props.heading_level, Some(1)),
            _ => panic!("expected heading after round-trip"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn one_click_on_save_as_engages_and_prefills_the_name() {
        let mut app = app_with(&["x"]);
        app.path = "report.docx".to_string();
        app.open_backstage();
        let row = backstage::ITEMS
            .iter()
            .position(|i| *i == backstage::Item::SaveAs)
            .unwrap();
        app.bs_mouse(3, 1 + row as u16); // a single click in the menu column
        let b = app.backstage.as_ref().unwrap();
        assert_eq!(b.pane, backstage::Pane::SaveAs); // editable right away
        assert_eq!(b.name_input, "report.docx"); // name shown immediately
        // a stray single click on New must NOT discard the document
        let nrow = backstage::ITEMS
            .iter()
            .position(|i| *i == backstage::Item::New)
            .unwrap();
        app.bs_mouse(3, 1 + nrow as u16);
        assert!(app.backstage.is_some()); // still in the backstage, nothing reset
    }

    #[test]
    fn review_tab_present_and_toggle_flips_the_panel() {
        let mut app = app_with(&["body text"]);
        // The ribbon has a Review tab after File, Home, Styles, Insert and Design.
        assert_eq!(app.ribbon.tab_label(5), Some("Review"));
        // Switching to it activates the Review ribbon.
        app.ribbon.set_active(5);
        assert_eq!(app.ribbon.active_tab(), 5);
        // The Comments toggle flips the side-panel flag.
        assert!(!app.show_comments);
        app.run_act(ribbon::Act::ToggleComments);
        assert!(app.show_comments);
        app.run_act(ribbon::Act::ToggleComments);
        assert!(!app.show_comments);
    }

    #[test]
    fn black_text_uses_terminal_default_foreground() {
        use docxcore::render::{Color as DC, Line as DL, Span as DS, Style as DST};
        let line = DL {
            spans: vec![
                DS {
                    text: "blk".into(),
                    style: DST {
                        color: Some(DC::Black),
                        ..Default::default()
                    },
                    link: None,
                },
                DS {
                    text: "red".into(),
                    style: DST {
                        color: Some(DC::Red),
                        ..Default::default()
                    },
                    link: None,
                },
            ],
        };
        let rl = doc_line_to_ratatui(&line);
        // black → no fg (terminal default, so it's visible on a dark background)
        assert_eq!(rl.spans[0].style.fg, None);
        // other colors still map
        assert_eq!(rl.spans[1].style.fg, Some(Color::Red));
    }

    #[test]
    fn view_ribbon_actions_toggle_their_state() {
        let mut app = app_with(&["heading"]);
        assert_eq!(app.ribbon.tab_label(6), Some("View"));
        app.run_act(ribbon::Act::PrintLayout);
        assert!(app.page_view);
        app.run_act(ribbon::Act::ReadMode);
        assert!(!app.page_view);
        assert!(!app.light_page);
        app.run_act(ribbon::Act::DarkMode);
        assert!(app.light_page);
        app.run_act(ribbon::Act::ToggleRuler);
        assert!(app.show_ruler);
        app.run_act(ribbon::Act::ToggleNav);
        assert!(app.show_nav);
    }

    #[test]
    fn backstage_tab_strip_leaves_the_panel() {
        let mut app = app_with(&["x"]);
        let click0 = |app: &mut App, col: u16| {
            app.on_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: col,
                row: 0,
                modifiers: KeyModifiers::NONE,
            });
        };
        // A click on the File header (row 0) closes the panel back to the document.
        app.open_backstage();
        click0(&mut app, 3); // inside "File"
        assert!(
            app.backstage.is_none(),
            "File header should close the panel"
        );
        // Clicking the strip's leading padding (left of File) also leaves it, so
        // the tiny header isn't a pixel-perfect target.
        app.open_backstage();
        click0(&mut app, 0);
        assert!(
            app.backstage.is_none(),
            "strip padding should close the panel"
        );
        // Clicking another tab switches to it and opens its ribbon.
        app.open_backstage();
        app.backstage_tab_click(1); // Home
        assert!(app.backstage.is_none());
        assert_eq!(app.ribbon.active_tab(), 1);
        assert!(app.ribbon_open);
    }

    #[test]
    fn backstage_tab_strip_renders_over_backstagecore_draw() {
        // `backstagecore::draw` clears its *entire* passed area (row 0 included)
        // before rendering the menu/content below it, so the app's own tab-strip
        // paint must happen after that call, not before — otherwise it would be
        // wiped. Guard the actual rendered buffer, not just the click routing.
        let mut app = app_with(&["x"]);
        app.open_backstage();
        // Wide enough that the trailing hint isn't clipped by the frame edge.
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(
            text.contains("File"),
            "the ribbon tab strip must still be visible on row 0: {text}"
        );
        assert!(
            text.contains("click a tab or Esc to leave"),
            "the row-0 hint must still be visible: {text}"
        );
    }

    #[test]
    fn auto_hide_ribbon_pins_and_collapses() {
        let mut app = app_with(&["heading"]);
        // Default: ribbon stays pinned open once expanded — a document click
        // moves focus out but leaves it open.
        app.ribbon_open = true;
        assert!(!app.auto_hide_ribbon);
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: app.doc_y0 + 1,
            modifiers: KeyModifiers::NONE,
        });
        assert!(
            app.ribbon_open,
            "ribbon should stay pinned when auto-hide off"
        );
        // Enabling auto-hide collapses it immediately, and a later document
        // click keeps it collapsed.
        app.ribbon_open = true;
        app.run_act(ribbon::Act::AutoHideRibbon);
        assert!(app.auto_hide_ribbon);
        assert!(!app.ribbon_open, "enabling auto-hide collapses on the spot");
        app.ribbon_open = true;
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: app.doc_y0 + 1,
            modifiers: KeyModifiers::NONE,
        });
        assert!(!app.ribbon_open, "auto-hide collapses on document click");
    }

    #[test]
    fn wrap_str_wraps_words_and_hard_breaks() {
        assert_eq!(wrap_str("hello world foo", 11), vec!["hello world", "foo"]);
        assert_eq!(wrap_str("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap_str("", 5), vec![String::new()]);
    }

    #[test]
    fn save_as_save_button_click_writes_the_file() {
        let tmp = std::env::temp_dir().join("docxy_save_btn");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let mut app = app_with(&["body"]);
        app.path = tmp.join("orig.docx").to_string_lossy().into_owned();
        app.backstage = Some(backstage::Backstage::open(tmp.clone(), app.extensions()));
        if let Some(b) = app.backstage.as_mut() {
            b.item = backstage::Item::SaveAs;
        }
        app.on_key(key(KeyCode::Enter));
        if let Some(b) = app.backstage.as_mut() {
            b.name_input = "clicked".to_string();
        }
        // Draw once (80x24) so the Save button's real geometry is recorded:
        // the rightmost 10 cells of the bottom 3 rows of the name band.
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        app.bs_mouse(74, 22);
        assert!(tmp.join("clicked.docx").exists());
        assert!(app.backstage.is_none());
        assert!(app.path.ends_with("clicked.docx"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn save_as_clicks_switch_focus_and_place_caret() {
        let tmp = std::env::temp_dir().join("docxy_saveas_focus");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        let mut app = app_with(&["x"]);
        app.path = tmp.join("report.docx").to_string_lossy().into_owned();
        app.backstage = Some(backstage::Backstage::open(tmp.clone(), app.extensions()));
        if let Some(b) = app.backstage.as_mut() {
            b.item = backstage::Item::SaveAs;
        }
        app.on_key(key(KeyCode::Enter));
        assert!(app.backstage.as_ref().unwrap().name_focus);
        // Draw once (80x23) so the name box geometry matches: name_top =
        // height - 3 = 20; the name box's first char sits at a fixed x0 = 16.
        let mut term = Terminal::new(TestBackend::new(80, 23)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        // click inside the name box, 3 cells into the text -> caret at char 3
        app.bs_mouse(16 + 3, 21);
        let b = app.backstage.as_ref().unwrap();
        assert!(b.name_focus);
        assert_eq!(b.name_cursor, 3);
        // click in the folder list -> focus the browser, deactivate the field
        app.bs_mouse(20, 2);
        assert!(!app.backstage.as_ref().unwrap().name_focus);
        // with the browser focused, typing must NOT edit the file name
        let before = app.backstage.as_ref().unwrap().name_input.clone();
        app.on_key(key(KeyCode::Char('Z')));
        assert_eq!(app.backstage.as_ref().unwrap().name_input, before);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn save_as_arrows_edit_the_name_not_the_file_list() {
        let mut app = app_with(&["x"]);
        app.path = "report.docx".to_string();
        app.open_backstage();
        if let Some(b) = app.backstage.as_mut() {
            b.item = backstage::Item::SaveAs;
        }
        app.on_key(key(KeyCode::Enter));
        let sel_before = app.backstage.as_ref().unwrap().sel;
        // caret starts at end; Left then type inserts mid-string
        app.on_key(key(KeyCode::Left)); // before the 'x' of ".docx"
        app.on_key(key(KeyCode::Left));
        app.on_key(key(KeyCode::Left));
        app.on_key(key(KeyCode::Left));
        app.on_key(key(KeyCode::Left)); // now before ".docx" -> after "report"
        app.on_key(key(KeyCode::Char('-')));
        app.on_key(key(KeyCode::Char('v')));
        app.on_key(key(KeyCode::Char('2')));
        let b = app.backstage.as_ref().unwrap();
        assert_eq!(b.name_input, "report-v2.docx");
        // the file list selection never moved
        assert_eq!(b.sel, sel_before);
        // Up/Down are inert in the dialog
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.backstage.as_ref().unwrap().sel, sel_before);
    }

    #[test]
    fn backstage_opens_on_the_menu_and_down_reaches_exit() {
        let mut app = app_with(&["doc"]);
        app.open_backstage();
        // lands on the vertical menu, not the file list
        assert_eq!(app.backstage.as_ref().unwrap().pane, backstage::Pane::Menu);
        // Down walks the menu straight to Exit
        for _ in 0..backstage::ITEMS.len() {
            if app.backstage.as_ref().unwrap().item == backstage::Item::Exit {
                break;
            }
            app.on_key(key(KeyCode::Down));
            // focus stays on the menu the whole way down
            assert_eq!(app.backstage.as_ref().unwrap().pane, backstage::Pane::Menu);
        }
        assert_eq!(app.backstage.as_ref().unwrap().item, backstage::Item::Exit);
        // Enter on Exit shows the confirm modal (and closes the backstage)
        app.on_key(key(KeyCode::Enter));
        assert!(app.confirm.is_some());
        assert!(app.backstage.is_none());
    }

    #[test]
    fn export_pdf_asks_before_overwriting() {
        let tmp = std::env::temp_dir().join("docxy_pdf_overwrite");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let docx = tmp.join("note.docx");
        let pdf = tmp.join("note.pdf");

        // No PDF yet → export writes straight away, no prompt.
        let mut app = app_with(&["hello"]);
        app.path = docx.to_string_lossy().into_owned();
        app.export_pdf();
        assert!(app.confirm.is_none(), "no prompt when the PDF is new");
        assert!(pdf.exists(), "PDF written on first export");

        // PDF now exists → a second export asks first, defaulting to No.
        app.status = None;
        app.export_pdf();
        let c = app.confirm.as_ref().expect("overwrite prompt shown");
        assert!(
            !c.yes_selected(),
            "default is No for a destructive overwrite"
        );
        assert!(app.status.is_none(), "nothing written until confirmed");

        // Confirming (press 'y') overwrites the file.
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.confirm.is_none());
        assert!(app.status.as_deref().unwrap().starts_with("exported"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn file_menu_exit_asks_to_confirm_then_quits() {
        let mut app = app_with(&["doc"]);
        app.open_backstage();
        if let Some(b) = app.backstage.as_mut() {
            b.item = backstage::Item::Exit;
            b.pane = backstage::Pane::Menu;
        }
        // Activating Exit opens a Yes/No modal instead of quitting outright.
        let quit = app.on_key(key(KeyCode::Enter));
        assert!(!quit);
        assert!(!app.quit_requested);
        assert!(app.backstage.is_none());
        assert!(app.confirm.is_some());
        assert!(
            app.confirm.as_ref().unwrap().yes_selected(),
            "Yes is the default"
        );
        // Esc dismisses without quitting regardless of the selection.
        app.on_key(key(KeyCode::Esc));
        assert!(app.confirm.is_none());
        assert!(!app.quit_requested);
        // Reopen and confirm with 'y'.
        app.open_backstage();
        if let Some(b) = app.backstage.as_mut() {
            b.item = backstage::Item::Exit;
        }
        app.on_key(key(KeyCode::Enter));
        let quit = app.on_key(key(KeyCode::Char('y')));
        assert!(quit);
        assert!(app.quit_requested);
    }

    #[test]
    fn single_click_exit_opens_confirm_and_new_stays_guarded() {
        // Exit is index 7 in the menu, drawn at screen row 1 + idx.
        let exit_row = 1 + backstage::ITEMS
            .iter()
            .position(|i| *i == backstage::Item::Exit)
            .unwrap() as u16;
        let mut app = app_with(&["doc"]);
        app.open_backstage();
        // One click on Exit goes straight to the confirm modal — no second click.
        app.bs_mouse(3, exit_row);
        assert!(app.backstage.is_none(), "Exit closes the backstage");
        assert!(app.confirm.is_some(), "Exit raises the confirm dialog");

        // New is guarded: a first click only selects it (discarding work needs a
        // confirming second click).
        let new_row = 1 + backstage::ITEMS
            .iter()
            .position(|i| *i == backstage::Item::New)
            .unwrap() as u16;
        let mut app = app_with(&["doc"]);
        app.open_backstage();
        app.bs_mouse(3, new_row);
        assert!(app.backstage.is_some(), "first New click only selects");
        assert_eq!(app.backstage.as_ref().unwrap().item, backstage::Item::New);
        // Second click on the already-selected New actually starts a new doc.
        app.bs_mouse(3, new_row);
        assert!(app.backstage.is_none());
        assert!(app.path.ends_with("untitled.docx"));
    }

    #[test]
    fn preview_pane_scrolls_and_clamps() {
        let mut app = app_with(&["doc"]);
        app.open_backstage();
        // Draw once (80x8) so the backstage records a preview height of 5
        // (area.height - 3), matching the layout `backstagecore::draw` computes.
        let mut term = Terminal::new(TestBackend::new(80, 8)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let b = app.backstage.as_mut().unwrap();
        b.preview = (0..20).map(|i| format!("line {i}")).collect();
        b.pane = backstage::Pane::Preview;
        b.scroll_preview(3);
        assert_eq!(app.backstage.as_ref().unwrap().preview_scroll, 3);
        // clamps at the bottom: max = len(20) - height(5) = 15
        app.backstage.as_mut().unwrap().scroll_preview(1000);
        assert_eq!(app.backstage.as_ref().unwrap().preview_scroll, 15);
        // and at the top
        app.backstage.as_mut().unwrap().scroll_preview(-1000);
        assert_eq!(app.backstage.as_ref().unwrap().preview_scroll, 0);
    }

    #[test]
    fn clicking_a_file_row_selects_then_opens_it() {
        let tmp = std::env::temp_dir().join("docxy_click_pick");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // a real, openable doc so the second click can open it
        let pkg = new_package(Document {
            body: vec![Block::Paragraph(docxcore::model::Paragraph::default())],
        });
        std::fs::write(tmp.join("hello.docx"), save_package(&pkg)).unwrap();
        let mut app = app_with(&["start"]);
        app.backstage = Some(backstage::Backstage::open(tmp.clone(), app.extensions()));
        // find the row of hello.docx (entries are folders-first, no "..")
        let row = app
            .backstage
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .position(|e| e.name == "hello.docx")
            .unwrap();
        let y = 2 + row as u16; // first entry is at screen y=2
        // first click selects it
        app.bs_mouse(20, y);
        assert_eq!(app.backstage.as_ref().unwrap().sel, row);
        assert_eq!(
            app.backstage.as_ref().unwrap().pane,
            backstage::Pane::Browser
        );
        // second click on the same row opens it and closes the backstage
        app.bs_mouse(20, y);
        assert!(app.backstage.is_none());
        assert!(app.path.ends_with("hello.docx"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn right_steps_into_preview_then_left_returns() {
        let mut app = app_with(&["doc"]);
        app.open_backstage();
        if let Some(b) = app.backstage.as_mut() {
            b.preview = vec!["x".to_string()];
            b.pane = backstage::Pane::Browser;
        }
        app.on_key(key(KeyCode::Right));
        assert_eq!(
            app.backstage.as_ref().unwrap().pane,
            backstage::Pane::Preview
        );
        app.on_key(key(KeyCode::Left));
        assert_eq!(
            app.backstage.as_ref().unwrap().pane,
            backstage::Pane::Browser
        );
    }

    #[test]
    fn backstage_opens_runs_new_and_closes() {
        let mut app = app_with(&["hello world"]);
        // File backstage opens and is modal.
        app.open_backstage();
        assert!(app.backstage.is_some());
        // Selecting "New" replaces the document and closes the backstage.
        if let Some(b) = app.backstage.as_mut() {
            b.item = backstage::Item::New;
        }
        app.on_key(key(KeyCode::Enter));
        assert!(app.backstage.is_none());
        assert_eq!(app.path, "untitled.docx");
        // Esc closes the backstage without quitting the app.
        app.open_backstage();
        assert!(!app.on_key(key(KeyCode::Esc)));
        assert!(app.backstage.is_none());
    }

    #[test]
    fn ribbon_focus_navigation_and_actions() {
        let mut app = app_with(&["hello world"]);
        // F9 expands and focuses the tabs.
        app.on_key(key(KeyCode::F(9)));
        assert!(app.ribbon_open);
        assert!(matches!(app.ribbon_focus, ribbon::Focus::Tab(_)));
        // Down drops into the button body.
        app.on_key(key(KeyCode::Down));
        assert!(matches!(app.ribbon_focus, ribbon::Focus::Button(_)));
        // A dimmed action reports "not implemented".
        app.run_act(ribbon::Act::Todo("Bullets"));
        assert!(
            app.status
                .as_deref()
                .unwrap_or("")
                .contains("not implemented")
        );
        // A live action applies to the document.
        app.editor.select_all();
        app.run_act(ribbon::Act::Bold);
        assert!(app.modified, "Bold should mark the document modified");
        // Esc leaves the ribbon (must not quit the app) and collapses it.
        assert!(!app.on_key(key(KeyCode::Esc)));
        assert!(!app.ribbon_open);
        assert_eq!(app.ribbon_focus, ribbon::Focus::None);
    }

    fn para_text(app: &App, i: usize) -> String {
        match &app.editor.doc.body[i] {
            Block::Paragraph(p) => p.plain_text(),
            _ => String::new(),
        }
    }

    #[test]
    fn insert_tab_button_inserts_a_horizontal_line() {
        let mut app = app_with(&["hello"]);
        // The ribbon has an Insert tab between Styles and Review.
        assert_eq!(app.ribbon.tab_label(3), Some("Insert"));
        app.run_act(ribbon::Act::HorizontalLine);
        match &app.editor.doc.body[0] {
            Block::Paragraph(p) => assert_eq!(
                p.props.borders.bottom,
                Some(docxcore::model::BorderKind::Single)
            ),
            _ => panic!("expected a paragraph"),
        }
        // a fresh, border-free paragraph follows for the caret
        match &app.editor.doc.body[1] {
            Block::Paragraph(p) => assert_eq!(p.props.borders.bottom, None),
            _ => panic!(),
        }
        assert!(app.modified);
    }

    #[test]
    fn comment_navigation_selects_jumps_and_wraps() {
        let mk = |id: &str, author: &str, quoted: &str| docxcore::comments::Comment {
            id: id.to_string(),
            author: author.to_string(),
            quoted: quoted.to_string(),
            text: "a note".to_string(),
            ..Default::default()
        };
        let mut app = app_with(&["alpha beta gamma delta"]);
        app.comments = vec![mk("1", "Ann", "beta"), mk("2", "Bob", "delta")];
        // First Next lands on the first comment, shows the panel, jumps the caret.
        app.run_act(ribbon::Act::NextComment);
        assert!(app.show_comments);
        assert!(app.comment_active);
        assert_eq!(app.comment_sel, 0);
        assert!(
            app.editor.has_selection(),
            "caret jumped to the anchored text"
        );
        // Next advances, then wraps.
        app.run_act(ribbon::Act::NextComment);
        assert_eq!(app.comment_sel, 1);
        app.run_act(ribbon::Act::NextComment);
        assert_eq!(app.comment_sel, 0);
        // Prev wraps to the last.
        app.run_act(ribbon::Act::PrevComment);
        assert_eq!(app.comment_sel, 1);
    }

    #[test]
    fn new_then_delete_comment() {
        let mut app = app_with(&["hello world"]);
        app.editor.select_all();
        app.run_act(ribbon::Act::NewComment);
        assert!(app.comment_input.is_some(), "entered comment-text mode");
        for c in "a note".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
        assert!(app.comment_input.is_none());
        assert_eq!(app.comments.len(), 1);
        assert_eq!(app.comments[0].text, "a note");
        // markers wrap the selection in the model
        let marked = |app: &App, needle: &str| match &app.editor.doc.body[0] {
            Block::Paragraph(p) => p
                .content
                .iter()
                .any(|i| matches!(i, Inline::Raw(r) if r.contains(needle))),
            _ => false,
        };
        assert!(marked(&app, "commentRangeStart"));
        assert!(marked(&app, "commentReference"));
        // delete removes the comment and its markers
        app.run_act(ribbon::Act::DeleteComment);
        assert!(app.comments.is_empty());
        assert!(!marked(&app, "commentRange"));
        assert!(!marked(&app, "commentReference"));
    }

    /// Select all of `app` and add a comment holding `text`, as the keys do.
    fn add_comment_by_keys(app: &mut App, text: &str) {
        app.editor.select_all();
        app.run_act(ribbon::Act::NewComment);
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        app.on_key(key(KeyCode::Enter));
    }

    /// Save `app` to a fresh `t.docx` under the temp dir; returns the path.
    fn save_to_temp(app: &mut App, tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("docxy-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.docx");
        app.path = path.to_string_lossy().into_owned();
        app.save();
        path
    }

    /// The saved document.xml and comments.xml (empty when there is none).
    fn saved_parts(path: &std::path::Path) -> (String, String) {
        let pkg = load_package(&std::fs::read(path).expect("saved")).unwrap();
        let text = |name: &str| pkg.part_text(name).unwrap_or_default();
        (text("word/document.xml"), text("word/comments.xml"))
    }

    fn is_utc_date_time(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() == 20
            && b.iter().enumerate().all(|(i, c)| match i {
                4 | 7 => *c == b'-',
                10 => *c == b'T',
                13 | 16 => *c == b':',
                19 => *c == b'Z',
                _ => c.is_ascii_digit(),
            })
    }

    /// #620: a new comment is the reviewer's, not the document creator's,
    /// and carries the time it was made.
    #[test]
    fn new_comment_author_is_not_document_creator_and_date_is_set() {
        let mut app = app_with(&["The quick brown fox."]);
        app.field_ctx.props.author = "Document Creator".to_string();
        add_comment_by_keys(&mut app, "Colour?");
        let path = save_to_temp(&mut app, "cmt-author");
        let (_, comments) = saved_parts(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let parsed = docxcore::comments::parse_comments_xml(&comments);
        assert_eq!(parsed.len(), 1, "{comments}");
        let c = &parsed[0];
        let expected = os_user_name().unwrap_or_else(|| DEFAULT_AUTHOR.to_string());
        assert_eq!(c.author, expected);
        assert_ne!(c.author, "Document Creator");
        assert_eq!(c.initials, docxcore::comments::initials(&expected));
        assert!(is_utc_date_time(&c.date), "w:date {:?}", c.date);
    }

    /// #620: one undo takes the whole comment, markers and comments.xml
    /// entry alike, and redo brings all of it back.
    #[test]
    fn undo_new_comment_then_save_has_no_comment() {
        let mut app = app_with(&["The quick brown fox."]);
        add_comment_by_keys(&mut app, "Colour?");
        app.on_key(ctrl(KeyCode::Char('z')));
        assert!(app.comments.is_empty(), "the panel drops it");
        let path = save_to_temp(&mut app, "cmt-undo");
        let (doc, comments) = saved_parts(&path);
        assert!(!doc.contains("w:id=\"1\""), "{doc}");
        assert!(!comments.contains("Colour?"), "{comments}");

        app.on_key(ctrl(KeyCode::Char('y')));
        assert_eq!(app.comments.len(), 1, "the panel shows it again");
        app.save();
        let (doc, comments) = saved_parts(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert_eq!(doc.matches("w:id=\"1\"").count(), 3, "{doc}");
        assert!(comments.contains("Colour?"), "{comments}");
    }

    /// #620: every save reloads `pkg` from the bytes written, so a comment
    /// saved once is in it; an undo after that save still drops it.
    #[test]
    fn add_save_undo_save_has_no_comment() {
        let mut app = app_with(&["The quick brown fox."]);
        add_comment_by_keys(&mut app, "Colour?");
        let path = save_to_temp(&mut app, "cmt-save-undo");
        assert!(saved_parts(&path).1.contains("Colour?"));
        app.on_key(ctrl(KeyCode::Char('z')));
        app.save();
        let (doc, comments) = saved_parts(&path);
        assert!(!doc.contains("w:id=\"1\""), "{doc}");
        assert!(!comments.contains("Colour?"), "{comments}");
        // And redo after that save writes it again.
        app.on_key(ctrl(KeyCode::Char('y')));
        app.save();
        let (_, comments) = saved_parts(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(comments.contains("Colour?"), "{comments}");
    }

    /// FIX r2 #1: deleting a loaded comment and undoing the delete puts its
    /// markers back (and, since #971, its record); a new comment must not
    /// take that id, or it would stay live after its own undo.
    #[test]
    fn a_new_comment_never_takes_an_id_whose_markers_are_in_the_body() {
        let mut app = app_with_loaded_comments();
        assert_eq!(comment_ids(&app), ["1", "2"]);
        app.comment_sel = 1;
        app.run_act(ribbon::Act::DeleteComment);
        assert_eq!(comment_ids(&app), ["1"]);
        app.on_key(ctrl(KeyCode::Char('z')));
        assert_eq!(comment_ids(&app), ["1", "2"], "the undo brings it back");
        add_comment_by_keys(&mut app, "Colour?");
        assert_eq!(comment_ids(&app), ["1", "2", "3"], "a fresh id, not 2");
        app.on_key(ctrl(KeyCode::Char('z')));
        assert_eq!(comment_ids(&app), ["1", "2"], "its undo takes it");
    }

    /// The ids the comments panel lists, in order.
    fn comment_ids(app: &App) -> Vec<String> {
        app.comments.iter().map(|c| c.id.clone()).collect()
    }

    /// The ids of the `<w:comment>`s in a comments.xml, in order.
    fn saved_comment_ids(comments_xml: &str) -> Vec<String> {
        docxcore::comments::parse_comments_xml(comments_xml)
            .into_iter()
            .map(|c| c.id)
            .collect()
    }

    /// A loaded comment Word could have written: two paragraphs, a bold
    /// run, a `w14:paraId`. A re-creation from its parsed record would lose
    /// all of that, so finding it byte-for-byte in a save proves its XML
    /// was kept (#971).
    const RICH_COMMENT: &str = "<w:comment w:id=\"1\" w:author=\"Ann\" w:initials=\"A\" \
        w:date=\"2020-01-02T03:04:05Z\" w14:paraId=\"1A2B\"><w:p><w:r><w:rPr><w:b/></w:rPr>\
        <w:t>Bold</w:t></w:r></w:p><w:p><w:r><w:t>second</w:t></w:r></w:p></w:comment>";

    /// "The quick brown fox." with loaded comments `1..=n` around it, 1
    /// written as [`RICH_COMMENT`].
    fn app_with_rich_comments(n: i32) -> App {
        let mut ed = Editor::new(Document {
            body: vec![Block::Paragraph(MPara {
                props: ParProps::default(),
                content: vec![Inline::Run(Run {
                    text: "The quick brown fox.".into(),
                    props: RunProps::default(),
                })],
            })],
        });
        for id in 1..=n {
            ed.select_all();
            assert!(ed.add_comment(&id.to_string()));
        }
        let mut pkg = new_package(ed.doc);
        pkg.insert_comment_xml(RICH_COMMENT);
        for id in 2..=n {
            pkg.add_comment(
                id,
                "Bob",
                "B",
                "2020-01-02T03:04:05Z",
                &format!("note {id}"),
            );
        }
        let mut app = App::new(pkg, "test.docx", false);
        app.os_clip = None;
        app
    }

    /// #971 A6: Delete Comment, then undo, lists a loaded comment again and
    /// saves its original XML.
    #[test]
    fn undo_delete_loaded_comment_restores_its_xml() {
        let mut app = app_with_rich_comments(2);
        assert_eq!(comment_ids(&app), ["1", "2"]);
        app.comment_sel = 0;
        app.run_act(ribbon::Act::DeleteComment);
        assert_eq!(comment_ids(&app), ["2"]);
        app.on_key(ctrl(KeyCode::Char('z')));
        assert_eq!(comment_ids(&app), ["1", "2"]);
        let path = save_to_temp(&mut app, "cmt-undo-delete");
        let (doc, comments) = saved_parts(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert_eq!(doc.matches("w:id=\"1\"").count(), 3, "{doc}");
        assert!(comments.contains(RICH_COMMENT), "{comments}");
    }

    fn saved_pkg(path: &std::path::Path) -> docxcore::package::Package {
        load_package(&std::fs::read(path).expect("saved")).unwrap()
    }

    fn saved_resolved(path: &std::path::Path) -> Vec<(String, bool)> {
        docxcore::comments::parse_comments(&saved_pkg(path))
            .into_iter()
            .map(|c| (c.id, c.resolved))
            .collect()
    }

    /// #621: Resolve toggles the selected comment and saves `w15:done`.
    #[test]
    fn resolve_and_reopen_comment_saves_done_state() {
        let mut app = app_with_rich_comments(2);
        app.comment_sel = 1;
        app.run_act(ribbon::Act::ResolveComment);
        assert!(app.comments[1].resolved && !app.comments[0].resolved);
        let path = save_to_temp(&mut app, "cmt-resolve");
        let pkg = saved_pkg(&path);
        let ext = pkg.part_text("word/commentsExtended.xml").expect("part");
        assert!(ext.contains("w15:done=\"1\""), "{ext}");
        assert_eq!(
            saved_resolved(&path),
            [("1".to_string(), false), ("2".to_string(), true)]
        );
        // The save reloaded `pkg`: Reopen writes done=0 over the entry.
        app.run_act(ribbon::Act::ResolveComment);
        assert!(!app.comments[1].resolved);
        app.save();
        assert_eq!(
            saved_resolved(&path),
            [("1".to_string(), false), ("2".to_string(), false)]
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// #621: a resolved comment loaded from a file shows resolved; and the
    /// state follows the comment through a delete and its undo.
    #[test]
    fn resolved_state_survives_delete_and_undo() {
        let mut app = app_with_rich_comments(1);
        app.run_act(ribbon::Act::ResolveComment);
        let path = save_to_temp(&mut app, "cmt-resolve-undo");
        let mut app = App::new(saved_pkg(&path), "test.docx", false);
        app.os_clip = None;
        app.path = path.to_string_lossy().into_owned();
        assert!(app.comments[0].resolved, "loaded as resolved");
        app.run_act(ribbon::Act::DeleteComment);
        app.save();
        assert!(saved_pkg(&path).part("word/commentsExtended.xml").is_none());
        app.on_key(ctrl(KeyCode::Char('z')));
        assert!(app.comments[0].resolved);
        app.save();
        assert_eq!(saved_resolved(&path), [("1".to_string(), true)]);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// #621 A2: Delete All Comments removes records and markers, keeps the
    /// anchored text, and one undo step restores both.
    #[test]
    fn delete_all_comments_is_one_undo_and_removes_every_part() {
        let mut app = app_with_rich_comments(3);
        app.comment_sel = 2;
        app.run_act(ribbon::Act::ResolveComment);
        app.run_act(ribbon::Act::DeleteAllComments);
        assert!(app.comments.is_empty());
        let path = save_to_temp(&mut app, "cmt-delete-all");
        let pkg = saved_pkg(&path);
        for part in ["word/comments.xml", "word/commentsExtended.xml"] {
            assert!(pkg.part(part).is_none(), "{part}");
        }
        let doc = pkg.part_text("word/document.xml").unwrap();
        assert!(!doc.contains("comment"), "{doc}");
        assert!(doc.contains("The quick brown fox."), "{doc}");
        app.on_key(ctrl(KeyCode::Char('z')));
        assert_eq!(comment_ids(&app), ["1", "2", "3"]);
        app.save();
        let pkg = saved_pkg(&path);
        let doc = pkg.part_text("word/document.xml").unwrap();
        assert_eq!(doc.matches("w:commentRangeStart").count(), 3, "{doc}");
        assert!(
            pkg.part_text("word/comments.xml")
                .unwrap()
                .contains(RICH_COMMENT)
        );
        assert_eq!(
            saved_resolved(&path),
            [
                ("1".to_string(), false),
                ("2".to_string(), false),
                ("3".to_string(), true)
            ]
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// #971 A7: every save reloads `pkg`, so the deleted comment's XML must
    /// outlive the save between the delete and its undo.
    #[test]
    fn delete_save_undo_save_restores_the_comment() {
        let mut app = app_with_rich_comments(2);
        app.comment_sel = 0;
        app.run_act(ribbon::Act::DeleteComment);
        let path = save_to_temp(&mut app, "cmt-delete-save-undo");
        assert_eq!(saved_comment_ids(&saved_parts(&path).1), ["2"]);
        app.on_key(ctrl(KeyCode::Char('z')));
        app.save();
        let (doc, comments) = saved_parts(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert_eq!(doc.matches("w:id=\"1\"").count(), 3, "{doc}");
        assert!(comments.contains(RICH_COMMENT), "{comments}");
    }

    /// #971 A8: a delete that stays leaves the comment and its markers out
    /// of the save, also after undo and redo.
    #[test]
    fn delete_comment_saves_without_it() {
        let mut app = app_with_rich_comments(2);
        app.comment_sel = 0;
        app.run_act(ribbon::Act::DeleteComment);
        let path = save_to_temp(&mut app, "cmt-delete-save");
        let (doc, comments) = saved_parts(&path);
        assert!(!doc.contains("w:id=\"1\""), "{doc}");
        assert_eq!(saved_comment_ids(&comments), ["2"], "{comments}");
        app.on_key(ctrl(KeyCode::Char('z')));
        app.on_key(ctrl(KeyCode::Char('y')));
        assert_eq!(comment_ids(&app), ["2"]);
        app.save();
        let (doc, comments) = saved_parts(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(!doc.contains("w:id=\"1\""), "{doc}");
        assert_eq!(saved_comment_ids(&comments), ["2"], "{comments}");
    }

    /// #971 FIX r1 m2: a comment added this session and saved, then
    /// deleted, keeps the XML the save wrote for its undo.
    #[test]
    fn delete_of_a_saved_session_comment_keeps_its_xml() {
        let mut app = app_with(&["The quick brown fox."]);
        add_comment_by_keys(&mut app, "Colour?");
        let path = save_to_temp(&mut app, "cmt-session-raw");
        let written = app.pkg.comment_xml("1").expect("saved");
        app.run_act(ribbon::Act::DeleteComment);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert_eq!(
            app.tracked_comments.get("1").and_then(|t| t.raw.as_deref()),
            Some(written.as_str())
        );
    }

    /// "The quick brown fox." with loaded comments `03` and `3` around it:
    /// one id as some producer wrote it, one that reads as the same number.
    fn app_with_03_and_3() -> App {
        let mut ed = Editor::new(Document {
            body: vec![Block::Paragraph(MPara {
                props: ParProps::default(),
                content: vec![Inline::Run(Run {
                    text: "The quick brown fox.".into(),
                    props: RunProps::default(),
                })],
            })],
        });
        for id in ["03", "3"] {
            ed.select_all();
            assert!(ed.add_comment(id));
        }
        let mut pkg = new_package(ed.doc);
        pkg.insert_comment_xml(ZERO_THREE);
        pkg.insert_comment_xml(THREE);
        let mut app = App::new(pkg, "test.docx", false);
        app.os_clip = None;
        app
    }

    const ZERO_THREE: &str = "<w:comment w:author=\"Ann\" w:id=\"03\"><w:p><w:r><w:t>oh-three</w:t></w:r></w:p></w:comment>";
    const THREE: &str =
        "<w:comment w:id=\"3\" w:author=\"Bob\"><w:p><w:r><w:t>three</w:t></w:r></w:p></w:comment>";

    /// #971 FIX r2 f4: Delete Comment of `w:id="03"` leaves it out of the
    /// save and leaves comment 3 alone; its undo writes `03` back verbatim.
    #[test]
    fn delete_comment_matches_the_id_as_written() {
        let mut app = app_with_03_and_3();
        let at = comment_ids(&app)
            .iter()
            .position(|id| id == "03")
            .expect("03 loaded");
        app.comment_sel = at;
        app.run_act(ribbon::Act::DeleteComment);
        assert_eq!(comment_ids(&app), ["3"]);
        let path = save_to_temp(&mut app, "cmt-03");
        let (_, comments) = saved_parts(&path);
        assert_eq!(saved_comment_ids(&comments), ["3"], "{comments}");
        assert!(comments.contains(THREE), "{comments}");
        app.on_key(ctrl(KeyCode::Char('z')));
        app.save();
        let (_, comments) = saved_parts(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(comments.contains(ZERO_THREE), "{comments}");
        assert!(comments.contains(THREE), "{comments}");
    }

    /// #971 A14: a comment an undo restores goes back to its place in the
    /// panel, and the selection stays on the comment it was on.
    #[test]
    fn undo_delete_keeps_panel_order() {
        let mut app = app_with_rich_comments(3);
        let loaded = comment_ids(&app);
        assert_eq!(loaded.len(), 3);
        app.comment_sel = 1;
        app.run_act(ribbon::Act::DeleteComment);
        assert_eq!(comment_ids(&app), [loaded[0].clone(), loaded[2].clone()]);
        app.comment_sel = 1;
        app.on_key(ctrl(KeyCode::Char('z')));
        assert_eq!(comment_ids(&app), loaded);
        assert_eq!(app.comment_sel, 2, "still on {}", loaded[2]);
    }

    /// `app` in header editing, with a comment holding `text` on the header
    /// text "Head".
    fn header_comment(app: &mut App, text: &str) {
        app.run_act(ribbon::Act::EditHeader);
        assert!(app.hf_edit.is_some());
        for c in "Head".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        add_comment_by_keys(app, text);
        assert_eq!(app.comments.len(), 1, "listed");
    }

    /// The saved header part's XML.
    fn saved_header(app: &App, path: &std::path::Path) -> String {
        let part = app.header_part.clone().expect("a header part");
        let pkg = load_package(&std::fs::read(path).expect("saved")).unwrap();
        pkg.part_text(&part).unwrap_or_default()
    }

    /// #971 A9: undoing a comment made in a header takes it out of the
    /// panel and the save, as for the body.
    #[test]
    fn undo_header_comment_then_save_has_no_comment() {
        let mut app = app_with(&["The quick brown fox."]);
        header_comment(&mut app, "Colour?");
        app.on_key(ctrl(KeyCode::Char('z')));
        assert!(app.comments.is_empty(), "the panel drops it");
        // Saving commits the header edit first, as Ctrl+S does.
        let path = save_to_temp(&mut app, "cmt-hf-undo");
        let (_, comments) = saved_parts(&path);
        let header = saved_header(&app, &path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(app.comments.is_empty());
        assert!(!comments.contains("Colour?"), "{comments}");
        assert!(!header.contains("commentReference"), "{header}");
    }

    /// #971 A10: a header comment that stays is saved with its markers in
    /// the header part, also after undo and redo.
    #[test]
    fn header_comment_is_saved_with_its_markers() {
        let mut app = app_with(&["The quick brown fox."]);
        header_comment(&mut app, "Colour?");
        let path = save_to_temp(&mut app, "cmt-hf-save");
        let (_, comments) = saved_parts(&path);
        let header = saved_header(&app, &path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert_eq!(app.comments.len(), 1);
        assert!(comments.contains("Colour?"), "{comments}");
        assert!(header.contains("commentReference"), "{header}");
    }

    #[test]
    fn header_comment_undo_redo_is_saved() {
        let mut app = app_with(&["The quick brown fox."]);
        header_comment(&mut app, "Colour?");
        app.on_key(ctrl(KeyCode::Char('z')));
        app.on_key(ctrl(KeyCode::Char('y')));
        assert_eq!(app.comments.len(), 1, "listed again");
        let path = save_to_temp(&mut app, "cmt-hf-redo");
        let (_, comments) = saved_parts(&path);
        let header = saved_header(&app, &path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(comments.contains("Colour?"), "{comments}");
        assert!(header.contains("commentReference"), "{header}");
    }

    /// #971 A13: a header edit left without committing takes its comment's
    /// markers with it, so the panel and the save drop the comment.
    #[test]
    fn header_comment_discarded_on_exit_is_not_listed() {
        let mut app = app_with(&["The quick brown fox."]);
        header_comment(&mut app, "Colour?");
        app.exit_hf_edit(false);
        assert!(app.comments.is_empty(), "the panel drops it");
        let path = save_to_temp(&mut app, "cmt-hf-discard");
        let (_, comments) = saved_parts(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(!comments.contains("Colour?"), "{comments}");
    }

    /// #971 A12: a loaded comment anchored in a header, deleted from the
    /// body, leaves the panel and the save at once: nothing can take its
    /// header markers, so it is not kept for an undo (they stay orphaned,
    /// as before).
    #[test]
    fn delete_header_anchored_comment_from_body_removes_it() {
        let mut first = app_with(&["The quick brown fox."]);
        header_comment(&mut first, "Colour?");
        let path = save_to_temp(&mut first, "cmt-hf-loaded");
        let bytes = std::fs::read(&path).unwrap();
        let mut app = App::new(
            load_package(&bytes).unwrap(),
            &path.to_string_lossy(),
            false,
        );
        app.os_clip = None;
        assert_eq!(app.comments.len(), 1, "loaded");
        assert!(app.hf_edit.is_none());
        app.run_act(ribbon::Act::DeleteComment);
        assert!(app.comments.is_empty(), "the panel drops it");
        app.on_key(key(KeyCode::Char('x')));
        assert!(app.comments.is_empty(), "and keeps it out");
        app.save();
        let (_, comments) = saved_parts(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(!comments.contains("Colour?"), "{comments}");
    }

    /// An app on "The quick brown fox." with loaded comments 1 and 2 around
    /// the whole text, markers and comments.xml alike.
    fn app_with_loaded_comments() -> App {
        let mut ed = Editor::new(Document {
            body: vec![Block::Paragraph(MPara {
                props: ParProps::default(),
                content: vec![Inline::Run(Run {
                    text: "The quick brown fox.".into(),
                    props: RunProps::default(),
                })],
            })],
        });
        for id in ["1", "2"] {
            ed.select_all();
            assert!(ed.add_comment(id));
        }
        let mut pkg = new_package(ed.doc);
        pkg.add_comment(1, "Ann", "A", "2020-01-02T03:04:05Z", "First");
        pkg.add_comment(2, "Ann", "A", "2020-01-02T03:04:05Z", "Second");
        let mut app = App::new(pkg, "test.docx", false);
        app.os_clip = None;
        app
    }

    /// FIX r3 #1: an id freed before the add (comment 2, deleted) must not
    /// be taken: undoing the add and then the delete brings 2's markers
    /// back, and the new comment would be live on them.
    #[test]
    fn a_new_comment_never_takes_a_deleted_comments_id() {
        let mut app = app_with_loaded_comments();
        app.comment_sel = 1;
        app.run_act(ribbon::Act::DeleteComment);
        add_comment_by_keys(&mut app, "Colour?");
        assert_eq!(app.comments.last().map(|c| c.id.as_str()), Some("3"));
        app.on_key(ctrl(KeyCode::Char('z')));
        app.on_key(ctrl(KeyCode::Char('z')));
        assert!(
            app.comments.iter().all(|c| c.text != "Colour?"),
            "the panel does not list it"
        );
        let path = save_to_temp(&mut app, "cmt-freed-id");
        let (_, comments) = saved_parts(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(!comments.contains("Colour?"), "{comments}");
    }

    /// FIX r3 #2: a new comment deleted before any undo or save is in no
    /// list and no marker any more; the next comment still gets a fresh id.
    #[test]
    fn a_comment_added_after_deleting_a_new_one_gets_a_fresh_id() {
        let mut app = app_with(&["The quick brown fox."]);
        add_comment_by_keys(&mut app, "one");
        let first = app.comments[0].id.clone();
        app.run_act(ribbon::Act::DeleteComment);
        assert!(app.comments.is_empty());
        add_comment_by_keys(&mut app, "two");
        assert_ne!(app.comments[0].id, first);
    }

    /// FIX r4 #1: Delete Comment while a header is being edited deletes the
    /// body comment: its markers leave the body, not the header editor, so
    /// it stays out of the panel and the save.
    #[test]
    fn delete_comment_while_editing_a_header_removes_the_body_markers() {
        let mut app = app_with(&["The quick brown fox."]);
        add_comment_by_keys(&mut app, "Colour?");
        app.run_act(ribbon::Act::EditHeader);
        assert!(app.hf_edit.is_some());
        app.run_act(ribbon::Act::DeleteComment);
        assert!(app.comments.is_empty(), "the panel drops it");
        app.run_act(ribbon::Act::EditDocument);
        assert!(app.hf_edit.is_none());
        assert!(app.comments.is_empty(), "and keeps it out");
        let path = save_to_temp(&mut app, "cmt-hf-delete");
        let (doc, comments) = saved_parts(&path);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        assert!(!doc.contains("w:id=\"1\""), "{doc}");
        assert!(!comments.contains("Colour?"), "{comments}");
    }

    #[test]
    fn new_comment_needs_a_selection() {
        let mut app = app_with(&["text"]);
        app.run_act(ribbon::Act::NewComment);
        assert!(app.comment_input.is_none());
        assert!(app.status.as_deref().unwrap_or("").contains("Select text"));
    }

    #[test]
    fn view_edit_surface_switch() {
        let mut app = app_with(&["body text"]);
        assert!(app.hf_edit.is_none());
        app.run_act(ribbon::Act::EditHeader);
        assert_eq!(
            app.hf_edit.as_ref().map(|h| h.is_header),
            Some(true),
            "switched to header editing"
        );
        app.run_act(ribbon::Act::EditFooter);
        assert_eq!(app.hf_edit.as_ref().map(|h| h.is_header), Some(false));
        app.run_act(ribbon::Act::EditDocument);
        assert!(app.hf_edit.is_none(), "returned to the body");
    }

    #[test]
    fn comment_navigation_with_no_comments_reports() {
        let mut app = app_with(&["text"]);
        app.run_act(ribbon::Act::NextComment);
        assert!(app.comments.is_empty());
        assert!(app.status.as_deref().unwrap_or("").contains("No comments"));
    }

    #[test]
    fn bullets_button_applies_a_list_and_renders_a_marker() {
        let mut app = app_with(&["item one", "item two"]);
        app.editor.select_all();
        app.run_act(ribbon::Act::Bullets);
        for i in 0..2 {
            match &app.editor.doc.body[i] {
                Block::Paragraph(p) => assert!(p.props.num_id.is_some(), "para {i} is a list item"),
                _ => panic!(),
            }
        }
        app.ensure_rendered(40);
        let plain: Vec<String> = app.lines.iter().map(|l| l.plain()).collect();
        assert!(
            plain.iter().any(|l| l.contains('•')),
            "a bullet marker should render: {plain:?}"
        );
        // toggling again removes the list
        app.editor.select_all();
        app.run_act(ribbon::Act::Bullets);
        match &app.editor.doc.body[0] {
            Block::Paragraph(p) => assert!(p.props.num_id.is_none()),
            _ => panic!(),
        }
    }

    #[test]
    fn increase_indent_shifts_the_paragraph() {
        let mut app = app_with(&["text"]);
        app.run_act(ribbon::Act::IncreaseIndent);
        match &app.editor.doc.body[0] {
            Block::Paragraph(p) => assert_eq!(p.props.indent, 720),
            _ => panic!(),
        }
        assert!(app.modified);
    }

    #[test]
    fn ribbon_first_line_and_hanging_indent() {
        let mut app = app_with(&["text"]);
        app.run_act(ribbon::Act::FirstLineIndent);
        assert_eq!(app.editor.caret_para_indent(), (0, 720));
        app.run_act(ribbon::Act::HangingIndent);
        assert_eq!(app.editor.caret_para_indent(), (0, -720));
    }

    #[test]
    fn paragraph_dialog_sets_left_and_first_line_indent() {
        let mut app = app_with(&["hello world"]);
        app.run_act(ribbon::Act::ParagraphDialog);
        assert!(app.para_dialog.is_some(), "dialog opened");
        {
            let d = app.para_dialog.as_mut().unwrap();
            d.adjust(1); // left += 0.25"
            d.adjust(1); // left = 0.5" (720)
            d.sel = 1; // Special row
            d.adjust(1); // none -> first line (by defaults to 0.5")
        }
        app.apply_para_dialog();
        assert!(app.para_dialog.is_none(), "dialog closes on apply");
        assert_eq!(app.editor.caret_para_indent(), (720, 720));
        assert!(app.modified);
    }

    #[test]
    fn paragraph_dialog_esc_cancels() {
        let mut app = app_with(&["x"]);
        app.run_act(ribbon::Act::ParagraphDialog);
        app.on_key(key(KeyCode::Esc));
        assert!(app.para_dialog.is_none());
        assert_eq!(app.editor.caret_para_indent(), (0, 0));
    }

    #[test]
    fn styles_tab_is_present() {
        let app = app_with(&["x"]);
        assert_eq!(app.ribbon.tab_label(2), Some("Styles"));
    }

    #[test]
    fn styles_ribbon_applies_a_named_style() {
        let mut app = app_with(&["text"]);
        app.run_act(ribbon::Act::ApplyStyle("Heading1"));
        match &app.editor.doc.body[0] {
            Block::Paragraph(p) => {
                assert_eq!(p.props.style_id.as_deref(), Some("Heading1"));
                assert_eq!(p.props.heading_level, Some(1));
            }
            _ => panic!(),
        }
        assert!(app.modified);
    }

    #[test]
    fn styles_dialog_opens_applies_and_cancels() {
        let mut app = app_with(&["text"]);
        app.run_act(ribbon::Act::StylesDialog);
        let d = app.styles_dialog.as_ref().expect("dialog opened");
        assert!(!d.items.is_empty(), "falls back to built-in styles");
        app.apply_styles_dialog();
        assert!(app.styles_dialog.is_none(), "closes on apply");
        // A second open then Esc leaves the paragraph unchanged.
        app.run_act(ribbon::Act::StylesDialog);
        app.on_key(key(KeyCode::Esc));
        assert!(app.styles_dialog.is_none());
    }

    #[test]
    fn font_color_picker_sets_the_run_colour() {
        let mut app = app_with(&["hello"]);
        app.editor.select_all();
        app.run_act(ribbon::Act::FontColor);
        assert!(app.font_picker.is_some(), "picker opened");
        // items: Automatic(0), Black(1), Red(2)
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert!(app.font_picker.is_none(), "picker closes after OK");
        let red = match &app.editor.doc.body[0] {
            Block::Paragraph(p) => p
                .content
                .iter()
                .any(|i| matches!(i, Inline::Run(r) if r.props.color.as_deref() == Some("FF0000"))),
            _ => false,
        };
        assert!(red, "the selection turned red");
    }

    #[test]
    fn font_picker_needs_a_selection() {
        let mut app = app_with(&["hello"]);
        app.run_act(ribbon::Act::FontColor); // no selection
        assert!(app.font_picker.is_none());
        assert!(app.status.as_deref().unwrap_or("").contains("Select text"));
    }

    #[test]
    fn grow_font_and_change_case_act_on_selection() {
        let mut app = app_with(&["abc"]);
        app.editor.select_all();
        app.run_act(ribbon::Act::GrowFont);
        app.run_act(ribbon::Act::ChangeCase);
        let txt = match &app.editor.doc.body[0] {
            Block::Paragraph(p) => p.plain_text(),
            _ => String::new(),
        };
        assert_eq!(txt, "Abc", "lowercase → Capitalize");
        assert!(app.modified);
    }

    #[test]
    fn insert_field_dialog_inserts_a_field_inline() {
        let mut app = app_with(&["x"]);
        app.run_act(ribbon::Act::InsertField);
        assert!(app.insert_field.is_some(), "dialog opened");
        app.on_key(key(KeyCode::Enter)); // insert the first field (Date)
        assert!(app.insert_field.is_none(), "dialog closes after insert");
        assert!(app.modified);
        let has_field = match &app.editor.doc.body[0] {
            Block::Paragraph(p) => p.content.iter().any(|i| matches!(i, Inline::Field { .. })),
            _ => false,
        };
        assert!(has_field, "a field inline was inserted");
    }

    #[test]
    fn insert_field_esc_cancels() {
        let mut app = app_with(&["x"]);
        app.run_act(ribbon::Act::InsertField);
        assert!(!app.on_key(key(KeyCode::Esc)));
        assert!(app.insert_field.is_none());
        assert!(!app.modified);
    }

    #[test]
    fn typing_three_dashes_then_enter_inserts_a_horizontal_line() {
        let mut app = app_with(&[""]);
        for _ in 0..3 {
            app.on_key(key(KeyCode::Char('-')));
        }
        app.on_key(key(KeyCode::Enter));
        match &app.editor.doc.body[0] {
            Block::Paragraph(p) => {
                assert!(p.content.is_empty(), "the rule paragraph is emptied");
                assert_eq!(
                    p.props.borders.bottom,
                    Some(docxcore::model::BorderKind::Single),
                    "--- + Enter should set a bottom border"
                );
            }
            _ => panic!("expected a paragraph"),
        }
        assert!(app.modified);
    }

    #[test]
    fn paste_special_offers_options_and_pastes_unformatted() {
        let mut app = app_with(&["dest"]);
        app.ensure_rendered(40);
        // Internal rich clip, with no OS clipboard available in the test.
        app.os_clip = None;
        app.clipboard = Some(Clip::from_text("hi"));
        app.clip_text = Some("hi".to_string());

        app.run_act(ribbon::Act::PasteSpecial);
        {
            let ps = app.paste_special.as_ref().expect("dialog opened");
            assert_eq!(ps.opts[0], PasteOpt::KeepSource);
            assert!(ps.opts.contains(&PasteOpt::Unformatted));
        }
        // Down twice highlights "Unformatted Text", Enter pastes and closes.
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Down));
        app.on_key(key(KeyCode::Enter));
        assert!(app.paste_special.is_none(), "dialog closes after pasting");
        assert!(app.modified);
        assert!(para_text(&app, 0).starts_with("hi"), "text was inserted");
    }

    #[test]
    fn formatting_lock_blocks_rich_paste_but_allows_plain_internal_text() {
        let mut app = app_with(&["dest"]);
        protect(&mut app, ProtectionEditMode::Unrestricted, true);
        app.os_clip = None;
        app.clipboard = Some(Clip {
            paras: vec![vec![Inline::Run(Run {
                text: "rich".to_string(),
                props: RunProps {
                    bold: true,
                    ..RunProps::default()
                },
            })]],
        });
        let before = app.editor.doc.clone();

        app.do_paste();

        assert_eq!(app.editor.doc, before);
        assert_eq!(
            app.status.as_deref(),
            Some("Edit blocked: document formatting is locked.")
        );
        assert!(!app.modified);

        app.clipboard = Some(Clip::from_text("plain"));
        app.status = None;
        app.do_paste();
        assert!(app.modified);
        assert!(para_text(&app, 0).starts_with("plain"));
    }

    #[test]
    fn paste_special_esc_cancels_without_editing() {
        let mut app = app_with(&["dest"]);
        app.os_clip = None;
        app.clipboard = Some(Clip::from_text("hi"));
        app.clip_text = Some("hi".to_string());
        app.run_act(ribbon::Act::PasteSpecial);
        assert!(app.paste_special.is_some());
        assert!(!app.on_key(key(KeyCode::Esc)));
        assert!(app.paste_special.is_none(), "Esc closes the dialog");
        assert!(!app.modified, "cancel must not modify the document");
    }

    #[test]
    fn paste_special_offers_hyperlink_for_a_url() {
        let mut app = app_with(&["dest"]);
        app.os_clip = None;
        app.clipboard = Some(Clip::from_text("https://example.com"));
        app.clip_text = Some("https://example.com".to_string());
        app.run_act(ribbon::Act::PasteSpecial);
        let ps = app.paste_special.as_ref().expect("dialog opened");
        assert!(ps.opts.contains(&PasteOpt::Hyperlink));
    }

    #[test]
    fn a_clip_with_a_formatted_complex_link_carries_formatting_212() {
        let link = |bold: bool| {
            Inline::Hyperlink(Hyperlink {
                anchor: Some("top".to_string()),
                content: vec![
                    Inline::Raw("<w:proofErr w:type=\"spellStart\"/>".to_string()),
                    Inline::Run(Run {
                        text: "Contoso".to_string(),
                        props: RunProps {
                            bold,
                            ..RunProps::default()
                        },
                    }),
                ],
                ..Hyperlink::default()
            })
        };
        assert!(clip_has_formatting(&Clip {
            paras: vec![vec![link(true)]],
        }));
        assert!(!clip_has_formatting(&Clip {
            paras: vec![vec![link(false)]],
        }));
    }

    /// A break carries its run's formatting (#279), so a clip holding only a
    /// bold break is formatted content, refused like a bold run is under
    /// formatting protection.
    #[test]
    fn a_formatted_break_counts_as_clip_formatting_279() {
        let brk = |bold| {
            Inline::Break(
                BreakKind::Line,
                RunProps {
                    bold,
                    ..RunProps::default()
                },
            )
        };
        assert!(clip_has_formatting(&Clip {
            paras: vec![vec![brk(true)]],
        }));
        assert!(!clip_has_formatting(&Clip {
            paras: vec![vec![brk(false)]],
        }));
    }

    fn vim_app(paras: &[&str]) -> App {
        let mut app = app_with(paras);
        app.vim = Some(VimState::new());
        app
    }

    fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn wheel_scroll_releases_caret_follow() {
        // Regression: the caret-follow used to snap the viewport back to the
        // caret every frame, so wheeling down from the top (caret at row 0) was
        // immediately undone — the view appeared frozen until a key was pressed.
        let paras: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = paras.iter().map(|s| s.as_str()).collect();
        let mut app = app_with(&refs);
        app.ensure_rendered(40);
        app.viewport_h = 10;
        assert!(app.follow_caret);
        app.on_mouse(mouse(MouseEventKind::ScrollDown, 0, 0));
        assert!(
            !app.follow_caret,
            "wheel scroll must stop following the caret"
        );
        assert_eq!(app.scroll, 3);
        // A key press re-arms caret-follow.
        app.on_key(key(KeyCode::Down));
        assert!(app.follow_caret);
    }

    #[test]
    fn left_drag_selects_text() {
        let mut app = app_with(&["hello world"]);
        app.ensure_rendered(40);
        app.viewport_h = 10;
        app.on_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 0));
        assert!(
            app.editor.anchor.is_none(),
            "a press starts with no selection"
        );
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 5, 0));
        let (lo, hi) = app
            .editor
            .selection_range()
            .expect("drag created a selection");
        assert_ne!(lo, hi, "selection should be non-empty after dragging");
        assert!(
            !app.follow_caret,
            "dragging shouldn't fight the user's scroll"
        );
    }

    #[test]
    fn ribbon_area_drag_does_not_move_document_selection() {
        // Regression: clicking a ribbon button while text was selected moved the
        // selection instead of running the button. A real click is
        // Down→Drag→Up; only Down was intercepted over the ribbon, so the Drag/Up
        // leaked to the document as a text-selection drag.
        let mut app = app_with(&["hello world"]);
        app.ribbon_open = true;
        let mut term = Terminal::new(TestBackend::new(90, 30)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        assert!(
            app.ribbon_h > 3,
            "ribbon body must cover button row 1 (y=3)"
        );
        // Select "hello" by dragging in the document (sets the drag anchor).
        let y = app.doc_y0;
        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            app.doc_x0,
            y,
        ));
        app.on_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            app.doc_x0 + 5,
            y,
        ));
        let before = app
            .editor
            .selection_range()
            .expect("dragging selected some text");
        // A drag + release over the ribbon (row 3, the Bold row) must be swallowed.
        app.on_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 14, 3));
        app.on_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 14, 3));
        assert_eq!(
            before,
            app.editor
                .selection_range()
                .expect("selection must be preserved"),
            "a drag/release over the ribbon must not touch the document selection"
        );
    }

    #[test]
    fn visual_right_uses_line_map_for_mixed_numeric_bidi_text() {
        let mut app = app_with(&["abc 123 אבג"]);
        app.ensure_rendered(40);
        app.viewport_h = 10;
        assert_eq!(app.lines[0].plain(), "abc 123 גבא");
        assert_eq!(app.caret_screen(), Some((0, 0)));

        let expected = [
            (1, 1),
            (2, 2),
            (3, 3),
            (4, 4),
            (5, 5),
            (6, 6),
            (7, 7),
            (8, 8),
            (11, 8),
            (10, 9),
            (9, 10),
            (8, 11),
        ];
        for (offset, col) in expected {
            app.on_key(key(KeyCode::Right));

            assert_eq!(app.editor.caret, Caret::at(vec![0], offset));
            assert_eq!(app.caret_screen(), Some((0, col)));
        }
    }

    #[test]
    fn visual_arrows_follow_rtl_paragraph_direction() {
        let mut app = rtl_app("אבג");
        app.ensure_rendered(20);
        app.viewport_h = 10;

        let start = app.caret_screen().expect("caret on screen");
        let right_edge = app.maps[0].edge_caret(true).expect("right edge");
        assert_eq!(right_edge.offset, 0);
        assert_eq!(start, (0, right_edge.col));

        app.on_key(key(KeyCode::Right));
        assert_eq!(app.editor.caret.offset, 0);
        assert_eq!(app.caret_screen(), Some(start));

        app.on_key(key(KeyCode::Left));
        let leftward = app.caret_screen().expect("caret on screen");
        assert_eq!(app.editor.caret.offset, 1);
        assert!(leftward.1 < start.1);

        app.on_key(key(KeyCode::Right));
        assert_eq!(app.editor.caret.offset, 0);
        assert_eq!(app.caret_screen(), Some(start));
    }

    #[test]
    fn visual_arrows_skip_combining_marks_and_respect_wide_cells() {
        let mut app = app_with(&["a\u{0301}哈b"]);
        app.ensure_rendered(20);
        app.viewport_h = 10;

        app.on_key(key(KeyCode::Right));
        assert_eq!(app.editor.caret.offset, 2);
        assert_eq!(app.caret_screen(), Some((0, 1)));

        app.on_key(key(KeyCode::Right));
        assert_eq!(app.editor.caret.offset, 3);
        assert_eq!(app.caret_screen(), Some((0, 3)));

        let mut selected = app_with(&["a\u{0301}哈b"]);
        selected.ensure_rendered(20);
        selected.viewport_h = 10;
        selected.on_key(shift(KeyCode::Right));
        assert_eq!(selected.editor.selection_text(), "a\u{0301}");
    }

    #[test]
    fn visual_home_end_stop_at_wrapped_line_edges() {
        let mut app = app_with(&["alpha beta gamma delta"]);
        app.ensure_rendered(8);
        app.viewport_h = 10;
        let row = 1;
        let left = app.maps[row].edge_caret(false).expect("line left edge");
        let right = app.maps[row].edge_caret(true).expect("line right edge");
        assert_ne!(left.offset, 0, "test must land on a wrapped line");

        app.set_visual_caret(row, right.clone());
        app.on_key(key(KeyCode::Home));
        assert_eq!(app.editor.caret, Caret::at(left.path.clone(), left.offset));
        assert_eq!(app.caret_screen(), Some((row, left.col)));

        app.on_key(key(KeyCode::End));
        assert_eq!(app.editor.caret, Caret::at(right.path, right.offset));
        assert_eq!(app.caret_screen(), Some((row, right.col)));
    }

    #[test]
    fn vertical_movement_preserves_desired_visual_column() {
        let mut app = app_with(&["abcdef", "x", "abcdef"]);
        app.ensure_rendered(20);
        app.viewport_h = 10;
        app.editor.set_caret(Caret::at(vec![0], 5));
        app.clear_visual_hint();
        assert_eq!(app.caret_screen(), Some((0, 5)));

        app.on_key(key(KeyCode::Down));
        assert_eq!(app.editor.caret, Caret::at(vec![1], 1));
        assert_eq!(app.caret_screen(), Some((1, 1)));

        app.on_key(key(KeyCode::Down));
        assert_eq!(app.editor.caret, Caret::at(vec![2], 5));
        assert_eq!(app.caret_screen(), Some((2, 5)));
    }

    #[test]
    fn rtl_mouse_drag_selects_logical_text_through_visual_map() {
        let mut app = rtl_app("שלום");
        app.ensure_rendered(20);
        app.viewport_h = 10;
        let right = app.maps[0].edge_caret(true).expect("right edge");
        let left = app.maps[0].edge_caret(false).expect("left edge");

        app.on_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            right.col as u16,
            0,
        ));
        assert_eq!(app.editor.caret.offset, 0);

        app.on_mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            left.col as u16,
            0,
        ));
        assert_eq!(app.editor.caret.offset, 4);
        assert_eq!(app.editor.selection_text(), "שלום");
    }

    #[test]
    fn bidi_selection_paint_skips_padding_and_controls() {
        let text = "ab \u{202e}cd\u{202c} אב";
        let mut rtl = ParProps::default();
        rtl.rtl = true;
        let doc = bidi_doc(vec![bidi_para_with(rtl, vec![bidi_run(text)])]);
        let mut opts = bidi_opts(20);
        opts.selection = vec![(vec![0], 0, text.chars().count())];

        let line = docxcore::render::render(&doc, &opts)
            .into_iter()
            .next()
            .expect("rendered line");
        let plain = line.plain();
        let highlighted: String = line
            .spans
            .iter()
            .filter(|span| span.style.highlight)
            .map(|span| span.text.as_str())
            .collect();

        assert!(plain.starts_with(' '), "test needs right-alignment padding");
        assert!(!highlighted.starts_with(' '));
        assert!(!plain.contains('\u{202e}'));
        assert!(!plain.contains('\u{202c}'));
        assert!(!highlighted.contains('\u{202e}'));
        assert!(!highlighted.contains('\u{202c}'));
        assert_eq!(highlighted, plain.trim_start());
    }

    #[test]
    fn up_movement_crosses_wrap_boundaries_to_top() {
        // A paragraph long enough to wrap into several visual lines at a narrow
        // width, followed by a short one. Regression: pressing Up used to stick
        // at a soft-wrap boundary because the boundary offset resolved to the
        // lower line, so the caret never climbed past it.
        let long = "word ".repeat(40);
        let mut app = app_with(&[&long, "tail"]);
        app.ensure_rendered(20);
        app.viewport_h = 100; // everything visible; isolate movement from scroll

        // Start at the very bottom of the document.
        app.editor.move_doc_end();
        let bottom = app.caret_screen().expect("caret on screen").0;
        assert!(
            bottom > 2,
            "expected the long paragraph to wrap into many lines"
        );

        // Walk up one visual line at a time; the row must keep decreasing and
        // finally reach the first line without getting stuck.
        let mut prev = bottom;
        for _ in 0..bottom + 5 {
            app.move_vert(false);
            let r = app.caret_screen().expect("caret on screen").0;
            assert!(r < prev || r == 0, "up-move stuck at row {prev} (got {r})");
            prev = r;
        }
        assert_eq!(prev, 0, "up movement never reached the top line");
    }

    fn first_line(app: &App) -> String {
        match &app.editor.doc.body[0] {
            Block::Paragraph(p) => p.plain_text(),
            _ => String::new(),
        }
    }

    #[test]
    fn parse_args_view_and_pdf() {
        let v = parse_args(&["a.docx".to_string()]).unwrap();
        assert_eq!(v.input.as_deref(), Some("a.docx"));
        let p = parse_args(&[
            "a.docx".to_string(),
            "--pdf".to_string(),
            "o.pdf".to_string(),
        ])
        .unwrap();
        assert_eq!(p.pdf_out.as_deref(), Some("o.pdf"));
        assert!(parse_args(&["a.docx".to_string(), "--pdf".to_string()]).is_err());
        assert!(parse_args(&["--bogus".to_string()]).is_err());
    }

    #[test]
    fn parse_args_md_and_docx_conversion() {
        let m = parse_args(&[
            "in.docx".to_string(),
            "--md".to_string(),
            "out.md".to_string(),
        ])
        .unwrap();
        assert_eq!(m.input.as_deref(), Some("in.docx"));
        assert_eq!(m.md_out.as_deref(), Some("out.md"));
        let d = parse_args(&[
            "in.md".to_string(),
            "--docx".to_string(),
            "out.docx".to_string(),
        ])
        .unwrap();
        assert_eq!(d.docx_out.as_deref(), Some("out.docx"));
        // Missing output paths are errors.
        assert!(parse_args(&["in.docx".to_string(), "--md".to_string()]).is_err());
        assert!(parse_args(&["in.md".to_string(), "--docx".to_string()]).is_err());
    }

    #[test]
    fn typing_inserts_and_marks_modified() {
        let mut app = app_with(&["ab"]);
        app.editor.move_end();
        app.on_key(key(KeyCode::Char('c')));
        assert_eq!(first_line(&app), "abc");
        assert!(app.modified);
    }

    #[test]
    fn enter_splits_and_backspace_merges() {
        let mut app = app_with(&["abcd"]);
        app.editor.caret.offset = 2;
        app.on_key(key(KeyCode::Enter));
        assert_eq!(app.editor.doc.content_block_count(), 2);
        // backspace at start of the new paragraph merges back
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.editor.doc.content_block_count(), 1);
        assert_eq!(first_line(&app), "abcd");
    }

    #[test]
    fn undo_redo_via_ctrl_keys() {
        let mut app = app_with(&["a"]);
        app.editor.move_end();
        app.on_key(key(KeyCode::Char('b')));
        app.on_key(key(KeyCode::Char('c')));
        assert_eq!(first_line(&app), "abc");
        app.on_key(ctrl(KeyCode::Char('z')));
        assert_eq!(first_line(&app), "a");
        app.on_key(ctrl(KeyCode::Char('y')));
        assert_eq!(first_line(&app), "abc");
    }

    #[test]
    fn ctrl_q_opens_the_exit_confirmation() {
        let mut app = app_with(&["a"]);
        app.editor.move_end();
        app.on_key(key(KeyCode::Char('x')));
        assert!(app.modified);
        // Ctrl+Q does not quit outright — it opens the Yes/No modal.
        assert!(!app.on_key(ctrl(KeyCode::Char('q'))));
        assert!(app.confirm.is_some());
        // the prompt warns about unsaved changes
        assert!(app.confirm.as_ref().unwrap().prompt().contains("Unsaved"));
        // confirming with 'y' quits
        assert!(app.on_key(key(KeyCode::Char('y'))));
        assert!(app.quit_requested);
    }

    fn final_tui_app() -> App {
        let mut app = App::new(
            crate::test_fixtures::marked_final_package(),
            "final.docx",
            false,
        );
        app.os_clip = None;
        app
    }

    #[test]
    fn a_final_document_asks_edit_anyway_and_refuses_until_yes() {
        let mut app = final_tui_app();
        assert!(app.doc_notice().contains("Marked as Final"));
        let before = app.editor.doc.clone();
        app.on_key(key(KeyCode::Char('x')));
        assert_eq!(app.editor.doc, before, "a final document took a key");
        assert!(!app.modified);
        let confirm = app.confirm.as_ref().expect("Edit Anyway question");
        assert!(confirm.prompt().contains("marked this document as final"));
        // No keeps it final, and the next refused key asks again.
        app.on_key(key(KeyCode::Esc));
        assert!(app.confirm.is_none());
        assert!(app.marked_final);
        app.on_key(key(KeyCode::Char('x')));
        assert!(app.confirm.is_some());
        // Yes: Edit Anyway. The refused key stays dropped.
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.confirm.is_none());
        assert!(!app.marked_final);
        assert!(!app.pkg.marked_final(), "the mark is still in the package");
        assert_eq!(app.editor.doc, before);
        assert!(!app.modified, "Edit Anyway is not an edit");
        assert!(!app.doc_notice().contains("Marked as Final"));
        app.on_key(key(KeyCode::Char('x')));
        assert!(app.modified);
    }

    #[test]
    fn ctrl_q_confirmation_can_be_cancelled() {
        let mut app = app_with(&["a"]);
        // even with no changes, Ctrl+Q asks first
        assert!(!app.on_key(ctrl(KeyCode::Char('q'))));
        assert!(app.confirm.is_some());
        // No / Esc dismisses without quitting
        assert!(!app.on_key(key(KeyCode::Esc)));
        assert!(app.confirm.is_none());
        assert!(!app.quit_requested);
    }

    #[test]
    fn esc_does_not_quit() {
        let mut app = app_with(&["a"]);
        // Esc on a clean doc must not quit.
        assert!(!app.on_key(KeyCode::Esc.into_key()));
        assert!(!app.on_key(KeyCode::Esc.into_key()));
    }

    #[test]
    fn ctrl_arrow_moves_by_word() {
        let mut app = app_with(&["alpha beta"]);
        app.editor.move_home();
        app.on_key(ctrl(KeyCode::Right));
        assert_eq!(app.editor.caret.offset, 6); // start of "beta"
        app.on_key(ctrl(KeyCode::Left));
        assert_eq!(app.editor.caret.offset, 0); // back to start of "alpha"
    }

    fn shift(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    #[test]
    fn shift_arrow_selects_and_ctrl_b_bolds() {
        let mut app = app_with(&["abcd"]);
        app.editor.move_home();
        app.on_key(shift(KeyCode::Right));
        app.on_key(shift(KeyCode::Right)); // select "ab"
        assert!(app.editor.has_selection());
        app.on_key(ctrl(KeyCode::Char('b')));
        if let Block::Paragraph(p) = &app.editor.doc.body[0] {
            if let Inline::Run(r) = &p.content[0] {
                assert_eq!(r.text, "ab");
                assert!(r.props.bold);
            }
        }
        assert!(app.modified);
    }

    #[test]
    fn esc_clears_selection_but_never_quits() {
        let mut app = app_with(&["ab"]);
        app.editor.move_home();
        app.on_key(shift(KeyCode::Right));
        assert!(app.editor.has_selection());
        assert!(!app.on_key(KeyCode::Esc.into_key())); // clears selection
        assert!(!app.editor.has_selection());
        assert!(!app.on_key(KeyCode::Esc.into_key())); // and still does not quit
    }

    #[test]
    fn cut_and_paste_via_keys() {
        let mut app = app_with(&["abcd"]);
        app.editor.move_home();
        app.on_key(shift(KeyCode::Right));
        app.on_key(shift(KeyCode::Right)); // select "ab"
        app.on_key(ctrl(KeyCode::Char('x'))); // cut
        assert_eq!(first_line(&app), "cd");
        app.editor.move_end();
        app.on_key(ctrl(KeyCode::Char('v'))); // paste
        assert_eq!(first_line(&app), "cdab");
    }

    #[test]
    fn ctrl_a_selects_all() {
        let mut app = app_with(&["ab", "cd"]);
        app.on_key(ctrl(KeyCode::Char('a')));
        assert!(app.editor.has_selection());
    }

    #[test]
    fn find_highlights_and_exits() {
        let mut app = app_with(&["foo bar foo"]);
        app.on_key(ctrl(KeyCode::Char('f')));
        assert!(app.find.is_some());
        for c in "foo".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.find.as_ref().unwrap().matches.len(), 2);
        assert!(app.editor.has_selection()); // current match is selected
        app.on_key(KeyCode::Esc.into_key());
        assert!(app.find.is_none());
    }

    #[test]
    fn replace_all_via_find_bar() {
        let mut app = app_with(&["x y x"]);
        app.on_key(ctrl(KeyCode::Char('f')));
        app.on_key(key(KeyCode::Char('x'))); // query
        app.on_key(key(KeyCode::Tab)); // switch to replace field
        app.on_key(key(KeyCode::Char('Z'))); // replacement
        app.on_key(ctrl(KeyCode::Char('a'))); // replace all
        assert_eq!(first_line(&app), "Z y Z");
    }

    /// `Alpha beta gamma delta.` with `new ` inserted and `beta ` deleted.
    fn app_with_ins_and_del() -> App {
        let doc = docxcore::load::parse_document_xml(
            "<w:document><w:body><w:p>\
             <w:r><w:t xml:space=\"preserve\">Alpha </w:t></w:r>\
             <w:ins w:id=\"1\" w:author=\"A\"><w:r><w:t xml:space=\"preserve\">new </w:t></w:r></w:ins>\
             <w:del w:id=\"2\" w:author=\"A\"><w:r><w:delText xml:space=\"preserve\">beta </w:delText></w:r></w:del>\
             <w:r><w:t>gamma delta.</w:t></w:r>\
             </w:p></w:body></w:document>",
            &docxcore::load::Relationships::default(),
        );
        let mut app = App::new(new_package(doc), "test.docx", false);
        app.os_clip = None;
        app
    }

    /// The rendered text of the body, one row per line, trimmed.
    fn shown_text(app: &mut App) -> String {
        app.dirty = true;
        app.ensure_rendered(80);
        app.lines
            .iter()
            .map(|l| l.plain().trim_end().to_string())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// #625 C1: the four views show the fixture as Word does, change nothing
    /// in the document, and the save is the same bytes under each.
    #[test]
    fn display_for_review_modes_render_without_changing_the_document() {
        use docxcore::markup::MarkupView;
        let mut app = app_with_ins_and_del();
        let doc = app.editor.doc.clone();
        let revisions = app.editor.doc.revisions().len();
        let mut saved = Vec::new();
        for (view, text) in [
            (MarkupView::All, "Alpha new beta gamma delta."),
            (MarkupView::Simple, "Alpha new gamma delta."),
            (MarkupView::NoMarkup, "Alpha new gamma delta."),
            (MarkupView::Original, "Alpha beta gamma delta."),
        ] {
            app.run_act(ribbon::Act::CycleMarkup);
            // The first step leaves All: walk to `view` by cycling.
            while app.markup != view {
                app.run_act(ribbon::Act::CycleMarkup);
            }
            assert_eq!(shown_text(&mut app), text, "{view:?}");
            assert_eq!(app.editor.doc, doc, "{view:?}");
            assert_eq!(app.editor.doc.revisions().len(), revisions);
            assert!(!app.modified, "{view:?}");
            app.pkg.document = app.editor.doc.clone();
            let bytes = docxcore::package::save_package(&app.pkg);
            let pkg = load_package(&bytes).unwrap();
            saved.push(pkg.part_text("word/document.xml").unwrap());
        }
        assert!(
            saved.windows(2).all(|w| w[0] == w[1]),
            "document.xml differs"
        );
    }

    /// #625 C2: in No Markup and Original a key that would edit is refused
    /// with a status; All and Simple Markup edit as always.
    #[test]
    fn view_only_markup_modes_refuse_typing_and_say_why() {
        use docxcore::markup::MarkupView;
        for (view, refused) in [
            (MarkupView::All, false),
            (MarkupView::Simple, false),
            (MarkupView::NoMarkup, true),
            (MarkupView::Original, true),
        ] {
            let mut app = app_with_ins_and_del();
            app.set_markup(view);
            let before = app.editor.doc.clone();
            app.on_key(key(KeyCode::Char('z')));
            if refused {
                assert_eq!(app.editor.doc, before, "{view:?}");
                let status = app.status.clone().unwrap_or_default();
                assert!(status.contains("Display for Review"), "{status}");
                assert!(!app.modified);
            } else {
                assert_ne!(app.editor.doc, before, "{view:?}");
            }
        }
    }

    /// `x ` + a tracked insertion `x` + ` x`: Find shows all three (#211).
    fn app_with_tracked_x() -> App {
        let doc = docxcore::load::parse_document_xml(
            "<w:document><w:body><w:p>\
             <w:r><w:t xml:space=\"preserve\">x </w:t></w:r>\
             <w:ins w:id=\"1\" w:author=\"A\"><w:r><w:t>x</w:t></w:r></w:ins>\
             <w:r><w:t xml:space=\"preserve\"> x</w:t></w:r>\
             </w:p></w:body></w:document>",
            &docxcore::load::Relationships::default(),
        );
        let mut app = App::new(new_package(doc), "test.docx", false);
        app.os_clip = None;
        app
    }

    fn find_x_replace_with_z(app: &mut App) {
        app.on_key(ctrl(KeyCode::Char('f')));
        app.on_key(key(KeyCode::Char('x')));
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Char('Z')));
    }

    #[test]
    fn find_bar_shows_text_in_a_tracked_change_211() {
        let mut app = app_with_tracked_x();
        app.on_key(ctrl(KeyCode::Char('f')));
        app.on_key(key(KeyCode::Char('x')));
        let f = app.find.as_ref().unwrap();
        let editable: Vec<bool> = f.matches.iter().map(|m| m.editable).collect();
        assert_eq!(editable, [true, false, true]);
        app.on_key(KeyCode::Down.into_key());
        assert_eq!(
            app.find.as_ref().unwrap().idx,
            1,
            "the read-only match is visited"
        );
    }

    /// Replace on a read-only match leaves the document alone and moves on;
    /// on the next (editable) match it replaces.
    #[test]
    fn find_bar_replace_skips_a_read_only_match_211() {
        let mut app = app_with_tracked_x();
        find_x_replace_with_z(&mut app);
        app.find_step(1); // onto the tracked `x`
        let before = app.editor.doc.body.clone();
        app.on_key(KeyCode::Enter.into_key());
        assert_eq!(
            app.editor.doc.body, before,
            "a read-only match is never edited"
        );
        assert!(!app.modified);
        assert_eq!(app.find.as_ref().unwrap().idx, 2, "and Replace advanced");
        app.on_key(KeyCode::Enter.into_key());
        assert_eq!(first_line(&app), "x x Z");
    }

    /// Replace with the selection moved off the current editable match
    /// selects the match again and edits nothing (m3 of review r1).
    #[test]
    fn find_bar_replace_reselects_a_match_the_selection_left_211() {
        let mut app = app_with_tracked_x();
        find_x_replace_with_z(&mut app);
        let m = app.find.as_ref().unwrap().matches[0].clone();
        assert!(m.editable && app.editor.selection_is(&m));
        app.editor.clear_selection(); // a click elsewhere
        let before = app.editor.doc.body.clone();
        app.on_key(KeyCode::Enter.into_key());
        assert_eq!(app.editor.doc.body, before, "nothing replaced");
        assert!(app.editor.selection_is(&m), "the match is selected again");
        assert_eq!(app.status, None, "and it is not reported as read-only");
        app.on_key(KeyCode::Enter.into_key());
        assert_eq!(first_line(&app), "Z x x");
    }

    /// An edit while the bar is open (a ribbon Accept here) rebuilds its
    /// matches, so Replace never acts on a stale range (r3 m2: Enter, Enter
    /// used to replace the space where the last `x` had been).
    #[test]
    fn an_edit_with_the_bar_open_refreshes_its_matches_211() {
        let mut app = app_with_tracked_x();
        find_x_replace_with_z(&mut app);
        app.find_step(1); // onto the tracked `x`, selected for review
        let caret = app.editor.caret.clone();
        app.review_current_revision(RevisionAction::Accept);
        assert_eq!(first_line(&app), "x x x");
        let f = app.find.as_ref().unwrap();
        let ranges: Vec<(usize, usize, bool)> = f
            .matches
            .iter()
            .map(|m| (m.start, m.end, m.editable))
            .collect();
        assert_eq!(ranges, [(0, 1, true), (2, 3, true), (4, 5, true)]);
        assert_eq!(f.idx, 1);
        assert_eq!(
            app.editor.caret, caret,
            "the refresh does not move the caret"
        );
        app.on_key(KeyCode::Enter.into_key()); // selects the current match again
        app.on_key(KeyCode::Enter.into_key()); // replaces it
        assert_eq!(first_line(&app), "x Z x");
    }

    #[test]
    fn find_bar_replace_all_reports_skipped_read_only_matches_211() {
        let mut app = app_with_tracked_x();
        find_x_replace_with_z(&mut app);
        app.on_key(ctrl(KeyCode::Char('a')));
        assert_eq!(first_line(&app), "Z x Z", "the tracked `x` stays");
        assert_eq!(
            app.status.as_deref(),
            Some("Replaced 2; 1 read-only match(es) skipped")
        );
    }

    #[test]
    fn vim_insert_and_escape() {
        let mut app = vim_app(&["abc"]);
        assert_eq!(app.vim.as_ref().unwrap().mode, VimMode::Normal);
        app.on_key(key(KeyCode::Char('A'))); // append at end -> Insert
        assert_eq!(app.vim.as_ref().unwrap().mode, VimMode::Insert);
        app.on_key(key(KeyCode::Char('X')));
        assert_eq!(first_line(&app), "abcX");
        app.on_key(KeyCode::Esc.into_key());
        assert_eq!(app.vim.as_ref().unwrap().mode, VimMode::Normal);
    }

    #[test]
    fn vim_x_deletes_char() {
        let mut app = vim_app(&["abc"]);
        app.editor.move_home();
        app.on_key(key(KeyCode::Char('x'))); // delete 'a'
        assert_eq!(first_line(&app), "bc");
    }

    #[test]
    fn vim_dd_and_paste() {
        let mut app = vim_app(&["one", "two", "three"]);
        // caret on line 0; dd deletes it
        app.on_key(key(KeyCode::Char('d')));
        app.on_key(key(KeyCode::Char('d')));
        let texts: Vec<String> = app
            .editor
            .doc
            .body
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => Some(p.plain_text()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["two", "three"]);
    }

    #[test]
    fn vim_count_motion() {
        let mut app = vim_app(&["abcdef"]);
        app.editor.move_home();
        app.on_key(key(KeyCode::Char('3')));
        app.on_key(key(KeyCode::Char('l'))); // 3l -> offset 3
        assert_eq!(app.editor.caret.offset, 3);
    }

    #[test]
    fn vim_visual_delete() {
        let mut app = vim_app(&["abcd"]);
        app.editor.move_home();
        app.on_key(key(KeyCode::Char('v'))); // visual
        app.on_key(key(KeyCode::Char('l'))); // extend right
        app.on_key(key(KeyCode::Char('l'))); // select "abc" (caret moved to 2, anchor 0)
        app.on_key(key(KeyCode::Char('d'))); // delete selection
        assert_eq!(first_line(&app), "d");
    }

    #[test]
    fn vim_command_quit() {
        let mut app = vim_app(&["x"]);
        app.on_key(key(KeyCode::Char(':')));
        app.on_key(key(KeyCode::Char('q')));
        assert!(app.on_key(key(KeyCode::Enter))); // :q with no changes -> quit
    }

    #[test]
    fn link_at_resolves_hyperlink() {
        let h = Inline::Hyperlink(Hyperlink {
            target: Some("https://x.test/".to_string()),
            anchor: None,
            rel_id: None,
            runs: vec![Run {
                text: "link".to_string(),
                props: RunProps::default(),
            }],
            ..Hyperlink::default()
        });
        let body = vec![Block::Paragraph(MPara {
            props: ParProps::default(),
            content: vec![h],
        })];
        let mut app = App::new(new_package(Document { body }), "t.docx", false);
        app.os_clip = None;
        app.ensure_rendered(40);
        assert_eq!(app.link_at(0, 1).as_deref(), Some("https://x.test/")); // over "link"
        assert_eq!(app.link_at(0, 20), None); // past the text
    }

    #[test]
    fn link_at_uses_rendered_cell_width() {
        let h = Inline::Hyperlink(Hyperlink {
            target: Some("https://wide.test/".to_string()),
            runs: vec![Run {
                text: "哈".to_string(),
                props: RunProps::default(),
            }],
            ..Hyperlink::default()
        });
        let body = vec![Block::Paragraph(MPara {
            props: ParProps::default(),
            content: vec![
                h,
                Inline::Run(Run {
                    text: "x".to_string(),
                    props: RunProps::default(),
                }),
            ],
        })];
        let mut app = App::new(new_package(Document { body }), "wide.docx", false);
        app.os_clip = None;
        app.ensure_rendered(40);

        assert_eq!(app.lines[0].plain(), "哈x");
        assert_eq!(app.link_at(0, 0).as_deref(), Some("https://wide.test/"));
        assert_eq!(app.link_at(0, 1).as_deref(), Some("https://wide.test/"));
        assert_eq!(app.link_at(0, 2), None);
    }

    #[test]
    fn link_at_follows_bidi_visual_columns() {
        let h = Inline::Hyperlink(Hyperlink {
            target: Some("https://rtl.test/".to_string()),
            runs: vec![Run {
                text: "אב".to_string(),
                props: RunProps::default(),
            }],
            ..Hyperlink::default()
        });
        let mut rtl = ParProps::default();
        rtl.rtl = true;
        let body = vec![Block::Paragraph(MPara {
            props: rtl,
            content: vec![
                Inline::Run(Run {
                    text: "x ".to_string(),
                    props: RunProps::default(),
                }),
                h,
                Inline::Run(Run {
                    text: " z".to_string(),
                    props: RunProps::default(),
                }),
            ],
        })];
        let mut app = App::new(new_package(Document { body }), "rtl-link.docx", false);
        app.os_clip = None;
        app.ensure_rendered(40);

        let visual = app.lines[0].plain();
        let link_col = visual
            .chars()
            .position(|ch| ch == 'ב')
            .unwrap_or_else(|| panic!("missing visual RTL link text: {visual:?}"));
        assert_eq!(
            app.link_at(0, link_col).as_deref(),
            Some("https://rtl.test/")
        );
        assert_eq!(app.link_at(0, 0), None);
    }

    #[test]
    fn safe_url_allows_only_web_links() {
        assert!(safe_url("https://example.com/path?q=1"));
        assert!(safe_url("http://host"));
        assert!(!safe_url("mailto:a@b.c"));
        assert!(!safe_url("file:///etc/passwd"));
        assert!(!safe_url("javascript:alert(1)"));
        assert!(!safe_url("data:text/html,x"));
        assert!(!safe_url("https://x\u{7f}evil")); // control char
        assert!(!safe_url(""));
    }

    fn link_doc(target: &str) -> Document {
        let h = Inline::Hyperlink(Hyperlink {
            target: Some(target.to_string()),
            anchor: None,
            rel_id: None,
            runs: vec![Run {
                text: "link".to_string(),
                props: RunProps::default(),
            }],
            ..Hyperlink::default()
        });
        Document {
            body: vec![Block::Paragraph(MPara {
                props: ParProps::default(),
                content: vec![h],
            })],
        }
    }

    fn left_click(app: &mut App, col: u16, row: u16) {
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }

    #[test]
    fn clicking_web_link_requires_confirmation() {
        let mut app = App::new(new_package(link_doc("https://x.test/")), "t.docx", false);
        app.os_clip = None;
        app.ensure_rendered(40);
        app.viewport_h = 10;
        left_click(&mut app, 1, 0);
        assert_eq!(app.pending_link.as_deref(), Some("https://x.test/")); // queued, not opened
        app.on_key(key(KeyCode::Char('n'))); // cancel
        assert!(app.pending_link.is_none());
    }

    #[test]
    fn clicking_nonweb_link_is_blocked_outright() {
        let mut app = App::new(
            new_package(link_doc("file:///c:/windows/system32/calc.exe")),
            "t.docx",
            false,
        );
        app.os_clip = None;
        app.ensure_rendered(40);
        app.viewport_h = 10;
        left_click(&mut app, 1, 0);
        assert!(app.pending_link.is_none()); // never even queued
    }

    #[test]
    fn color_mapping_is_total() {
        for c in [
            DocColor::Black,
            DocColor::BrightWhite,
            DocColor::Cyan,
            DocColor::BrightBlue,
        ] {
            let _ = map_color(c);
        }
    }

    // ---- "Word basics" curriculum ------------------------------------------
    // Each standard beginner-Word lesson, run through the real key / ribbon
    // handlers and checked on the resulting document. Mirrors suite/docxy's
    // ui_e2e.ps1 so the same lessons are covered on both the TUI and the UI.

    fn run0(app: &App) -> &Run {
        match &app.editor.doc.body[0] {
            Block::Paragraph(p) => p
                .content
                .iter()
                .find_map(|i| {
                    if let Inline::Run(r) = i {
                        Some(r)
                    } else {
                        None
                    }
                })
                .expect("a run"),
            _ => panic!("expected a paragraph"),
        }
    }
    fn props0(app: &App) -> &ParProps {
        match &app.editor.doc.body[0] {
            Block::Paragraph(p) => &p.props,
            _ => panic!("expected a paragraph"),
        }
    }

    #[test]
    fn lesson_01_type_text() {
        let mut app = app_with(&["Hell"]);
        app.editor.move_end();
        app.on_key(key(KeyCode::Char('o')));
        assert_eq!(first_line(&app), "Hello");
    }

    #[test]
    fn lesson_02_bold_italic_underline() {
        let mut app = app_with(&["text"]);
        app.editor.select_all();
        app.run_act(ribbon::Act::Bold);
        app.run_act(ribbon::Act::Italic);
        app.run_act(ribbon::Act::Underline);
        let r = run0(&app);
        assert!(
            r.props.bold && r.props.italic && r.props.underline,
            "B/I/U not all applied"
        );
    }

    #[test]
    fn lesson_03_apply_heading() {
        let mut app = app_with(&["Chapter One"]);
        app.run_act(ribbon::Act::ApplyStyle("Heading1"));
        assert_eq!(props0(&app).heading_level, Some(1));
    }

    #[test]
    fn lesson_04_no_spacing_style() {
        let mut app = app_with(&["body"]);
        app.run_act(ribbon::Act::ApplyStyle("NoSpacing"));
        assert_eq!(props0(&app).style_id.as_deref(), Some("NoSpacing"));
    }

    #[test]
    fn lesson_05_bulleted_list() {
        let mut app = app_with(&["item"]);
        app.run_act(ribbon::Act::Bullets);
        assert!(props0(&app).num_id.is_some(), "no bullet list applied");
    }

    #[test]
    fn lesson_06_numbered_list() {
        let mut app = app_with(&["item"]);
        app.run_act(ribbon::Act::Numbering);
        assert!(props0(&app).num_id.is_some(), "no numbered list applied");
    }

    #[test]
    fn lesson_07_center_align() {
        let mut app = app_with(&["centered"]);
        app.run_act(ribbon::Act::AlignCenter);
        assert_eq!(props0(&app).align, Align::Center);
    }

    #[test]
    fn lesson_08_increase_indent() {
        let mut app = app_with(&["indent me"]);
        app.run_act(ribbon::Act::IncreaseIndent);
        assert!(props0(&app).indent > 0, "indent not increased");
    }

    #[test]
    fn lesson_09_change_case() {
        let mut app = app_with(&["hello"]);
        app.editor.select_all();
        app.run_act(ribbon::Act::ChangeCase);
        assert_ne!(first_line(&app), "hello", "case not changed");
    }

    #[test]
    fn lesson_10_find_and_replace() {
        let mut app = app_with(&["x y x"]);
        app.on_key(ctrl(KeyCode::Char('f')));
        app.on_key(key(KeyCode::Char('x'))); // query
        app.on_key(key(KeyCode::Tab)); // to replace field
        app.on_key(key(KeyCode::Char('Z'))); // replacement
        app.on_key(ctrl(KeyCode::Char('a'))); // replace all
        assert_eq!(first_line(&app), "Z y Z");
    }

    #[test]
    fn lesson_11_undo_redo() {
        let mut app = app_with(&["a"]);
        app.editor.move_end();
        app.on_key(key(KeyCode::Char('b')));
        assert_eq!(first_line(&app), "ab");
        app.on_key(ctrl(KeyCode::Char('z')));
        assert_eq!(first_line(&app), "a");
        app.on_key(ctrl(KeyCode::Char('y')));
        assert_eq!(first_line(&app), "ab");
    }

    #[test]
    fn lesson_12_edit_header() {
        let mut app = app_with(&["body"]);
        app.run_act(ribbon::Act::EditHeader);
        assert!(app.hf_edit.is_some(), "header edit not entered");
        app.on_key(key(KeyCode::Char('H')));
        app.on_key(key(KeyCode::Char('i')));
        assert_eq!(first_line(&app), "Hi", "typed header text missing");
    }

    #[test]
    fn lesson_13_grow_font() {
        let mut app = app_with(&["text"]);
        app.editor.select_all();
        app.run_act(ribbon::Act::GrowFont);
        assert!(
            run0(&app).props.size_half_pts.is_some(),
            "grow font set no size"
        );
    }

    #[test]
    fn lesson_14_clear_formatting() {
        let mut app = app_with(&["text"]);
        app.editor.select_all();
        app.run_act(ribbon::Act::Bold);
        app.run_act(ribbon::Act::ClearFormatting);
        assert!(!run0(&app).props.bold, "clear formatting left bold on");
    }

    #[test]
    fn lesson_16_page_break() {
        let mut app = app_with(&["text"]);
        app.editor.move_end();
        app.run_act(ribbon::Act::PageBreak);
        let has_break = app.editor.doc.body.iter().any(|b| matches!(b, Block::Paragraph(p) if p.content.iter().any(|i| matches!(i, Inline::Break(BreakKind::Page, _)))));
        assert!(has_break, "no page break inserted");
    }

    #[test]
    fn lesson_17_page_number() {
        let mut app = app_with(&["text"]);
        app.editor.move_end();
        app.run_act(ribbon::Act::PageNumber);
        let has_page = app.editor.doc.body.iter().any(|b| matches!(b, Block::Paragraph(p) if p.content.iter().any(|i| matches!(i, Inline::Field { raw, .. } if raw.contains("PAGE")))));
        assert!(has_page, "no PAGE field inserted");
    }

    #[test]
    fn tab_moves_between_table_cells_and_adds_a_row_in_the_last() {
        let mut app = app_with(&["text"]);
        app.run_act(ribbon::Act::InsertTable);
        let table_path = app
            .editor
            .table_at_caret()
            .expect("caret in the table")
            .table;
        app.modified = false;
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Tab));
        app.on_key(key(KeyCode::Char('b')));
        app.on_key(KeyCode::BackTab.into());
        // Shift+Tab selected "a": typing replaces it.
        app.on_key(key(KeyCode::Char('c')));
        let text = |app: &App, r: usize, c: usize| {
            app.editor.table(&table_path).unwrap().rows[r].cells[c].blocks[0].plain_text()
        };
        assert_eq!(text(&app, 0, 0), "c");
        assert_eq!(text(&app, 0, 1), "b");
        // To the last cell, then Tab adds a row.
        for _ in 0..3 {
            app.on_key(key(KeyCode::Tab));
        }
        assert_eq!(app.editor.table(&table_path).unwrap().rows.len(), 2);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.editor.table(&table_path).unwrap().rows.len(), 3);
        // Ctrl+Tab types a tab in the cell.
        app.on_key(ctrl(KeyCode::Tab));
        let t = app.editor.table(&table_path).unwrap();
        let Block::Paragraph(p) = &t.rows[2].cells[0].blocks[0] else {
            panic!("paragraph")
        };
        assert!(matches!(p.content.as_slice(), [Inline::Tab(_)]));
    }

    #[test]
    fn tab_between_cells_is_not_an_edit() {
        let mut app = app_with(&["text"]);
        app.run_act(ribbon::Act::InsertTable);
        app.modified = false;
        app.on_key(key(KeyCode::Tab));
        app.on_key(KeyCode::BackTab.into());
        assert!(!app.modified);
    }

    #[test]
    fn lesson_18_insert_table() {
        let mut app = app_with(&["text"]);
        app.run_act(ribbon::Act::InsertTable);
        let t = app
            .editor
            .doc
            .body
            .iter()
            .find_map(|b| {
                if let Block::Table(t) = b {
                    Some(t)
                } else {
                    None
                }
            })
            .expect("no table inserted");
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[0].cells.len(), 2);
    }

    #[test]
    fn lesson_19_insert_symbol() {
        let mut app = app_with(&["text"]);
        app.editor.move_end();
        app.run_act(ribbon::Act::InsertSymbol);
        assert!(app.font_picker.is_some(), "symbol picker did not open");
        app.apply_picker(); // sel 0 = em dash
        assert!(
            first_line(&app).ends_with('\u{2014}'),
            "em dash not inserted: {}",
            first_line(&app)
        );
    }

    #[test]
    fn lesson_20_line_spacing() {
        let mut app = app_with(&["text"]);
        app.run_act(ribbon::Act::LineSpacing);
        app.font_picker.as_mut().expect("line-spacing picker").sel = 2; // "1.5"
        app.apply_picker();
        assert_eq!(
            props0(&app).spacing.line,
            Some(360),
            "line spacing not 1.5x"
        );
    }

    #[test]
    fn lesson_21_typographic_quotes() {
        // The signature typesetting lesson: proper guillemets are in the picker.
        let mut app = app_with(&["text"]);
        app.editor.move_end();
        app.run_act(ribbon::Act::InsertSymbol);
        let items = PickerKind::Symbol.items();
        let idx = items
            .iter()
            .position(|&s| s == "\u{00AB}")
            .expect("no guillemet in symbol picker");
        app.font_picker.as_mut().unwrap().sel = idx;
        app.apply_picker();
        assert!(
            first_line(&app).ends_with('\u{00AB}'),
            "guillemet not inserted"
        );
    }

    #[test]
    fn lesson_22_columns() {
        let mut app = app_with(&["text"]);
        assert_eq!(app.pkg.columns(), 1);
        app.run_act(ribbon::Act::Columns);
        assert_eq!(app.pkg.columns(), 2, "columns not set to 2");
    }

    #[test]
    fn lesson_23_hyphenation() {
        let mut app = app_with(&["text"]);
        assert!(!app.pkg.has_auto_hyphenation());
        app.run_act(ribbon::Act::Hyphenation);
        assert!(app.pkg.has_auto_hyphenation(), "hyphenation not turned on");
    }

    #[test]
    fn lesson_24_non_breaking_space() {
        let mut app = app_with(&["ab"]);
        app.editor.move_end();
        app.on_key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert!(
            first_line(&app).ends_with('\u{00A0}'),
            "no non-breaking space inserted"
        );
    }

    #[test]
    fn lesson_25_insert_equation() {
        let mut app = app_with(&["text"]);
        app.editor.move_end();
        app.run_act(ribbon::Act::InsertEquation);
        assert!(app.font_picker.is_some(), "equation picker did not open");
        // sel 0 = "x²" -> latex "x^2"
        app.apply_picker();
        let has_eq = app.editor.doc.body.iter().any(|b| matches!(b, Block::Paragraph(p)
            if p.content.iter().any(|i| matches!(i, Inline::Equation { raw, .. } if raw.contains("m:oMath")))));
        assert!(has_eq, "no OMML equation inserted");
    }

    #[test]
    fn lesson_15_save_captures_edits() {
        let mut app = app_with(&["Hello"]);
        app.editor.select_all();
        app.run_act(ribbon::Act::Bold);
        let doc = app.current_document();
        match &doc.body[0] {
            Block::Paragraph(p) => assert!(
                p.content
                    .iter()
                    .any(|i| matches!(i, Inline::Run(r) if r.props.bold))
            ),
            _ => panic!(),
        }
    }

    // small helper to turn a KeyCode into a no-modifier KeyEvent in asserts
    trait IntoKey {
        fn into_key(self) -> KeyEvent;
    }
    impl IntoKey for KeyCode {
        fn into_key(self) -> KeyEvent {
            KeyEvent::new(self, KeyModifiers::NONE)
        }
    }
    /// An original and a revised .docx (the #626 example) in a fresh directory.
    fn compare_fixture(tag: &str) -> (std::path::PathBuf, String, String) {
        let dir = std::env::temp_dir().join(format!("docxy-compare-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, text: &str| {
            let path = dir.join(name);
            let doc = Document {
                body: vec![Block::Paragraph(MPara {
                    props: ParProps::default(),
                    content: vec![Inline::Run(Run {
                        text: text.to_string(),
                        props: RunProps::default(),
                    })],
                })],
            };
            std::fs::write(&path, save_package(&new_package(doc))).unwrap();
            path.display().to_string()
        };
        let original = write("orig.docx", "The cat sat on the mat.");
        let revised = write("rev.docx", "The black cat sat on a mat.");
        (dir, original, revised)
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn compare_opens_an_unsaved_compare_result_beside_the_revised_file() {
        let (dir, original, revised) = compare_fixture("open");
        let (before_o, before_r) = (
            std::fs::read(&original).unwrap(),
            std::fs::read(&revised).unwrap(),
        );
        let input = load_input(&original).unwrap();
        let mut app = App::new(input.pkg, &original, false);
        app.os_clip = None;

        let summary = app.compare_paths(&original, &revised, false).unwrap();
        let expected = dir.join("Compare Result 1.docx").display().to_string();
        assert_eq!(summary.path, expected);
        assert_eq!(app.path, expected);
        assert!(app.modified, "the result exists only in memory");
        assert!(
            !Path::new(&expected).exists(),
            "nothing is written until saved"
        );
        assert_eq!((summary.insertions, summary.deletions), (2, 1));
        assert_eq!(app.editor.doc.revisions().len(), 3);
        assert_eq!(
            app.status.as_deref(),
            Some("compared: 2 insertions, 1 deletions")
        );
        assert_eq!(std::fs::read(&original).unwrap(), before_o);
        assert_eq!(std::fs::read(&revised).unwrap(), before_r);

        app.editor.accept_all_revisions();
        assert_eq!(app.editor.doc.plain_text(), "The black cat sat on a mat.\n");

        // The name skips results already on disk.
        std::fs::write(&expected, b"taken").unwrap();
        app.modified = false;
        let second = app.compare_paths(&original, &revised, false).unwrap();
        assert_eq!(
            second.path,
            dir.join("Compare Result 2.docx").display().to_string()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compare_refuses_unsaved_changes_unless_the_terminal_confirms() {
        let (dir, original, revised) = compare_fixture("unsaved");
        let mut app = app_with(&["draft"]);
        app.modified = true;
        let err = app.compare_paths(&original, &revised, false).err().unwrap();
        assert!(err.contains("unsaved"), "{err}");
        assert_eq!(app.path, "test.docx");
        assert_eq!(app.editor.doc.plain_text(), "draft\n");

        // Review ▸ Compare: test.docx is not on disk, so both paths are typed.
        app.run_act(ribbon::Act::Compare);
        let d = app.compare_dialog.as_ref().expect("compare dialog");
        assert_eq!((d.original.as_str(), d.field), ("", 0));
        type_text(&mut app, &original);
        app.on_key(key(KeyCode::Tab));
        type_text(&mut app, &format!("\"{revised}\""));
        app.on_key(key(KeyCode::Enter));
        assert!(app.compare_dialog.is_none());
        let prompt = app
            .confirm
            .as_ref()
            .expect("discard prompt")
            .prompt()
            .to_string();
        assert!(prompt.contains("Discard unsaved changes"), "{prompt}");
        assert_eq!(app.path, "test.docx", "nothing happens before Yes");
        app.on_key(key(KeyCode::Char('y')));
        assert!(app.path.ends_with("Compare Result 1.docx"), "{}", app.path);
        assert_eq!(app.editor.doc.revisions().len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compare_during_header_editing_leaves_it_and_shows_the_result() {
        let (dir, original, revised) = compare_fixture("header");
        let input = load_input(&original).unwrap();
        let mut app = App::new(input.pkg, &original, false);
        app.os_clip = None;
        app.enter_hf_edit(true);
        assert!(app.hf_edit.is_some());
        type_text(&mut app, "H");

        // Uncommitted header edits count as unsaved changes.
        let err = app.compare_paths(&original, &revised, false).err().unwrap();
        assert!(err.contains("unsaved"), "{err}");
        assert!(app.hf_edit.is_none(), "header editing was left");

        app.compare_paths(&original, &revised, true).unwrap();
        assert!(app.hf_edit.is_none());
        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.editor.doc.revisions().len(), 3, "the result stays open");
        assert!(
            app.header_part.is_none(),
            "the revised package has no header"
        );
        assert!(
            app.pkg.part_names().iter().all(|n| !n.contains("header")),
            "{:?}",
            app.pkg.part_names()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compare_dialog_defaults_the_original_to_the_saved_docx() {
        let (dir, original, revised) = compare_fixture("dialog");
        let input = load_input(&original).unwrap();
        let mut app = App::new(input.pkg, &original, false);
        app.os_clip = None;
        app.run_act(ribbon::Act::Compare);
        let d = app.compare_dialog.as_ref().expect("compare dialog");
        assert_eq!((d.original.as_str(), d.field), (original.as_str(), 1));
        // A non-.docx revised path is refused with a clear status.
        type_text(&mut app, "notes.md");
        app.on_key(key(KeyCode::Enter));
        let status = app.status.clone().unwrap_or_default();
        assert!(
            status.contains("compare failed") && status.contains("not a .docx"),
            "{status}"
        );
        assert_eq!(app.path, original);
        // Esc closes without comparing.
        app.run_act(ribbon::Act::Compare);
        type_text(&mut app, &revised);
        app.on_key(key(KeyCode::Esc));
        assert!(app.compare_dialog.is_none());
        assert_eq!(app.path, original);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod html_bundle_tests {
    //! Editable HTML (`*.docx.html`) in the terminal editor: open, save back
    //! into the bundle, Save As out of and into the format.
    use super::*;

    fn sample_docx() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../assets/sample.docx"
        ))
        .unwrap()
    }

    fn bundle_of(docx: &[u8], name: &str) -> String {
        // Reading and rewrapping never touch the engine block, so a stand-in
        // engine keeps these tests independent of the html-export feature.
        htmlbundle::wrap(
            &htmlbundle::docx_assets(),
            b"\0asm stand-in",
            "docx",
            name,
            docx,
            "test",
            "2026-09-25T00:00:00Z",
        )
        .unwrap()
    }

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("docxy-html-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn open_app(path: &std::path::Path) -> App {
        let mut app = App::new(new_package(Document::default()), "untitled.docx", false);
        app.open_path(path).unwrap();
        app
    }

    #[test]
    fn bundle_paths_pick_the_html_format() {
        assert_eq!(format_for("sample.docx.html"), DocFormat::Html);
        assert_eq!(format_for(r"C:\x\Report.DOCX.HTML"), DocFormat::Html);
        // Any HTML name: browsers rename downloads, people pick names.
        assert_eq!(format_for("sample.docx (1).html"), DocFormat::Html);
        assert_eq!(format_for("page.html"), DocFormat::Html);
        assert!(DocFormat::Html.is_docx());
        let err = load_input("book.xlsx.html").err().unwrap();
        assert!(err.contains("spreadsheet"), "{err}");
    }

    #[test]
    fn opening_a_bundle_edits_the_embedded_docx() {
        let dir = temp("open");
        let path = dir.join("sample.docx.html");
        std::fs::write(&path, bundle_of(&sample_docx(), "sample.docx")).unwrap();
        let app = open_app(&path);
        assert_eq!(app.format, DocFormat::Html);
        assert!(app.bundle_html.is_some());
        assert!(app.editor.doc.body.len() > 10);
        assert!(window_title("docxy", &app.path, false).ends_with("sample.docx.html"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_rewraps_the_bundle_in_place() {
        let dir = temp("save");
        let path = dir.join("sample.docx.html");
        let original = bundle_of(&sample_docx(), "sample.docx");
        std::fs::write(&path, &original).unwrap();
        let mut app = open_app(&path);
        app.editor.set_caret(Caret::top(0, 0));
        app.editor.insert_str("Edited in docxy ");
        app.modified = true;
        app.save();
        assert!(!app.modified, "{:?}", app.status);

        let saved = std::fs::read_to_string(&path).unwrap();
        let cut = |h: &str| h[..h.rfind(htmlbundle::PAYLOAD_OPEN).unwrap()].to_string();
        assert_eq!(cut(&saved), cut(&original), "engine and UI untouched");
        let b = htmlbundle::unwrap(&saved).unwrap();
        let before = htmlbundle::unwrap(&original).unwrap();
        assert_eq!(b.meta.source_sha256(), before.meta.source_sha256());
        let pkg = load_package(&b.payload).unwrap();
        assert!(
            pkg.document.body[0]
                .plain_text()
                .starts_with("Edited in docxy ")
        );

        // Saving again rewraps the file it just wrote.
        app.editor.insert_str("again ");
        app.modified = true;
        app.save();
        let again = htmlbundle::unwrap(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let pkg = load_package(&again.payload).unwrap();
        assert!(
            pkg.document.body[0]
                .plain_text()
                .starts_with("Edited in docxy again ")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_as_docx_writes_plain_ooxml() {
        let dir = temp("saveas-docx");
        let path = dir.join("sample.docx.html");
        std::fs::write(&path, bundle_of(&sample_docx(), "sample.docx")).unwrap();
        let mut app = open_app(&path);
        app.commit_save_as(dir.clone(), "out.docx".to_string());
        let bytes = std::fs::read(dir.join("out.docx")).unwrap();
        assert_eq!(&bytes[..2], b"PK");
        assert!(load_package(&bytes).is_ok());
        assert_eq!(app.format, DocFormat::Docx);
        assert!(app.bundle_html.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_as_bundle_from_an_open_bundle_keeps_its_engine() {
        let dir = temp("saveas-bundle");
        let path = dir.join("sample.docx.html");
        let original = bundle_of(&sample_docx(), "sample.docx");
        std::fs::write(&path, &original).unwrap();
        let mut app = open_app(&path);
        app.commit_save_as(dir.clone(), "copy.docx.html".to_string());
        let copy = std::fs::read_to_string(dir.join("copy.docx.html")).unwrap();
        assert!(copy.contains(&htmlbundle::base64::encode(b"\0asm stand-in")));
        assert_eq!(app.format, DocFormat::Html);
        assert!(app.path.ends_with("copy.docx.html"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_as_bundle_from_a_docx_needs_the_engine() {
        let dir = temp("saveas-new");
        let mut app = App::new(load_package(&sample_docx()).unwrap(), "sample.docx", false);
        app.path = dir.join("sample.docx").to_string_lossy().into_owned();
        app.commit_save_as(dir.clone(), "sample.docx.html".to_string());
        let out = dir.join("sample.docx.html");
        if html::can_export() {
            let b = htmlbundle::unwrap(&std::fs::read_to_string(&out).unwrap()).unwrap();
            assert_eq!(b.meta.source_name(), "sample.docx");
            assert!(load_package(&b.payload).is_ok());
            assert_eq!(app.format, DocFormat::Html);
        } else {
            assert!(!out.exists());
            let status = app.status.clone().unwrap_or_default();
            assert!(status.contains("--features html-export"), "{status}");
            assert_eq!(app.format, DocFormat::Docx);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_original_next_to_the_bundle_is_reported() {
        let dir = temp("sibling");
        let path = dir.join("sample.docx.html");
        std::fs::write(&path, bundle_of(&sample_docx(), "sample.docx")).unwrap();
        std::fs::write(dir.join("sample.docx"), sample_docx()).unwrap();
        let app = open_app(&path);
        let status = app.status.clone().unwrap_or_default();
        assert!(!status.contains("changed since export"), "{status}");

        std::fs::write(dir.join("sample.docx"), b"edited elsewhere").unwrap();
        let app = open_app(&path);
        let status = app.status.clone().unwrap_or_default();
        assert!(
            status.contains("sample.docx changed since export"),
            "{status}"
        );
        // Informational only: the original is untouched.
        assert_eq!(
            std::fs::read(dir.join("sample.docx")).unwrap(),
            b"edited elsewhere"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_bundle_retains_html_and_changed_original_warning() {
        let dir = temp("startup-sibling");
        let path = dir.join("sample.docx.html");
        std::fs::write(&path, bundle_of(&sample_docx(), "sample.docx")).unwrap();
        std::fs::write(dir.join("sample.docx"), b"edited elsewhere").unwrap();
        let input = load_input(path.to_str().unwrap()).unwrap();
        let app = startup_app(
            input.pkg,
            path.to_str().unwrap(),
            input.format,
            input.bundle,
            input.encoding,
            false,
        );
        assert_eq!(app.format, DocFormat::Html);
        assert!(app.bundle_html.is_some());
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .contains("sample.docx changed since export")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn renamed_bundles_open_and_save_as_bundles() {
        for name in ["sample.docx (1).html", "sample.docx(1).html", "notes.html"] {
            let dir = temp("renamed");
            let path = dir.join(name);
            let original = bundle_of(&sample_docx(), "sample.docx");
            std::fs::write(&path, &original).unwrap();
            let mut app = open_app(&path);
            assert_eq!(app.format, DocFormat::Html, "{name}");
            assert!(app.bundle_html.is_some(), "{name}: {:?}", app.status);
            app.editor.set_caret(Caret::top(0, 0));
            app.editor.insert_str("kept ");
            app.modified = true;
            app.save();
            // Still a bundle, never OOXML over the .html.
            let saved = std::fs::read_to_string(&path).unwrap();
            let b = htmlbundle::unwrap(&saved).unwrap();
            let pkg = load_package(&b.payload).unwrap();
            assert!(
                pkg.document.body[0].plain_text().starts_with("kept "),
                "{name}"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_plain_html_page_is_not_opened_or_overwritten() {
        let dir = temp("plain-html");
        let path = dir.join("page.html");
        std::fs::write(&path, "<html><body>hello</body></html>").unwrap();
        let err = load_input(&path.to_string_lossy()).err().unwrap();
        assert!(err.contains("not a docxy editable HTML file"), "{err}");
        let mut app = App::new(new_package(Document::default()), "untitled.docx", false);
        app.open_path(&path).unwrap_err();
        assert!(
            app.status
                .clone()
                .unwrap_or_default()
                .contains("cannot open")
        );
        assert_eq!(app.path, "untitled.docx");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "<html><body>hello</body></html>"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_as_any_html_name_writes_a_bundle() {
        let dir = temp("saveas-html");
        let path = dir.join("sample.docx.html");
        std::fs::write(&path, bundle_of(&sample_docx(), "sample.docx")).unwrap();
        let mut app = open_app(&path);
        app.commit_save_as(dir.clone(), "notes.html".to_string());
        let out = std::fs::read_to_string(dir.join("notes.html")).unwrap();
        assert!(htmlbundle::unwrap(&out).is_ok(), "a bundle, not OOXML");
        assert_eq!(app.format, DocFormat::Html);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_as_never_replaces_a_page_that_is_not_a_bundle() {
        let dir = temp("no-clobber");
        let plain = dir.join("index.html");
        let legacy = dir.join("report.htm");
        std::fs::write(&plain, "<html><body>mine</body></html>").unwrap();
        std::fs::write(&legacy, b"<html>caf\xe9</html>").unwrap();

        // From a .docx, and from an opened bundle (rewrap would not care what
        // it replaces), onto both pages.
        let bundle_path = dir.join("sample.docx.html");
        std::fs::write(&bundle_path, bundle_of(&sample_docx(), "sample.docx")).unwrap();
        for from_bundle in [false, true] {
            for (target, before) in [
                (&plain, b"<html><body>mine</body></html>".to_vec()),
                (&legacy, b"<html>caf\xe9</html>".to_vec()),
            ] {
                let mut app = if from_bundle {
                    open_app(&bundle_path)
                } else {
                    let mut app =
                        App::new(load_package(&sample_docx()).unwrap(), "sample.docx", false);
                    app.path = dir.join("sample.docx").to_string_lossy().into_owned();
                    app
                };
                let name = target.file_name().unwrap().to_string_lossy().into_owned();
                app.commit_save_as(dir.clone(), name.clone());
                let status = app.status.clone().unwrap_or_default();
                assert!(status.contains("not overwriting"), "{name}: {status}");
                assert_eq!(std::fs::read(target).unwrap(), before, "{name} replaced");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_refuses_when_the_bundle_was_replaced_by_a_plain_page() {
        let dir = temp("replaced");
        let path = dir.join("sample.docx.html");
        std::fs::write(&path, bundle_of(&sample_docx(), "sample.docx")).unwrap();
        let mut app = open_app(&path);
        std::fs::write(&path, "<html>someone else's page</html>").unwrap();
        app.modified = true;
        app.save();
        assert!(app.modified);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "<html>someone else's page</html>"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_bundle_is_refused_clearly() {
        let dir = temp("damaged");
        let path = dir.join("sample.docx.html");
        let good = bundle_of(b"PK original", "sample.docx");
        let bad = good.replace(
            &htmlbundle::base64::encode(b"PK original"),
            &htmlbundle::base64::encode(b"PK tampered"),
        );
        std::fs::write(&path, bad).unwrap();
        let err = load_input(&path.to_string_lossy()).err().unwrap();
        assert!(err.contains("integrity"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
