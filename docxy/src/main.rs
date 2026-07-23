//! `docxy` — terminal viewer/**editor** for `.docx`.
//!
//! Usage:
//!   docxy                          open the editor with a new blank document
//!   docxy <file.docx>              open in the editor
//!   docxy <file.docx> --pdf <out>  headless: export to PDF and exit
//!   docxy <in> --md <out.md>       headless: convert to Markdown and exit
//!   docxy <in> --docx <out.docx>   headless: convert to .docx and exit
//!
//! The logic lives in the pure `docxcore` crate; this binary is the TUI shell:
//! it maps `docxcore::render` lines onto ratatui, draws a caret via the render
//! line-map, and routes keys into a `docxcore::editor::Editor`.

mod backstage;
mod control;
mod mcp;
mod metafile;
mod ribbon;
mod skill;

use std::collections::HashMap;
use std::io;
use std::process::ExitCode;

// Bring the trait's methods (`extensions`, `default_save_name`, …) into scope
// for the `impl backstage::BackstageHost for App` call sites below.
use backstage::BackstageHost as _;

use docxcore::editor::{Caret, Clip, Editor, Match};
use docxcore::export::{PdfOptions, to_pdf};
#[cfg(test)]
use docxcore::load::parse_header_footer;
use docxcore::load::{Relationships, parse_rels_xml};
use docxcore::markdown::{from_markdown, to_markdown_with};
use docxcore::model::{Align, Block, Document, Hyperlink, Inline, PageGeom, Run, RunProps};
use docxcore::numbering::{Numbering, compute_markers, parse_numbering_xml};
use docxcore::package::{Package, load_package, new_markdown_package, new_package, save_package};
use docxcore::render::{
    Color as DocColor, ImageBox, Line as DocLine, LineMap, PageParts, RenderOptions,
    Span as DocSpan, Style as DocStyle, render_with_images,
};
use docxcore::serialize::blocks_to_xml;
use docxcore::styles::{StyleSheet, parse_styles_xml};
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
}

/// Pick a format from a path's extension (Markdown for `.md`/`.markdown`/`.mdown`,
/// `.docx` otherwise).
fn format_for(path: &str) -> DocFormat {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".mdown") {
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

/// Load a file into a package, parsing Markdown into a numbered package when the
/// path is a `.md`. Returns the package and the format it was read as.
fn load_input(path: &str) -> Result<(Package, DocFormat), String> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".xlsx") || lower.ends_with(".xls") {
        return Err(format!(
            "{path} is a spreadsheet, not a document — try: xlsxy {path}"
        ));
    }
    let data = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    match format_for(path) {
        DocFormat::Markdown => {
            let text = String::from_utf8_lossy(&data).into_owned();
            Ok((
                new_markdown_package(from_markdown(&text)),
                DocFormat::Markdown,
            ))
        }
        DocFormat::Docx => load_package(&data)
            .map(|p| (p, DocFormat::Docx))
            .map_err(|e| e.to_string()),
    }
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
    let (pkg, input, format) = match parsed.input {
        Some(input) => match load_input(&input) {
            Ok((pkg, fmt)) => (pkg, input, fmt),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => {
            if parsed.pdf_out.is_some() || parsed.md_out.is_some() || parsed.docx_out.is_some() {
                eprintln!("error: headless conversion (--pdf/--md/--docx) requires an input file");
                return ExitCode::from(2);
            }
            let pkg = new_package(Document {
                body: vec![Block::Paragraph(docxcore::model::Paragraph::default())],
            });
            (pkg, "untitled.docx".to_string(), DocFormat::Docx)
        }
    };

    if let Some(out) = parsed.pdf_out {
        let styles = pkg
            .part("word/styles.xml")
            .map(|b| parse_styles_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        let pdf = to_pdf(
            &pkg.document,
            &PdfOptions {
                styles: Rc::new(styles),
                ..PdfOptions::default()
            },
        );
        if let Err(e) = std::fs::write(&out, &pdf) {
            eprintln!("error: cannot write {out}: {e}");
            return ExitCode::FAILURE;
        }
        println!("wrote {out} ({} bytes)", pdf.len());
        return ExitCode::SUCCESS;
    }

    // Headless format conversion: `--md` renders the document to Markdown,
    // `--docx` writes it as a Word package. Either way `pkg` already holds the
    // document parsed from the input (Markdown opened via `new_markdown_package`,
    // `.docx` via `load_package`), so conversion is just re-serialization.
    if let Some(out) = parsed.md_out {
        let numbering = pkg
            .part("word/numbering.xml")
            .map(|b| parse_numbering_xml(std::str::from_utf8(b).unwrap_or("")))
            .unwrap_or_default();
        let markers = compute_markers(&pkg.document, &numbering);
        let md = to_markdown_with(&pkg.document, &markers);
        if let Err(e) = std::fs::write(&out, md.as_bytes()) {
            eprintln!("error: cannot write {out}: {e}");
            return ExitCode::FAILURE;
        }
        println!("wrote {out} ({} bytes)", md.len());
        return ExitCode::SUCCESS;
    }

    if let Some(out) = parsed.docx_out {
        let bytes = save_package(&pkg);
        if let Err(e) = std::fs::write(&out, &bytes) {
            eprintln!("error: cannot write {out}: {e}");
            return ExitCode::FAILURE;
        }
        println!("wrote {out} ({} bytes)", bytes.len());
        return ExitCode::SUCCESS;
    }

    match run_tui(pkg, &input, format, parsed.vim, start) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
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
           docxy --mcp                      run the MCP bridge to drive a live docxy\n  \
           docxy install skill              install the agent SKILL.md (self-onboarding)\n  \
           (Save As to a .md/.docx name converts between the two formats;\n   \
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
    help: bool,
    vim: bool,
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut input = None;
    let mut pdf_out = None;
    let mut md_out = None;
    let mut docx_out = None;
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
    matches: Vec<Match>,
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

/// What a confirmed (Yes) modal should do.
#[derive(Clone, PartialEq, Eq, Debug)]
enum ConfirmAction {
    Exit,
    /// Overwrite an existing PDF at this path during Export.
    OverwritePdf(std::path::PathBuf),
}

// The Yes/No modal itself lives in `backstage::Confirm<ConfirmAction>` (shared
// across all apps); docxy only supplies the action carried on Yes.

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
}

impl PickerKind {
    fn title(self) -> &'static str {
        match self {
            PickerKind::FontName => " Font ",
            PickerKind::FontSize => " Font Size ",
            PickerKind::FontColor => " Font Colour ",
            PickerKind::Highlight => " Highlight ",
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
        }
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
    /// The on-disk format this document is bound to (`.docx` or Markdown).
    format: DocFormat,
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
    /// Document-level notices surfaced on open (protection state, watermark text,
    /// page borders) — Word features docxy shows but doesn't render/enforce.
    doc_protection: Option<String>,
    doc_watermark: Option<String>,
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
    show_comments: bool,
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
    footer_part: Option<String>,
    /// Active header/footer focus-edit, if any.
    hf_edit: Option<HfEdit>,
    vim: Option<VimState>,
    pending_link: Option<String>,
    /// (caret, visual row) hint to disambiguate wrap boundaries during j/k.
    vrow_hint: Option<(Caret, usize)>,
    /// When true, `draw` scrolls to keep the caret visible. Cleared while the
    /// user drives the viewport directly (wheel scroll, drag-select).
    follow_caret: bool,
    /// Where a left-button press landed, so a drag can select from there.
    drag_from: Option<Caret>,
    lines: Vec<DocLine>,
    maps: Vec<LineMap>,
    /// Where each embedded image's placeholder box sits (for pixel overlay).
    images: Vec<ImageBox>,
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
    fn new(mut pkg: Package, path: &str, vim: bool) -> Self {
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
        let parts = |kind: &str| PageParts {
            default: Rc::new(load_hdr_ftr(&pkg, &rels, kind, "default")),
            first: Rc::new(load_hdr_ftr(&pkg, &rels, kind, "first")),
            even: Rc::new(load_hdr_ftr(&pkg, &rels, kind, "even")),
        };
        let headers = parts("headerReference");
        let footers = parts("footerReference");
        let title_page = flag_on(pkg.sect_pr(), "titlePg");
        let even_odd = pkg
            .part("word/settings.xml")
            .map(|b| flag_on(std::str::from_utf8(b).unwrap_or(""), "evenAndOddHeaders"))
            .unwrap_or(false);
        let header_part = hf_part_name(&pkg, &rels, "headerReference");
        let footer_part = hf_part_name(&pkg, &rels, "footerReference");
        let comments = docxcore::comments::parse_comments(&pkg);
        let notes = docxcore::notes::parse_notes(&pkg);
        // Recompute fields that depend on the clock / document properties (DATE,
        // TIME, AUTHOR, CREATEDATE, …) so they show a live value like Word does,
        // rather than the value last cached in the file.
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
        docxcore::field::recompute(&mut pkg.document, &field_ctx);
        let doc_protection = pkg.protection();
        let doc_watermark = pkg.watermark();
        let doc_page_borders = pkg.has_page_borders();
        let doc = std::mem::take(&mut pkg.document);
        App {
            pkg,
            editor: Editor::new(doc),
            path: path.to_string(),
            format: format_for(path),
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
            doc_watermark,
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
            show_comments: false,
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
            headers,
            footers,
            title_page,
            even_odd,
            header_part,
            footer_part,
            hf_edit: None,
            vim: if vim { Some(VimState::new()) } else { None },
            pending_link: None,
            vrow_hint: None,
            follow_caret: true,
            drag_from: None,
            lines: Vec::new(),
            maps: Vec::new(),
            images: Vec::new(),
            rels,
            picker: None,
            img_cache: HashMap::new(),
            rendered_width: 0,
            dirty: true,
        }
    }

    fn options(&self, width: u16) -> RenderOptions {
        // In find mode, highlight all matches; otherwise the live selection.
        let selection = match &self.find {
            Some(f) => f
                .matches
                .iter()
                .map(|m| (m.path.clone(), m.start, m.end))
                .collect(),
            None => self.editor.selection_spans(),
        };
        RenderOptions {
            width: width.max(1) as usize,
            show_invisibles: self.invisibles,
            page_view: self.page_view,
            borderless_tables: self.borderless,
            selection,
            styles: self.styles.clone(),
            list_markers: Rc::new(compute_markers(&self.editor.doc, &self.numbering)),
            page: self.pkg.page_geom(),
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
                    ribbon::Focus::Tab(_) => self.ribbon_focus = self.ribbon.enter_body(),
                    ribbon::Focus::Button(_) => {
                        if let Some((act, _)) = self.ribbon.focus_act(self.ribbon_focus) {
                            self.run_act(act);
                        }
                    }
                    ribbon::Focus::None => {}
                }
                self.dirty = true;
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

    /// Run a ribbon command, mapping it to the matching editor operation.
    fn run_act(&mut self, act: ribbon::Act) {
        use ribbon::Act::*;
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
                self.open_path(&p);
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
            BackstageEvent::Exit => {
                self.request_exit();
                self.quit_requested
            }
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
    /// extension (`.md`/`.markdown` → Markdown, `.docx` → Word; none → keep the
    /// current format). This is how a `.docx` is exported to Markdown and vice
    /// versa. Makes the new file current and closes the backstage.
    fn commit_save_as(&mut self, dir: std::path::PathBuf, name: String) {
        if name.is_empty() {
            self.status = Some("Save As — type a file name first.".to_string());
            return;
        }
        // Resolve the target format + ensure the name carries an extension.
        let lower = name.to_ascii_lowercase();
        let known = [".docx", ".md", ".markdown", ".mdown"];
        let (mut fname, target) = if known.iter().any(|e| lower.ends_with(e)) {
            let fmt = format_for(&name);
            (name, fmt)
        } else {
            let mut f = name;
            f.push_str(match self.format {
                DocFormat::Markdown => ".md",
                DocFormat::Docx => ".docx",
            });
            (f, self.format)
        };
        fname = fname.trim().to_string();
        let path = dir.join(&fname);
        let path_str = path.to_string_lossy().into_owned();
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
            DocFormat::Docx => {
                self.pkg.document = self.current_document();
                (save_package(&self.pkg), self.pkg.clone())
            }
        };
        match std::fs::write(&path, &bytes) {
            Ok(()) => {
                let n = bytes.len();
                self.load_package_state(pkg, path_str.clone());
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
    fn open_path(&mut self, path: &std::path::Path) {
        let p = path.display().to_string();
        match load_input(&p) {
            Ok((pkg, _fmt)) => {
                self.load_package_state(pkg, p.clone());
                self.status = Some(format!("opened {p}"));
            }
            Err(e) => self.status = Some(format!("cannot open {p}: {e}")),
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

    /// Render the document to a PDF at `out` and report the result in the status
    /// line. Callers handle any overwrite confirmation first.
    fn write_pdf(&mut self, out: std::path::PathBuf) {
        let pdf = to_pdf(
            &self.editor.doc,
            &PdfOptions {
                styles: self.styles.clone(),
                ..PdfOptions::default()
            },
        );
        self.status = match std::fs::write(&out, &pdf) {
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
        let parts = |kind: &str| PageParts {
            default: Rc::new(load_hdr_ftr(&pkg, &rels, kind, "default")),
            first: Rc::new(load_hdr_ftr(&pkg, &rels, kind, "first")),
            even: Rc::new(load_hdr_ftr(&pkg, &rels, kind, "even")),
        };
        self.headers = parts("headerReference");
        self.footers = parts("footerReference");
        self.title_page = flag_on(pkg.sect_pr(), "titlePg");
        self.even_odd = pkg
            .part("word/settings.xml")
            .map(|b| flag_on(std::str::from_utf8(b).unwrap_or(""), "evenAndOddHeaders"))
            .unwrap_or(false);
        self.header_part = hf_part_name(&pkg, &rels, "headerReference");
        self.footer_part = hf_part_name(&pkg, &rels, "footerReference");
        self.comments = docxcore::comments::parse_comments(&pkg);
        self.notes = docxcore::notes::parse_notes(&pkg);
        self.notes_scroll = 0;
        self.comments_scroll = 0;
        self.comment_sel = 0;
        self.comment_active = false;
        self.doc_protection = pkg.protection();
        self.doc_watermark = pkg.watermark();
        self.doc_page_borders = pkg.has_page_borders();
        let doc = std::mem::take(&mut pkg.document);
        self.pkg = pkg;
        self.editor = Editor::new(doc);
        self.styles = Rc::new(styles);
        self.numbering = Rc::new(numbering);
        self.rels = rels;
        self.format = format_for(&path);
        // Page view has no meaning for Markdown (no fixed pages).
        if self.format == DocFormat::Markdown {
            self.page_view = false;
        }
        self.md_source = false;
        self.path = path;
        self.modified = false;
        self.scroll = 0;
        self.find = None;
        self.img_cache.clear();
        self.dirty = true;
    }

    /// A compact status-line suffix for document-level notices (protection,
    /// watermark, page borders) — empty when the document has none.
    fn doc_notice(&self) -> String {
        let mut parts = Vec::new();
        if let Some(p) = &self.doc_protection {
            parts.push(format!("Protected: {p}"));
        }
        if let Some(w) = &self.doc_watermark {
            parts.push(format!("Watermark: {w}"));
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

    /// The comments review side panel: each comment's author/date, the quoted
    /// span it anchors to, and its text, scrollable with the wheel.
    /// The next free comment id (max existing + 1).
    fn next_comment_id(&self) -> i32 {
        self.comments
            .iter()
            .filter_map(|c| c.id.parse::<i32>().ok())
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

    /// Commit the new comment: wrap the selection in markers, add it to comments.xml
    /// and the live panel.
    fn commit_comment(&mut self) {
        let text = self.comment_input.take().unwrap_or_default();
        if text.trim().is_empty() {
            self.status = Some("Comment cancelled (empty)".to_string());
            self.dirty = true;
            return;
        }
        let quoted = self.editor.selection_text();
        let id = self.next_comment_id();
        if !self.editor.add_comment(&id.to_string()) {
            self.status = Some("No selection to comment on".to_string());
            self.dirty = true;
            return;
        }
        let author = if self.field_ctx.props.author.trim().is_empty() {
            "docxy".to_string()
        } else {
            self.field_ctx.props.author.clone()
        };
        let initials: String = author
            .split_whitespace()
            .filter_map(|w| w.chars().next())
            .collect();
        let date = self
            .field_ctx
            .now
            .as_ref()
            .map(|d| {
                format!(
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
                    d.year, d.month, d.day, d.hour, d.min, d.sec
                )
            })
            .unwrap_or_default();
        self.pkg.add_comment(id, &author, &initials, &date, &text);
        self.comments.push(docxcore::comments::Comment {
            id: id.to_string(),
            author,
            initials,
            date,
            text,
            quoted,
        });
        self.comment_active = true;
        self.comment_sel = self.comments.len() - 1;
        self.show_comments = true;
        self.after_edit();
        self.status = Some("Comment added".to_string());
    }

    /// Delete the navigation-selected comment (markers + comments.xml + panel).
    fn delete_comment(&mut self) {
        if self.comments.is_empty() {
            self.status = Some("No comments to delete".to_string());
            self.dirty = true;
            return;
        }
        let idx = self.comment_sel.min(self.comments.len() - 1);
        let c = self.comments.remove(idx);
        self.editor.remove_comment_markers(&c.id);
        if let Ok(id) = c.id.parse::<i32>() {
            self.pkg.remove_comment(id);
        }
        self.comment_sel = idx.min(self.comments.len().saturating_sub(1));
        self.comment_active = !self.comments.is_empty();
        self.after_edit();
        self.status = Some(format!("Deleted comment by {}", c.author));
    }

    fn comment_input_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.comment_input = None;
                self.status = Some("Comment cancelled".to_string());
            }
            KeyCode::Enter => self.commit_comment(),
            KeyCode::Backspace => {
                if let Some(s) = self.comment_input.as_mut() {
                    s.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(s) = self.comment_input.as_mut() {
                    s.push(c);
                }
            }
            _ => {}
        }
        self.dirty = true;
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
            lines.push(RLine::styled(format!("▣ {who}  {date}"), hstyle));
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
            let (mut lines, mut maps, mut images, _mmd) =
                render_with_images(&self.editor.doc, &opts);
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
            self.lines = lines;
            self.maps = maps;
            self.images = images;
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
        // end of one, the start of the next). Collect every match; if a vertical
        // hint points at one of them (and is still valid for this caret), trust
        // it so up/down movement doesn't stick at the boundary. Otherwise resolve
        // to the last (lower) line, matching how a fresh caret reads.
        let mut matches: Vec<(usize, usize)> = Vec::new();
        for (i, m) in self.maps.iter().enumerate() {
            if let Some(seg) = m.seg_for(&c.path, c.offset) {
                matches.push((i, seg.col_for_offset(c.offset).unwrap_or(seg.col0)));
            }
        }
        if let Some((hint_caret, hint_row)) = &self.vrow_hint {
            if hint_caret == c {
                if let Some(m) = matches.iter().find(|(r, _)| r == hint_row) {
                    return Some(*m);
                }
            }
        }
        matches.last().copied()
    }

    fn move_vert(&mut self, down: bool) {
        let Some((row, col)) = self.caret_screen() else {
            return;
        };
        let rows: Box<dyn Iterator<Item = usize>> = if down {
            Box::new(row + 1..self.maps.len())
        } else {
            Box::new((0..row).rev())
        };
        for r in rows {
            if let Some(seg) = self.maps[r].nearest_seg(col) {
                let off = seg.offset_for_col(col);
                self.editor.caret = Caret::at(seg.path.clone(), off);
                self.editor.clamp();
                // Pin the caret to the row we navigated to so caret_screen
                // reports it there even when its offset sits on a wrap boundary.
                self.vrow_hint = Some((self.editor.caret.clone(), r));
                return;
            }
        }
    }

    fn after_edit(&mut self) {
        self.modified = true;
        self.dirty = true;
        self.status = None;
    }

    /// Enter focus-editing of the default header (or footer): park the body
    /// editor and point the main editor at the header/footer document.
    fn enter_hf_edit(&mut self, is_header: bool) {
        if self.hf_edit.is_some() {
            self.exit_hf_edit(true);
        }
        let what = if is_header { "header" } else { "footer" };
        // Resolve the part, creating one from scratch if the document has none.
        let existing = if is_header {
            self.header_part.clone()
        } else {
            self.footer_part.clone()
        };
        let part = match existing {
            Some(p) => p,
            None => match self.pkg.create_hf(is_header) {
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
            },
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
        if commit {
            let blocks = edited.doc.body;
            let rc = Rc::new(blocks.clone());
            if hf.is_header {
                self.headers.default = rc;
            } else {
                self.footers.default = rc;
            }
            let tag = if hf.is_header { "w:hdr" } else { "w:ftr" };
            if let Some(orig) = self.pkg.part(&hf.part) {
                let orig = String::from_utf8_lossy(orig).into_owned();
                let new_xml = splice_hf(&orig, &blocks, tag);
                self.pkg.set_part(&hf.part, new_xml.into_bytes());
            }
            self.modified = true;
        }
        self.page_view = hf.saved_page_view;
        self.dirty = true;
    }

    /// Insert a section break at the caret: content up to here keeps the current
    /// page geometry; the rest of the document becomes a new section with the
    /// given orientation. (Works cleanly when the caret is in the final section.)
    fn insert_section(&mut self, landscape: bool) {
        if self.hf_edit.is_some() {
            return;
        }
        let current = self.pkg.sect_pr().to_string();
        let break_sect = if current.is_empty() {
            "<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/></w:sectPr>".to_string()
        } else {
            current.clone()
        };
        if !self.editor.set_caret_section_break(Some(break_sect)) {
            self.status = Some("Can't insert a section break here.".to_string());
            self.dirty = true;
            return;
        }
        self.pkg.set_sect_pr(orient_sectpr(&current, landscape));
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
        let bytes = match self.format {
            DocFormat::Markdown => self.current_markdown().into_bytes(),
            DocFormat::Docx => {
                self.pkg.document = self.editor.doc.clone();
                save_package(&self.pkg)
            }
        };
        match std::fs::write(&path, &bytes) {
            Ok(()) => {
                self.modified = false;
                self.status = Some(format!("Saved {} ({} bytes)", path, bytes.len()));
            }
            Err(e) => self.status = Some(format!("save failed: {e}")),
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
        let clip = match os_text {
            // Our own content is still on the clipboard -> paste with full styling.
            Some(t) if Some(&t) == self.clip_text.as_ref() => self.clipboard.clone(),
            // External text -> paste as plain.
            Some(t) => Some(Clip::from_text(&t)),
            // OS clipboard unavailable -> fall back to the internal clip.
            None => self.clipboard.clone(),
        };
        if let Some(c) = clip {
            self.editor.paste(&c);
            self.after_edit();
        }
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
            }
            KeyCode::Down | KeyCode::Tab => {
                ps.sel = (ps.sel + 1).min(ps.opts.len().saturating_sub(1));
            }
            KeyCode::Enter => self.apply_paste_special(),
            KeyCode::Esc => self.paste_special = None,
            _ => {}
        }
        self.dirty = true;
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
        }
        self.dirty = true;
    }

    /// The computed value of `kind`, with a fresh clock for date/time fields.
    fn field_value(&self, kind: FieldKind) -> String {
        let mut ctx = self.field_ctx.clone();
        ctx.now = local_now();
        docxcore::field::eval_field_ctx(kind.instr(), &ctx)
            .unwrap_or_else(|| kind.fallback().to_string())
    }

    /// Build a simple field (`<w:fldSimple>`) inline with its computed value.
    fn build_field(&self, kind: FieldKind) -> Inline {
        let text = self.field_value(kind);
        let raw = format!(
            "<w:fldSimple w:instr=\"{}\"><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:fldSimple>",
            xml_esc_attr(kind.instr()),
            xml_esc_text(&text),
        );
        Inline::Field { raw, text }
    }

    fn apply_insert_field(&mut self) {
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
            KeyCode::Up | KeyCode::BackTab => d.sel = d.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => d.sel = (d.sel + 1).min(FieldKind::ALL.len() - 1),
            KeyCode::Enter => self.apply_insert_field(),
            KeyCode::Esc => self.insert_field = None,
            _ => {}
        }
        self.dirty = true;
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
        }
        self.dirty = true;
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
            KeyCode::Up | KeyCode::BackTab => d.sel = d.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => d.sel = (d.sel + 1).min(ParagraphDialog::ROWS - 1),
            KeyCode::Left => d.adjust(-1),
            KeyCode::Right => d.adjust(1),
            KeyCode::Enter => self.apply_para_dialog(),
            KeyCode::Esc => self.para_dialog = None,
            _ => {}
        }
        self.dirty = true;
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
        }
        self.dirty = true;
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
            KeyCode::Up | KeyCode::BackTab => d.sel = d.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => d.sel = (d.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Home => d.sel = 0,
            KeyCode::End => d.sel = n.saturating_sub(1),
            KeyCode::Enter => self.apply_styles_dialog(),
            KeyCode::Esc => self.styles_dialog = None,
            _ => {}
        }
        self.dirty = true;
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
        }
        self.dirty = true;
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
        if !self.editor.has_selection() {
            self.status = Some(format!("Select text first, then {}", kind.title().trim()));
            self.dirty = true;
            return;
        }
        self.font_picker = Some(FontPicker { kind, sel: 0 });
        self.dirty = true;
    }

    fn apply_picker(&mut self) {
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
        }
        self.after_edit();
        self.status = Some(format!("{}: {item}", p.kind.title().trim()));
    }

    fn picker_key(&mut self, key: KeyEvent) -> bool {
        let Some(p) = self.font_picker.as_mut() else {
            return false;
        };
        let n = p.kind.items().len();
        match key.code {
            KeyCode::Up | KeyCode::BackTab => p.sel = p.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => p.sel = (p.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Enter => self.apply_picker(),
            KeyCode::Esc => self.font_picker = None,
            _ => {}
        }
        self.dirty = true;
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
        }
        self.dirty = true;
    }

    /// The hyperlink target at a given document line and column, if any.
    fn link_at(&self, doc_line: usize, col: usize) -> Option<String> {
        let line = self.lines.get(doc_line)?;
        let mut cum = 0usize;
        for span in &line.spans {
            let w = span.text.chars().count();
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
        true
    }

    /// The caret at a screen position, if it lands on editable text.
    fn click_caret(&self, row: usize, col: usize) -> Option<Caret> {
        let doc_line = self.scroll + row;
        let seg = self.maps.get(doc_line)?.nearest_seg(col)?;
        Some(Caret::at(seg.path.clone(), seg.offset_for_col(col)))
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
                self.dirty = true;
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
                    self.editor.caret = c.clone();
                    self.editor.clear_selection();
                    self.drag_from = Some(c);
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
                    self.editor.caret = c;
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

    fn find_recompute(&mut self) {
        let Some((query, idx0)) = self.find.as_ref().map(|f| (f.query.clone(), f.idx)) else {
            return;
        };
        let matches = self.editor.find_all(&query, false);
        let idx = if idx0 < matches.len() { idx0 } else { 0 };
        if let Some(m) = matches.get(idx) {
            let m = m.clone();
            self.editor.select_match(&m);
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
        self.editor.select_match(&m);
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
                    let n = self.editor.replace_all(&q, &repl, false);
                    self.modified = true;
                    self.status = Some(format!("Replaced {n}"));
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
                    let repl = self
                        .find
                        .as_ref()
                        .and_then(|f| f.replacement.clone())
                        .unwrap_or_default();
                    self.editor.replace_current_with(&repl);
                    self.modified = true;
                    self.find_recompute();
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
                'h' => self.editor.move_left(),
                'l' => self.editor.move_right(),
                'j' => self.move_vert(true),
                'k' => self.move_vert(false),
                'w' => self.editor.move_word_right(),
                'b' => self.editor.move_word_left(),
                'e' => self.editor.move_word_end(),
                '0' | '^' => self.editor.move_home(),
                '$' => self.editor.move_end(),
                'G' => self.editor.move_doc_end(),
                _ => {}
            }
        }
        self.dirty = true;
    }

    fn vim_apply_op(&mut self, op: char, linewise: bool) {
        // Charwise visual selection is inclusive of the char under the cursor.
        if !linewise && self.vim_mode() == Some(VimMode::Visual) {
            if let Some((lo, hi)) = self.editor.selection_range() {
                self.editor.anchor = Some(lo);
                self.editor.caret = hi;
                self.editor.move_right();
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
                    self.editor.caret = lo;
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
        let count = self.vim.as_mut().map(|v| v.take_count()).unwrap_or(1);
        let op = self.vim.as_ref().and_then(|v| v.pending_op);
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
            self.dirty = true;
        }
    }

    fn vim_char(&mut self, c: char, ctrl: bool) {
        if ctrl && c == 'r' {
            let n = self.vim.as_mut().map(|v| v.take_count()).unwrap_or(1);
            for _ in 0..n {
                if self.editor.redo() {
                    self.modified = true;
                }
            }
            self.dirty = true;
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
                self.editor.move_right();
                self.vim_enter_insert();
            }
            'A' => {
                self.editor.move_end();
                self.vim_enter_insert();
            }
            'I' => {
                self.editor.move_home();
                self.vim_enter_insert();
            }
            'o' => {
                self.editor.move_end();
                self.editor.insert_newline();
                self.after_edit();
                self.vim_enter_insert();
            }
            'O' => {
                self.editor.move_home();
                self.editor.insert_newline();
                self.move_vert(false);
                self.after_edit();
                self.vim_enter_insert();
            }
            'x' => {
                let n = self.vim.as_mut().unwrap().take_count();
                for _ in 0..n {
                    self.editor.delete_forward();
                }
                self.after_edit();
            }
            'D' => {
                let s = self.editor.caret.clone();
                self.editor.move_end();
                self.editor.anchor = Some(s);
                let c = self.editor.cut();
                self.set_clip(c, false);
                self.after_edit();
            }
            'p' => self.vim_paste(false),
            'P' => self.vim_paste(true),
            'u' => {
                let n = self.vim.as_mut().unwrap().take_count();
                for _ in 0..n {
                    if self.editor.undo() {
                        self.modified = true;
                    }
                }
                self.dirty = true;
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
            KeyCode::Home => self.editor.move_home(),
            KeyCode::End => self.editor.move_end(),
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
        match key.code {
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
                    self.modified = true;
                    self.dirty = true;
                    self.status = None;
                }
            }
            KeyCode::Char('y') if ctrl => {
                if self.editor.redo() {
                    self.modified = true;
                    self.dirty = true;
                    self.status = None;
                }
            }
            KeyCode::Char(c) if !ctrl => {
                self.editor.insert_char(c);
                self.after_edit();
            }
            KeyCode::Enter => {
                // "---" / "===" / "___" … on a line becomes a horizontal rule.
                if self.editor.hrule_autoformat() {
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
            KeyCode::Tab => {
                self.editor.insert_str("    ");
                self.after_edit();
            }
            KeyCode::Left => {
                self.editor.extend_selection(shift);
                if ctrl {
                    self.editor.move_word_left();
                } else {
                    self.editor.move_left();
                }
                self.dirty = true;
            }
            KeyCode::Right => {
                self.editor.extend_selection(shift);
                if ctrl {
                    self.editor.move_word_right();
                } else {
                    self.editor.move_right();
                }
                self.dirty = true;
            }
            KeyCode::Home => {
                self.editor.extend_selection(shift);
                self.editor.move_home();
                self.dirty = true;
            }
            KeyCode::End => {
                self.editor.extend_selection(shift);
                self.editor.move_end();
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
        // Light page: black on white. In page view the page sits on a black
        // "desktop" (Word-style) — each line's page region is painted white and the
        // centering margins / inter-page gaps stay black. In continuous view there
        // is no page frame, so the whole content area is white.
        let mut para = if self.light_page {
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
            match &f.replacement {
                None => format!(
                    " Find: {}▏  ({cur}/{n})  ·  ↵/↓ next · ↑ prev · Tab→replace · Esc done",
                    f.query
                ),
                Some(repl) => {
                    let (qc, rc) = if f.editing_replacement {
                        ("", "▏")
                    } else {
                        ("▏", "")
                    };
                    format!(
                        " Replace: {}{qc} → {}{rc}  ({cur}/{n})  ·  ↵ replace · Ctrl-A all · Tab field · Esc done",
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
                    Some(msg) => format!(" {m} {dirty_mark}{}  │ {msg}", self.path),
                    None => format!(
                        " {m}  │ {dirty_mark}{}  ln {cr} col {cc}{pending}{}",
                        self.path,
                        self.doc_notice()
                    ),
                }
            }
        } else {
            match &self.status {
                Some(msg) => format!("{left} │ {msg}"),
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
        if self.styles_dialog.is_some() {
            self.draw_styles_dialog(f, f.area());
        }
        if self.font_picker.is_some() {
            self.draw_picker(f, f.area());
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
        &["docx"]
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
        match std::fs::read(path).ok().and_then(|d| load_package(&d).ok()) {
            Some(pkg) => {
                let styles = pkg
                    .part("word/styles.xml")
                    .map(|b| parse_styles_xml(std::str::from_utf8(b).unwrap_or("")))
                    .unwrap_or_default();
                let opts = RenderOptions {
                    width: w,
                    styles: Rc::new(styles),
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
        Block::Raw(_) => false,
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

fn xml_esc_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn xml_esc_attr(s: &str) -> String {
    xml_esc_text(s).replace('"', "&quot;")
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

/// Style one page-view line as a white page on a black desktop: the cells before
/// the first non-blank one (the centering margin) are painted black, and the page
/// itself — from the left border to the end of the line — is painted white with
/// black default text (coloured text keeps its colour). Fully-blank lines (the gaps
/// between pages) become all black.
fn paint_page_on_black(line: RLine<'static>) -> RLine<'static> {
    let white = |sp: RSpan<'static>| -> RSpan<'static> {
        let mut st = sp.style.bg(Color::White);
        if st.fg.is_none() {
            st = st.fg(Color::Black);
        }
        RSpan::styled(sp.content, st)
    };
    let mut in_page = false;
    let mut out: Vec<RSpan<'static>> = Vec::new();
    for span in line.spans {
        if in_page {
            out.push(white(span));
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
                out.push(white(RSpan::styled(text[i..].to_string(), span.style)));
                in_page = true;
            }
        }
    }
    RLine::from(out)
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

/// Load the default header/footer block content referenced by the section's
/// `<w:{kind}>` (kind = "headerReference" or "footerReference"). Empty if none.
/// Thin wrapper over `docxcore::load::resolve_header_footer` (shared with
/// docxwasm's `docx_ctl`, which needs the identical sectPr -> rels -> part ->
/// parse resolution for its `doc.header`/`doc.footer` verbs).
fn load_hdr_ftr(pkg: &Package, rels: &Relationships, kind: &str, wtype: &str) -> Vec<Block> {
    docxcore::load::resolve_header_footer(pkg, rels, kind, wtype)
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
    if sect.is_empty() {
        return format!("<w:sectPr>{pgsz}</w:sectPr>");
    }
    if let Some(s) = sect.find("<w:pgSz") {
        if let Some(e) = sect[s..].find("/>").map(|x| s + x + 2) {
            return format!("{}{pgsz}{}", &sect[..s], &sect[e..]);
        }
    }
    sect.replacen("</w:sectPr>", &format!("{pgsz}</w:sectPr>"), 1)
}

/// The package part name of the default header/footer (for editing/saving).
fn hf_part_name(pkg: &Package, rels: &Relationships, kind: &str) -> Option<String> {
    let rid = docxcore::load::header_footer_ref_rid(pkg.sect_pr(), kind, "default")?;
    let target = rels.target(&rid)?;
    Some(match target.strip_prefix('/') {
        Some(r) => r.to_string(),
        None => format!("word/{}", target.trim_start_matches("./")),
    })
}

/// Replace the inner content of a preserved header/footer part with serialized
/// blocks, keeping the original `<w:hdr …>` wrapper (and its namespaces).
fn splice_hf(original: &str, blocks: &[Block], tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let (Some(os), Some(ce)) = (original.find(&open), original.find(&close)) else {
        return original.to_string();
    };
    let Some(inner_start) = original[os..].find('>').map(|e| os + e + 1) else {
        return original.to_string();
    };
    if inner_start > ce {
        return original.to_string();
    }
    let mut out = String::with_capacity(original.len() + 64);
    out.push_str(&original[..inner_start]);
    out.push_str(&blocks_to_xml(blocks));
    out.push_str(&original[ce..]);
    out
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

fn run_tui(pkg: Package, path: &str, format: DocFormat, vim: bool, start: bool) -> io::Result<()> {
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

    let mut app = App::new(pkg, path, vim);
    app.format = format;
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
    use docxcore::model::{
        Block, Document, Hyperlink, Inline, ParProps, Paragraph as MPara, Run, RunProps,
    };
    use docxcore::package::new_package;
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
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
    fn doc_notice_reports_surfaced_features() {
        let body = vec![Block::Paragraph(docxcore::model::Paragraph::default())];
        let mut app = App::new(new_package(Document { body }), "a.docx", false);
        // A plain document shows nothing.
        assert_eq!(app.doc_notice(), "");
        // Each surfaced feature appears in the notice.
        app.doc_protection = Some("read-only".to_string());
        app.doc_watermark = Some("CONFIDENTIAL".to_string());
        app.doc_page_borders = true;
        let n = app.doc_notice();
        assert!(n.contains("Protected: read-only"), "{n}");
        assert!(n.contains("Watermark: CONFIDENTIAL"), "{n}");
        assert!(n.contains("Page border"), "{n}");
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
        r.set_active(5); // View
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
        let out = splice_hf(orig, &blocks, "w:hdr");
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
        let (pkg, fmt) = load_input(&md_path.to_string_lossy()).expect("load .md");
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
        let (back, _) = load_input(&docx_path.to_string_lossy()).expect("load .docx");
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
        // The ribbon has a Review tab after File, Home, Styles and Insert.
        assert_eq!(app.ribbon.tab_label(4), Some("Review"));
        // Switching to it activates the Review ribbon.
        app.ribbon.set_active(4);
        assert_eq!(app.ribbon.active_tab(), 4);
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
        assert_eq!(app.ribbon.tab_label(5), Some("View"));
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
        assert_eq!(app.editor.doc.body.len(), 2);
        // backspace at start of the new paragraph merges back
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.editor.doc.body.len(), 1);
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

    // small helper to turn a KeyCode into a no-modifier KeyEvent in asserts
    trait IntoKey {
        fn into_key(self) -> KeyEvent;
    }
    impl IntoKey for KeyCode {
        fn into_key(self) -> KeyEvent {
            KeyEvent::new(self, KeyModifiers::NONE)
        }
    }
}
