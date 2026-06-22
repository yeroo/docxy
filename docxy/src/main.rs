//! `docxy` — terminal viewer/**editor** for `.docx`.
//!
//! Usage:
//!   docxy                          open the editor with a new blank document
//!   docxy <file.docx>              open in the editor
//!   docxy <file.docx> --pdf <out>  headless: export to PDF and exit
//!
//! The logic lives in the pure `docxcore` crate; this binary is the TUI shell:
//! it maps `docxcore::render` lines onto ratatui, draws a caret via the render
//! line-map, and routes keys into a `docxcore::editor::Editor`.

mod backstage;
mod metafile;
mod ribbon;

use std::collections::HashMap;
use std::io;
use std::process::ExitCode;

use docxcore::editor::{Caret, Clip, Editor, Match};
use docxcore::export::{PdfOptions, to_pdf};
use docxcore::load::{Relationships, parse_header_footer, parse_rels_xml};
use docxcore::model::{Align, Block, Document, PageGeom};
use docxcore::numbering::{Numbering, compute_markers, parse_numbering_xml};
use docxcore::package::{Package, load_package, new_package, save_package};
use docxcore::render::{
    Color as DocColor, ImageBox, Line as DocLine, LineMap, PageParts, RenderOptions,
    render_with_images,
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
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
    // With a file argument we load it; with none, start a fresh blank document.
    let (pkg, input) = match parsed.input {
        Some(input) => {
            let data = match std::fs::read(&input) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("error: cannot read {input}: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let pkg = match load_package(&data) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
            };
            (pkg, input)
        }
        None => {
            if parsed.pdf_out.is_some() {
                eprintln!("error: --pdf requires an input file");
                return ExitCode::from(2);
            }
            let pkg = new_package(Document {
                body: vec![Block::Paragraph(docxcore::model::Paragraph::default())],
            });
            (pkg, "untitled.docx".to_string())
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

    match run_tui(pkg, &input, parsed.vim) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "Docxy — terminal .docx viewer & editor\n\n\
         USAGE:\n  \
           docxy                           new blank document in the editor\n  \
           docxy <file.docx>               open in the editor\n  \
           docxy <file.docx> --vim         open with vim keybindings\n  \
           docxy <file.docx> --pdf <out>   export to PDF and exit\n\n\
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
    help: bool,
    vim: bool,
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut input = None;
    let mut pdf_out = None;
    let mut help = false;
    let mut vim = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => help = true,
            "--vim" => vim = true,
            "--pdf" => {
                i += 1;
                match args.get(i) {
                    Some(p) => pdf_out = Some(p.clone()),
                    None => return Err("error: --pdf requires an output path".to_string()),
                }
            }
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum ConfirmAction {
    Exit,
}

/// A modal Yes/No dialog.
struct Confirm {
    prompt: String,
    /// The selected button: true = Yes, false = No.
    yes: bool,
    action: ConfirmAction,
}

struct App {
    pkg: Package,
    editor: Editor,
    path: String,
    modified: bool,
    /// Set when the File ▸ Exit item is chosen, so the event loop quits.
    quit_requested: bool,
    status: Option<String>,
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
    /// Review comments parsed from the document, and whether the side panel that
    /// lists them is shown.
    comments: Vec<docxcore::comments::Comment>,
    show_comments: bool,
    /// First comment row shown in the panel (scroll offset).
    comments_scroll: usize,
    /// The comments panel rect (set by draw() for wheel hit-testing).
    comments_rect: Rect,
    /// In page view, how far the canvas is scrolled right to reveal the comments
    /// that sit beside the (un-shrunk) page. 0 = comments off-screen.
    comments_hscroll: usize,
    /// The horizontal scroll applied to the document this frame (= comments_hscroll
    /// when comments sit aside, else 0). Used to map the caret and mouse columns.
    doc_hscroll: u16,
    /// The full-screen File backstage, when open.
    backstage: Option<backstage::Backstage>,
    /// Preview pane size (cells), set by draw() so the preview fills/scrolls it.
    bs_preview_w: usize,
    bs_preview_h: usize,
    /// Index of the first file row shown (for mapping a click to an entry).
    bs_list_start: usize,
    /// Save As name-box geometry (screen cells), set by draw() for hit-testing:
    /// the editable text row, the first name char's x, and the box's top y.
    bs_name_y: u16,
    bs_name_x0: u16,
    bs_name_top: u16,
    /// The Save As "Save" button rect (set by draw() for mouse hit-testing).
    bs_save_btn: Rect,
    /// A modal Yes/No confirmation (e.g. Exit). The two button rects are stored
    /// each frame by draw() for mouse hit-testing.
    confirm: Option<Confirm>,
    confirm_btns: [Rect; 2],
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
        let doc = std::mem::take(&mut pkg.document);
        App {
            pkg,
            editor: Editor::new(doc),
            path: path.to_string(),
            modified: false,
            quit_requested: false,
            status: None,
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
            comments,
            show_comments: false,
            comments_scroll: 0,
            comments_rect: Rect::default(),
            comments_hscroll: 0,
            doc_hscroll: 0,
            // Set each frame by draw(); 0 until then so mouse rows map directly.
            ribbon_h: 0,
            backstage: None,
            bs_preview_w: 36,
            bs_preview_h: 20,
            bs_list_start: 0,
            bs_name_y: 0,
            bs_name_x0: 0,
            bs_name_top: 0,
            bs_save_btn: Rect::default(),
            confirm: None,
            confirm_btns: [Rect::default(); 2],
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
            headers: self.headers.clone(),
            footers: self.footers.clone(),
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
        }
        .save();
    }

    // ---- ribbon ----

    /// Rows the ribbon currently occupies: 1 for the collapsed tab strip, or the
    /// strip + body + yellow hint bar when expanded.
    fn ribbon_height(&self) -> usize {
        if self.ribbon_open { 1 + 5 + 1 } else { 1 }
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

    /// Run a ribbon command, mapping it to the matching editor operation.
    fn run_act(&mut self, act: ribbon::Act) {
        use ribbon::Act::*;
        match act {
            Cut => self.do_cut(),
            Copy => self.do_copy(),
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
            Todo(name) => {
                self.status = Some(format!("{name} — not implemented yet"));
                self.dirty = true;
            }
        }
    }

    /// Set print-layout (page) view on/off and persist it.
    fn set_page_view(&mut self, on: bool) {
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
            lines.push(
                self.ribbon
                    .render_hint(self.ribbon_focus, self.ribbon.width()),
            );
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
        self.backstage = Some(backstage::Backstage::open(dir));
        self.ribbon_focus = ribbon::Focus::None;
        self.ribbon_open = false;
        self.bs_update_preview();
        self.dirty = true;
    }

    fn backstage_key(&mut self, key: KeyEvent) -> bool {
        use backstage::Pane;
        let pane = match &self.backstage {
            Some(b) => b.pane,
            None => return false,
        };
        if key.code == KeyCode::Esc {
            self.backstage = None;
            self.dirty = true;
            return false;
        }
        // The Save As dialog has its own typed-filename handling.
        if pane == Pane::SaveAs {
            return self.save_as_key(key);
        }
        match pane {
            Pane::Menu => match key.code {
                KeyCode::Up => self.bs_menu_move(false),
                KeyCode::Down => self.bs_menu_move(true),
                KeyCode::Enter | KeyCode::Right => return self.bs_menu_activate(),
                _ => {}
            },
            Pane::Browser => match key.code {
                KeyCode::Up => {
                    if let Some(b) = self.backstage.as_mut() {
                        b.move_sel(false);
                    }
                    self.bs_update_preview();
                }
                KeyCode::Down => {
                    if let Some(b) = self.backstage.as_mut() {
                        b.move_sel(true);
                    }
                    self.bs_update_preview();
                }
                KeyCode::Enter => {
                    let path = self.backstage.as_mut().and_then(|b| b.enter());
                    if let Some(p) = path {
                        self.open_path(&p);
                        self.backstage = None;
                    } else {
                        self.bs_update_preview();
                    }
                }
                KeyCode::Backspace => {
                    if let Some(b) = self.backstage.as_mut() {
                        b.go_up();
                    }
                    self.bs_update_preview();
                }
                KeyCode::Left => {
                    if let Some(b) = self.backstage.as_mut() {
                        b.pane = Pane::Menu;
                    }
                }
                // Step right into the read-only preview to scroll it.
                KeyCode::Right | KeyCode::Tab => {
                    if let Some(b) = self.backstage.as_mut() {
                        if !b.preview.is_empty() {
                            b.pane = Pane::Preview;
                        }
                    }
                }
                _ => {}
            },
            Pane::Preview => {
                let page = self.bs_preview_h.saturating_sub(1).max(1) as isize;
                match key.code {
                    KeyCode::Up => self.bs_scroll_preview(-1),
                    KeyCode::Down => self.bs_scroll_preview(1),
                    KeyCode::PageUp => self.bs_scroll_preview(-page),
                    KeyCode::PageDown => self.bs_scroll_preview(page),
                    KeyCode::Home => self.bs_scroll_preview(isize::MIN / 2),
                    KeyCode::End => self.bs_scroll_preview(isize::MAX / 2),
                    KeyCode::Left | KeyCode::Tab => {
                        if let Some(b) = self.backstage.as_mut() {
                            b.pane = Pane::Browser;
                        }
                    }
                    _ => {}
                }
            }
            // Handled above by save_as_key; here only to keep the match total.
            Pane::SaveAs => {}
        }
        self.dirty = true;
        false
    }

    /// Route a left-click inside the File backstage. The layout is fixed:
    /// menu column x<14, then (for Open) the file list x 14..48, then the
    /// preview. Clicking a row selects it; clicking the already-selected row
    /// activates it (open the file / step into the folder).
    fn bs_mouse(&mut self, x: u16, y: u16) {
        use backstage::{ITEMS, Item, Pane};
        // Left menu column. The navigational items (Open / Save As / Info) just
        // switch the right pane, so a single click engages them right away —
        // Save As prefills the name and lets you type immediately. The acting
        // items (New / Save / Print / Export / Exit) need a second click on the
        // already-selected row, so a stray click can't discard or quit.
        if x < 14 {
            if y >= 1 {
                let idx = (y - 1) as usize;
                if idx < ITEMS.len() {
                    let it = ITEMS[idx];
                    let cur = self.backstage.as_ref().map(|b| b.item);
                    let navigational = matches!(it, Item::Open | Item::SaveAs | Item::Info);
                    if let Some(b) = self.backstage.as_mut() {
                        b.item = it;
                    }
                    if navigational || cur == Some(it) {
                        let _ = self.bs_menu_activate();
                    } else if let Some(b) = self.backstage.as_mut() {
                        b.pane = Pane::Menu;
                    }
                    self.bs_update_preview();
                    self.dirty = true;
                }
            }
            return;
        }
        let (item, pane) = match &self.backstage {
            Some(b) => (b.item, b.pane),
            None => return,
        };
        // The Save As dialog has two pieces; clicking one focuses it and
        // deactivates the other.
        if pane == Pane::SaveAs {
            // The Save button (a clickable Enter).
            if self.bs_save_btn.contains(Position { x, y }) {
                self.commit_save_as();
                self.dirty = true;
                return;
            }
            // Click inside the name box (its 3 bottom rows): focus the field and
            // drop the caret at the clicked character.
            if y >= self.bs_name_top {
                if let Some(b) = self.backstage.as_mut() {
                    b.name_focus = true;
                    let off = x.saturating_sub(self.bs_name_x0) as usize;
                    b.name_cursor = off.min(b.name_input.chars().count());
                    self.dirty = true;
                }
                return;
            }
            // Click in the folder list: focus the browser (hiding the name caret)
            // and select the row. A folder steps in on a second click; a file
            // copies its name into the field as an overwrite target.
            if y < 2 {
                return;
            }
            let idx = self.bs_list_start + (y - 2) as usize;
            if let Some(b) = self.backstage.as_mut() {
                if idx < b.entries.len() {
                    let was_sel = idx == b.sel;
                    b.name_focus = false;
                    b.sel = idx;
                    let is_dir = b.entries[idx].is_dir;
                    if is_dir && was_sel {
                        let _ = b.enter();
                    } else if !is_dir {
                        b.name_input = b.entries[idx].name.clone();
                        b.name_cursor = b.name_input.chars().count();
                    }
                    self.dirty = true;
                }
            }
            return;
        }
        // The list/preview only exist for the Open item.
        if item != Item::Open {
            return;
        }
        if x < 48 {
            // File list: rows start below the box's top border (body y=1, +1).
            if y < 2 {
                return;
            }
            let row = (y - 2) as usize;
            let idx = self.bs_list_start + row;
            let (n, sel) = match &self.backstage {
                Some(b) => (b.entries.len(), b.sel),
                None => return,
            };
            if idx >= n {
                return;
            }
            if idx == sel {
                // Second click on the highlighted row activates it.
                let path = self.backstage.as_mut().and_then(|b| b.enter());
                if let Some(p) = path {
                    self.open_path(&p);
                    self.backstage = None;
                } else {
                    self.bs_update_preview();
                }
            } else if let Some(b) = self.backstage.as_mut() {
                b.sel = idx;
                b.pane = Pane::Browser;
                self.bs_update_preview();
            }
            self.dirty = true;
        } else {
            // Click in the preview gives it focus so the wheel/keys scroll it.
            if let Some(b) = self.backstage.as_mut() {
                b.pane = Pane::Preview;
            }
            self.dirty = true;
        }
    }

    fn bs_scroll_preview(&mut self, delta: isize) {
        let h = self.bs_preview_h.max(1);
        if let Some(b) = self.backstage.as_mut() {
            let max = b.preview.len().saturating_sub(h) as isize;
            let new = (b.preview_scroll as isize + delta).clamp(0, max.max(0));
            b.preview_scroll = new as usize;
        }
    }

    fn bs_menu_move(&mut self, down: bool) {
        if let Some(b) = self.backstage.as_mut() {
            let n = backstage::ITEMS.len();
            let i = backstage::ITEMS
                .iter()
                .position(|x| *x == b.item)
                .unwrap_or(0);
            let ni = if down {
                (i + 1).min(n - 1)
            } else {
                i.saturating_sub(1)
            };
            b.item = backstage::ITEMS[ni];
        }
    }

    /// Activate the selected menu item. Returns true only if the app should quit.
    fn bs_menu_activate(&mut self) -> bool {
        use backstage::{Item, Pane};
        let item = match &self.backstage {
            Some(b) => b.item,
            None => return false,
        };
        match item {
            Item::Open => {
                if let Some(b) = self.backstage.as_mut() {
                    b.pane = Pane::Browser;
                }
                self.bs_update_preview();
            }
            Item::Info => {} // the Info pane is shown on the right; nothing to do
            Item::Save => {
                self.save();
                self.backstage = None;
            }
            Item::SaveAs => {
                // Prefill the current file's name with the caret at its end.
                let base = std::path::Path::new(&self.path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "untitled.docx".to_string());
                if let Some(b) = self.backstage.as_mut() {
                    b.name_cursor = base.chars().count();
                    b.name_input = base;
                    b.name_focus = true;
                    b.pane = Pane::SaveAs;
                }
            }
            Item::New => {
                self.new_document();
                self.backstage = None;
            }
            Item::Print | Item::Export => {
                self.export_pdf();
                self.backstage = None;
            }
            Item::Exit => self.request_exit(),
        }
        self.dirty = true;
        self.quit_requested
    }

    /// Keys for the Save As dialog. Tab moves focus between the file-name field
    /// and the folder browser; each piece only reacts when it's focused. Enter
    /// saves, Esc (handled by the caller) cancels.
    fn save_as_key(&mut self, key: KeyEvent) -> bool {
        let name_focus = self
            .backstage
            .as_ref()
            .map(|b| b.name_focus)
            .unwrap_or(true);
        match key.code {
            KeyCode::Enter => {
                self.commit_save_as();
                self.dirty = true;
                return false;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if let Some(b) = self.backstage.as_mut() {
                    b.name_focus = !b.name_focus;
                }
                self.dirty = true;
                return false;
            }
            _ => {}
        }
        if name_focus {
            self.save_as_name_key(key);
        } else {
            self.save_as_browser_key(key);
        }
        self.dirty = true;
        false
    }

    /// Editing keys while the file-name field is focused.
    fn save_as_name_key(&mut self, key: KeyEvent) {
        let Some(b) = self.backstage.as_mut() else {
            return;
        };
        let len = b.name_input.chars().count();
        match key.code {
            KeyCode::Char(c) => {
                let at = byte_index(&b.name_input, b.name_cursor);
                b.name_input.insert(at, c);
                b.name_cursor += 1;
            }
            KeyCode::Backspace => {
                if b.name_cursor > 0 {
                    let start = byte_index(&b.name_input, b.name_cursor - 1);
                    let end = byte_index(&b.name_input, b.name_cursor);
                    b.name_input.replace_range(start..end, "");
                    b.name_cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if b.name_cursor < len {
                    let start = byte_index(&b.name_input, b.name_cursor);
                    let end = byte_index(&b.name_input, b.name_cursor + 1);
                    b.name_input.replace_range(start..end, "");
                }
            }
            KeyCode::Left => b.name_cursor = b.name_cursor.saturating_sub(1),
            KeyCode::Right => b.name_cursor = (b.name_cursor + 1).min(len),
            KeyCode::Home => b.name_cursor = 0,
            KeyCode::End => b.name_cursor = len,
            _ => {}
        }
    }

    /// Navigation keys while the folder browser is focused. Picking a file copies
    /// its name into the field and returns focus there.
    fn save_as_browser_key(&mut self, key: KeyEvent) {
        let Some(b) = self.backstage.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Up => b.move_sel(false),
            KeyCode::Down => b.move_sel(true),
            KeyCode::Left => b.go_up(),
            KeyCode::Right => {
                if b.selected().map(|e| e.is_dir).unwrap_or(false) {
                    let _ = b.enter();
                } else if let Some(e) = b.selected() {
                    let name = e.name.clone();
                    b.name_cursor = name.chars().count();
                    b.name_input = name;
                    b.name_focus = true;
                }
            }
            _ => {}
        }
    }

    /// Apply the modal's choice. Returns true if the app should quit.
    fn apply_confirm(&mut self) -> bool {
        self.dirty = true;
        if let Some(c) = self.confirm.take() {
            if c.yes {
                match c.action {
                    ConfirmAction::Exit => {
                        self.quit_requested = true;
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Keys for a Yes/No modal: ←/→/Tab move, y/n choose, Enter confirms,
    /// Esc cancels.
    fn confirm_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                if let Some(c) = self.confirm.as_mut() {
                    c.yes = !c.yes;
                }
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(c) = self.confirm.as_mut() {
                    c.yes = true;
                }
                return self.apply_confirm();
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.confirm = None;
            }
            KeyCode::Enter => return self.apply_confirm(),
            _ => {}
        }
        self.dirty = true;
        false
    }

    /// Click on a modal's Yes/No button. Returns true if the app should quit.
    fn confirm_mouse(&mut self, x: u16, y: u16) -> bool {
        let p = Position { x, y };
        if self.confirm_btns[0].contains(p) {
            if let Some(c) = self.confirm.as_mut() {
                c.yes = true;
            }
            return self.apply_confirm();
        }
        if self.confirm_btns[1].contains(p) {
            self.confirm = None;
            self.dirty = true;
        }
        false
    }

    /// Write the document to `dir/name_input` (adding `.docx` if absent), make it
    /// the current file, and close the backstage.
    fn commit_save_as(&mut self) {
        let (dir, name) = match &self.backstage {
            Some(b) => (b.dir.clone(), b.name_input.trim().to_string()),
            None => return,
        };
        if name.is_empty() {
            self.status = Some("Save As — type a file name first.".to_string());
            return;
        }
        let mut fname = name;
        if !fname.to_ascii_lowercase().ends_with(".docx") {
            fname.push_str(".docx");
        }
        let path = dir.join(&fname);
        if self.hf_edit.is_some() {
            self.exit_hf_edit(true);
        }
        self.pkg.document = self.editor.doc.clone();
        let bytes = save_package(&self.pkg);
        match std::fs::write(&path, &bytes) {
            Ok(()) => {
                self.path = path.to_string_lossy().into_owned();
                self.modified = false;
                self.backstage = None;
                self.status = Some(format!("Saved {} ({} bytes)", self.path, bytes.len()));
            }
            Err(e) => self.status = Some(format!("save failed: {e}")),
        }
    }

    /// Render a quick preview of the highlighted `.docx` into the backstage.
    fn bs_update_preview(&mut self) {
        let w = self.bs_preview_w.max(8);
        let sel = self.backstage.as_ref().and_then(|b| b.selected_docx());
        let Some(b) = self.backstage.as_mut() else {
            return;
        };
        // Re-render when the highlighted file or the pane width changes.
        if sel == b.preview_path && w == b.preview_w {
            return;
        }
        if sel != b.preview_path {
            b.preview_scroll = 0; // a new file starts at the top
        }
        b.preview_path = sel.clone();
        b.preview_w = w;
        b.preview.clear();
        let Some(path) = sel else { return };
        match std::fs::read(&path)
            .ok()
            .and_then(|d| load_package(&d).ok())
        {
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
                b.preview = docxcore::render::render(&pkg.document, &opts)
                    .iter()
                    .take(120)
                    .map(|l| l.plain())
                    .collect();
            }
            None => b.preview = vec!["(cannot read this file)".to_string()],
        }
    }

    /// Replace the open document with one loaded from `path`.
    fn open_path(&mut self, path: &std::path::Path) {
        match std::fs::read(path).ok().and_then(|d| load_package(&d).ok()) {
            Some(pkg) => {
                self.load_package_state(pkg, path.display().to_string());
                self.status = Some(format!("opened {}", path.display()));
            }
            None => self.status = Some(format!("cannot open {}", path.display())),
        }
    }

    fn new_document(&mut self) {
        let pkg = new_package(Document {
            body: vec![Block::Paragraph(docxcore::model::Paragraph::default())],
        });
        self.load_package_state(pkg, "untitled.docx".to_string());
        self.status = Some("new document".to_string());
    }

    fn export_pdf(&mut self) {
        let mut out = std::path::PathBuf::from(&self.path);
        out.set_extension("pdf");
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
        self.comments_scroll = 0;
        let doc = std::mem::take(&mut pkg.document);
        self.pkg = pkg;
        self.editor = Editor::new(doc);
        self.styles = Rc::new(styles);
        self.numbering = Rc::new(numbering);
        self.rels = rels;
        self.path = path;
        self.modified = false;
        self.scroll = 0;
        self.find = None;
        self.img_cache.clear();
        self.dirty = true;
    }

    fn draw_backstage(&self, f: &mut Frame, area: Rect) {
        use backstage::{Item, Pane};
        let Some(bs) = &self.backstage else { return };
        f.render_widget(Clear, area);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let accent = Style::default().fg(Color::Black).bg(Color::Cyan);
        let rev = Style::default().add_modifier(Modifier::REVERSED);

        let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
        f.render_widget(
            Paragraph::new(RLine::styled("  File      (Esc to close)", rev)),
            rows[0],
        );
        let cols = Layout::horizontal([Constraint::Length(14), Constraint::Min(10)]).split(rows[1]);

        // left menu
        let menu_focus = bs.pane == Pane::Menu;
        let menu_lines: Vec<RLine> = backstage::ITEMS
            .iter()
            .map(|it| {
                let on = *it == bs.item;
                let style = if on && menu_focus {
                    accent
                } else if on {
                    rev
                } else {
                    Style::default()
                };
                RLine::styled(format!(" {:<12}", it.label()), style)
            })
            .collect();
        f.render_widget(
            Paragraph::new(menu_lines)
                .block(RBlock::default().borders(Borders::RIGHT).border_style(dim)),
            cols[0],
        );

        // right content pane
        match bs.item {
            Item::Open => self.draw_bs_open(f, cols[1], bs),
            Item::SaveAs => self.draw_bs_save_as(f, cols[1], bs),
            Item::Info => self.draw_bs_info(f, cols[1]),
            other => {
                let msg = match other {
                    Item::Save => "Save (Ctrl+S) — write changes to the current file.",
                    Item::Print | Item::Export => "Export — write a PDF next to the document.",
                    Item::New => "New — start a blank document.",
                    Item::Exit => "Exit — quit docxy.",
                    _ => "",
                };
                f.render_widget(
                    Paragraph::new(format!("\n  {msg}\n\n  Enter to run · Esc to close")),
                    cols[1],
                );
            }
        }
    }

    fn draw_bs_open(&self, f: &mut Frame, area: Rect, bs: &backstage::Backstage) {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let accent = Style::default().fg(Color::Black).bg(Color::Cyan);
        let rev = Style::default().add_modifier(Modifier::REVERSED);
        // The preview gets most of the width; the file list is a compact column.
        let panes = Layout::horizontal([Constraint::Length(34), Constraint::Min(20)]).split(area);

        // file list (dir path as the box title)
        let title = format!(" {} ", bs.dir.display());
        let list_focus = bs.pane == backstage::Pane::Browser;
        let inner_h = panes[0].height.saturating_sub(2) as usize;
        let inner_w = panes[0].width.saturating_sub(2) as usize;
        let start = self.bs_list_start;
        let mut lines = Vec::new();
        for (i, e) in bs.entries.iter().enumerate().skip(start).take(inner_h) {
            let on = i == bs.sel;
            let label = if e.is_dir {
                format!(" {}/", e.name)
            } else {
                // Right-align the size (with its unit) and fit the name to what's
                // left, so the unit is never clipped at the pane's edge.
                let size = e.size_str();
                let name_w = inner_w.saturating_sub(size.len() + 2).max(1);
                let name = fit_width(&e.name, name_w);
                format!(" {:<name_w$} {}", name, size)
            };
            let style = if on && list_focus {
                accent
            } else if on {
                rev
            } else if e.locked {
                dim
            } else {
                Style::default()
            };
            lines.push(RLine::styled(label, style));
        }
        f.render_widget(
            Paragraph::new(lines).block(
                RBlock::default()
                    .borders(Borders::ALL)
                    .border_style(dim)
                    .title(title),
            ),
            panes[0],
        );

        // preview — a scrollable, read-only render of the highlighted document
        let prev_focus = bs.pane == backstage::Pane::Preview;
        let inner_ph = panes[1].height.saturating_sub(2) as usize;
        let scroll = bs
            .preview_scroll
            .min(bs.preview.len().saturating_sub(inner_ph));
        let prev: Vec<RLine> = bs
            .preview
            .iter()
            .skip(scroll)
            .take(inner_ph)
            .map(|s| RLine::raw(s.clone()))
            .collect();
        let pstyle = if prev_focus {
            Style::default().fg(Color::Cyan)
        } else {
            dim
        };
        f.render_widget(
            Paragraph::new(prev).block(
                RBlock::default()
                    .borders(Borders::ALL)
                    .border_style(pstyle)
                    .title(if prev_focus {
                        " Preview  (↑↓ PgUp/Dn scroll · ← list) "
                    } else {
                        " Preview "
                    }),
            ),
            panes[1],
        );
        // scrollbar on the preview's right edge
        if bs.preview.len() > inner_ph {
            let mut sb = ScrollbarState::new(bs.preview.len())
                .position(scroll)
                .viewport_content_length(inner_ph);
            f.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None),
                panes[1].inner(ratatui::layout::Margin {
                    vertical: 1,
                    horizontal: 0,
                }),
                &mut sb,
            );
        }
    }

    fn draw_bs_save_as(&self, f: &mut Frame, area: Rect, bs: &backstage::Backstage) {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let accent = Style::default().fg(Color::Black).bg(Color::Cyan);
        let rev = Style::default().add_modifier(Modifier::REVERSED);
        let focused = Style::default().fg(Color::Cyan);
        // The focused piece gets a cyan border; the other is dimmed.
        let (list_border, name_border) = if bs.name_focus {
            (dim, focused)
        } else {
            (focused, dim)
        };
        // Folder list on top, the typed file name in a box below it.
        let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).split(area);

        // folder list (only subfolders matter for choosing a destination)
        let title = format!(" {} ", bs.dir.display());
        let inner_h = rows[0].height.saturating_sub(2) as usize;
        let inner_w = rows[0].width.saturating_sub(2) as usize;
        let start = self.bs_list_start;
        let mut lines = Vec::new();
        for (i, e) in bs.entries.iter().enumerate().skip(start).take(inner_h) {
            let on = i == bs.sel;
            let label = if e.is_dir {
                format!(" {}/", e.name)
            } else {
                let size = e.size_str();
                let name_w = inner_w.saturating_sub(size.len() + 2).max(1);
                format!(" {:<name_w$} {}", fit_width(&e.name, name_w), size)
            };
            // Highlight the selection strongly only when the browser is focused.
            let style = if on && !bs.name_focus {
                accent
            } else if on {
                rev
            } else if e.is_dir {
                Style::default()
            } else {
                // existing files are dimmed — they're overwrite targets, not folders
                dim
            };
            lines.push(RLine::styled(label, style));
        }
        f.render_widget(
            Paragraph::new(lines).block(
                RBlock::default()
                    .borders(Borders::ALL)
                    .border_style(list_border)
                    .title(title),
            ),
            rows[0],
        );

        // The name band is the file-name input plus a Save button on the right.
        let btn = self.bs_save_btn;
        let name_box = Rect {
            width: rows[1].width.saturating_sub(btn.width),
            ..rows[1]
        };
        // file-name input — the text is plain; the caret is the real terminal
        // cursor (same as the main editor), placed via set_cursor_position only
        // while the field is focused.
        f.render_widget(
            Paragraph::new(RLine::raw(format!(" {}", bs.name_input))).block(
                RBlock::default()
                    .borders(Borders::ALL)
                    .border_style(name_border)
                    .title(" File name  (Tab · Enter · Esc) "),
            ),
            name_box,
        );
        // Save button — clickable duplicate of Enter.
        f.render_widget(
            Paragraph::new("Save")
                .alignment(Alignment::Center)
                .block(RBlock::default().borders(Borders::ALL).border_style(accent))
                .style(accent),
            btn,
        );
        if bs.name_focus {
            // left border (1) + leading space (1) + caret column, clamped to the box
            let inner_w = name_box.width.saturating_sub(2);
            let cx = (2 + bs.name_cursor as u16).min(inner_w);
            f.set_cursor_position(Position {
                x: name_box.x + cx,
                y: name_box.y + 1,
            });
        }
    }

    /// The comments review side panel: each comment's author/date, the quoted
    /// span it anchors to, and its text, scrollable with the wheel.
    fn draw_comments_panel(&self, f: &mut Frame, area: Rect) {
        let head = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        let quote = Style::default().fg(Color::Yellow);
        let inner_w = area.width.saturating_sub(2).max(4) as usize;
        let inner_h = area.height.saturating_sub(2).max(1) as usize;

        let mut lines: Vec<RLine> = Vec::new();
        for (i, c) in self.comments.iter().enumerate() {
            if i > 0 {
                lines.push(RLine::raw(""));
            }
            let who = if c.author.is_empty() {
                "Unknown".to_string()
            } else {
                c.author.clone()
            };
            let date = c.date.split('T').next().unwrap_or("").to_string();
            lines.push(RLine::styled(format!("▣ {who}  {date}"), head));
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

    /// A centered modal Yes/No box. Stores the button rects for hit-testing.
    fn draw_confirm(&mut self, f: &mut Frame, area: Rect, prompt: &str, yes: bool) {
        let w = ((prompt.chars().count() as u16) + 6)
            .clamp(28, area.width.saturating_sub(4).max(28))
            .min(area.width);
        let h = 7u16.min(area.height);
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
            .title(" Confirm ");
        let inner = block.inner(rect);
        f.render_widget(block, rect);

        // prompt, centered above the buttons
        let prompt_area = Rect {
            height: inner.height.saturating_sub(2),
            ..inner
        };
        f.render_widget(
            Paragraph::new(prompt)
                .wrap(Wrap { trim: true })
                .alignment(Alignment::Center),
            prompt_area,
        );

        // [ Yes ] [ No ] buttons, centered on the bottom row
        let yes_lbl = "  Yes  ";
        let no_lbl = "  No  ";
        let (yw, nw) = (yes_lbl.len() as u16, no_lbl.len() as u16);
        let total = yw + 2 + nw;
        let bx = inner.x + inner.width.saturating_sub(total) / 2;
        let by = inner.y + inner.height.saturating_sub(1);
        let yes_rect = Rect {
            x: bx,
            y: by,
            width: yw,
            height: 1,
        };
        let no_rect = Rect {
            x: bx + yw + 2,
            y: by,
            width: nw,
            height: 1,
        };
        let sel = Style::default().fg(Color::Black).bg(Color::Cyan);
        let unsel = Style::default().add_modifier(Modifier::REVERSED);
        f.render_widget(
            Paragraph::new(yes_lbl).style(if yes { sel } else { unsel }),
            yes_rect,
        );
        f.render_widget(
            Paragraph::new(no_lbl).style(if yes { unsel } else { sel }),
            no_rect,
        );
        self.confirm_btns = [yes_rect, no_rect];
    }

    fn draw_bs_info(&self, f: &mut Frame, area: Rect) {
        let dim = Style::default().add_modifier(Modifier::DIM);
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
        let lines = vec![
            RLine::raw(format!("  File        {}", self.path)),
            RLine::raw(format!(
                "  Modified    {}",
                if self.modified { "yes" } else { "no" }
            )),
            RLine::raw(String::new()),
            RLine::raw(format!("  Paragraphs  {paras}")),
            RLine::raw(format!("  Words       {words}")),
            RLine::raw(format!("  Characters  {chars}")),
        ];
        f.render_widget(
            Paragraph::new(lines).block(
                RBlock::default()
                    .borders(Borders::ALL)
                    .border_style(dim)
                    .title(" Info "),
            ),
            area,
        );
    }

    fn ensure_rendered(&mut self, width: u16) {
        if self.dirty || width != self.rendered_width {
            let (lines, maps, images) = render_with_images(&self.editor.doc, &self.options(width));
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
        self.pkg.document = self.editor.doc.clone();
        let bytes = save_package(&self.pkg);
        match std::fs::write(&self.path, &bytes) {
            Ok(()) => {
                self.modified = false;
                self.status = Some(format!("Saved {} ({} bytes)", self.path, bytes.len()));
            }
            Err(e) => self.status = Some(format!("save failed: {e}")),
        }
    }

    /// Open the Exit confirmation modal (used by Ctrl+Q and File ▸ Exit).
    fn request_exit(&mut self) {
        self.backstage = None;
        self.confirm = Some(Confirm {
            prompt: if self.modified {
                "Exit docxy? Unsaved changes will be lost.".to_string()
            } else {
                "Exit docxy?".to_string()
            },
            yes: true,
            action: ConfirmAction::Exit,
        });
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
        if self.confirm.is_some() {
            if m.kind == MouseEventKind::Down(MouseButton::Left) {
                // quit (if any) propagates via quit_requested in handle_event
                self.confirm_mouse(m.column, m.row);
            }
            return;
        }
        if self.backstage.is_some() {
            match m.kind {
                MouseEventKind::Down(MouseButton::Left) => self.bs_mouse(m.column, m.row),
                MouseEventKind::ScrollDown => {
                    self.bs_scroll_preview(3);
                    self.dirty = true;
                }
                MouseEventKind::ScrollUp => {
                    self.bs_scroll_preview(-3);
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
        // A left-click in the ribbon area drives the ribbon; other events (wheel,
        // drag) fall through to the document so scrolling works anywhere.
        if mrow < self.ribbon_h {
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                self.ribbon_click(m.column, m.row);
                return;
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
                // Returning to the document collapses the on-demand ribbon and
                // hands keyboard control back to the editor.
                if self.ribbon_open {
                    self.ribbon_open = false;
                    self.ribbon_focus = ribbon::Focus::None;
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
                // A clicked link is never opened directly: show the real URL and
                // ask for confirmation, and refuse anything that isn't http/https.
                if let Some(url) = self.link_at(doc_line, col) {
                    if !url.is_empty() {
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
        // A modal confirmation owns all keys while open.
        if self.confirm.is_some() {
            return self.confirm_key(key);
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
                self.editor.insert_newline();
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
            KeyCode::F(2) => {
                self.page_view = !self.page_view;
                self.save_view_prefs();
                self.dirty = true;
            }
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
        // A confirmation modal owns the whole screen — no content behind it.
        if let Some(c) = &self.confirm {
            let (prompt, yes) = (c.prompt.clone(), c.yes);
            f.render_widget(Clear, f.area());
            self.draw_confirm(f, f.area(), &prompt, yes);
            return;
        }
        // The File backstage takes over the whole screen.
        if self.backstage.is_some() {
            // Preview pane size = total − menu(14) − list(34) − borders(2) wide,
            // − title(1) − borders(2) tall.
            self.bs_preview_w = (f.area().width as usize).saturating_sub(50).max(8);
            self.bs_preview_h = (f.area().height as usize).saturating_sub(3).max(1);
            // Save As name box: bottom 3 rows of the content column (x starts at
            // menu(14) + border(1) + space(1) = 16).
            self.bs_name_top = f.area().height.saturating_sub(3);
            self.bs_name_y = f.area().height.saturating_sub(2);
            self.bs_name_x0 = 16;
            // Save button: the rightmost 10 cells of the name band.
            self.bs_save_btn = Rect {
                x: f.area().width.saturating_sub(10),
                y: self.bs_name_top,
                width: 10,
                height: 3,
            };
            // Top file row shown, so a click can be mapped back to an entry.
            let list_h = (f.area().height as usize).saturating_sub(3).max(1);
            if let Some(b) = &self.backstage {
                self.bs_list_start = b
                    .sel
                    .saturating_sub(list_h / 2)
                    .min(b.entries.len().saturating_sub(list_h));
            }
            self.bs_update_preview();
            self.draw_backstage(f, f.area());
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
        toggles.push(if self.page_view {
            ribbon::Act::PrintLayout
        } else {
            ribbon::Act::ReadMode
        });
        if self.show_ruler {
            toggles.push(ribbon::Act::ToggleRuler);
        }
        if self.show_nav {
            toggles.push(ribbon::Act::ToggleNav);
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

        let text = Text::from(visible.iter().map(doc_line_to_ratatui).collect::<Vec<_>>());
        // Light page: black on white (fills the whole content rect). Black/auto
        // text inherits this base, coloured text keeps its colour.
        let mut para = if self.light_page {
            Paragraph::new(text).style(Style::default().fg(Color::Black).bg(Color::White))
        } else {
            Paragraph::new(text)
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
        let status_text = if let Some(url) = &self.pending_link {
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
                        " {m}  │ {dirty_mark}{}  ln {cr} col {cc}{pending}",
                        self.path
                    ),
                }
            }
        } else {
            match &self.status {
                Some(msg) => format!("{left} │ {msg}"),
                None => format!("{left}│ Ctrl-S save · Ctrl-F find · Ctrl-Q quit"),
            }
        };
        let status_widget =
            Paragraph::new(status_text).style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_widget(status_widget, status);
    }
}

fn on_off(b: bool) -> &'static str {
    if b { "on" } else { "off" }
}

/// Byte offset of char index `i` in `s` (its length if `i` is past the end).
fn byte_index(s: &str, i: usize) -> usize {
    s.char_indices().nth(i).map(|(b, _)| b).unwrap_or(s.len())
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
                    _ => {}
                }
            }
        }
        p
    }

    fn to_conf(self) -> String {
        format!(
            "page_view={}\ninvisibles={}\nborderless={}\nlight_page={}\nshow_ruler={}\nshow_nav={}\nshow_comments={}\n",
            self.page_view as u8,
            self.invisibles as u8,
            self.borderless as u8,
            self.light_page as u8,
            self.show_ruler as u8,
            self.show_nav as u8,
            self.show_comments as u8,
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
fn load_hdr_ftr(pkg: &Package, rels: &Relationships, kind: &str, wtype: &str) -> Vec<Block> {
    let Some(rid) = ref_rid(pkg.sect_pr(), kind, wtype) else {
        return Vec::new();
    };
    let Some(target) = rels.target(&rid) else {
        return Vec::new();
    };
    let name = match target.strip_prefix('/') {
        Some(r) => r.to_string(),
        None => format!("word/{}", target.trim_start_matches("./")),
    };
    let Some(bytes) = pkg.part(&name) else {
        return Vec::new();
    };
    parse_header_footer(std::str::from_utf8(bytes).unwrap_or(""), rels)
}

/// The relationship id of a section header/footer reference of the given type
/// (`default` / `first` / `even`).
fn ref_rid(sect: &str, kind: &str, wtype: &str) -> Option<String> {
    let needle = format!("<w:{kind}");
    let want = format!("w:type=\"{wtype}\"");
    let mut i = 0;
    while let Some(p) = sect[i..].find(&needle) {
        let start = i + p;
        let end = sect[start..]
            .find('>')
            .map(|e| start + e)
            .unwrap_or(sect.len());
        let el = &sect[start..end];
        if el.contains(&want) {
            return attr_value(el, "r:id");
        }
        i = (end + 1).min(sect.len());
    }
    None
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
        attr_value(&xml[p..end], "w:val").as_deref(),
        Some("false" | "0" | "off")
    )
}

fn attr_value(el: &str, key: &str) -> Option<String> {
    let k = format!("{key}=\"");
    let s = el.find(&k)? + k.len();
    let rest = &el[s..];
    let e = rest.find('"')?;
    Some(rest[..e].to_string())
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
    let rid = ref_rid(pkg.sect_pr(), kind, "default")?;
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

fn run_tui(pkg: Package, path: &str, vim: bool) -> io::Result<()> {
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
    // Restore persisted view-mode toggles and enable saving them going forward.
    let prefs = ViewPrefs::load();
    app.page_view = prefs.page_view;
    app.invisibles = prefs.invisibles;
    app.borderless = prefs.borderless;
    app.light_page = prefs.light_page;
    app.show_ruler = prefs.show_ruler;
    app.show_nav = prefs.show_nav;
    app.show_comments = prefs.show_comments;
    app.persist_prefs = true;
    // Detect the terminal's graphics capability (kitty/iTerm2/Sixel); fall back
    // to a half-block renderer if the query fails (e.g. a plain console).
    app.picker =
        Some(Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((8, 16))));
    let result = loop {
        if let Err(e) = terminal.draw(|f| app.draw(f)) {
            break Err(e);
        }
        // Gather input before the next draw, capping the redraw rate to ~30 fps so
        // a fast scroll wheel (events arriving quicker than the terminal can paint)
        // batches into one redraw per frame instead of one slow repaint per tick.
        // When idle we block for input, so there's no redraw when nothing changes.
        let drawn = std::time::Instant::now();
        let frame = std::time::Duration::from_millis(33);
        let mut quit = false;
        let mut got_event = false;
        let mut err = None;
        loop {
            let timeout = if got_event {
                match frame.checked_sub(drawn.elapsed()) {
                    Some(rem) if !rem.is_zero() => rem, // still gathering this frame
                    _ => break,                         // frame budget spent → redraw
                }
            } else {
                std::time::Duration::from_secs(3600) // idle: block for input
            };
            match event::poll(timeout) {
                Ok(false) => break, // frame budget spent or idle tick → redraw
                Ok(true) => match event::read() {
                    Ok(ev) => {
                        got_event = true;
                        if handle_event(&mut app, ev) {
                            quit = true;
                            break;
                        }
                    }
                    Err(e) => {
                        err = Some(e);
                        break;
                    }
                },
                Err(e) => {
                    err = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = err {
            break Err(e);
        }
        if quit {
            break Ok(());
        }
    };

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
        };
        let back = ViewPrefs::parse(&p.to_conf());
        assert_eq!(back.page_view, p.page_view);
        assert_eq!(back.invisibles, p.invisibles);
        assert_eq!(back.borderless, p.borderless);
        assert_eq!(back.light_page, p.light_page);
        assert_eq!(back.show_ruler, p.show_ruler);
        assert_eq!(back.show_nav, p.show_nav);
        assert_eq!(back.show_comments, p.show_comments);
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
        app.bs_menu_activate();
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
        // The ribbon has a Review tab after File and Home.
        assert_eq!(app.ribbon.tab_label(2), Some("Review"));
        // Switching to it activates the Review ribbon.
        app.ribbon.set_active(2);
        assert_eq!(app.ribbon.active_tab(), 2);
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
        assert_eq!(app.ribbon.tab_label(3), Some("View"));
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
        app.backstage = Some(backstage::Backstage::open(tmp.clone()));
        if let Some(b) = app.backstage.as_mut() {
            b.item = backstage::Item::SaveAs;
        }
        app.bs_menu_activate();
        if let Some(b) = app.backstage.as_mut() {
            b.name_input = "clicked".to_string();
        }
        // simulate the Save button geometry draw() would store, then click it
        app.bs_save_btn = Rect {
            x: 40,
            y: 20,
            width: 10,
            height: 3,
        };
        app.bs_mouse(44, 21);
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
        app.backstage = Some(backstage::Backstage::open(tmp.clone()));
        if let Some(b) = app.backstage.as_mut() {
            b.item = backstage::Item::SaveAs;
        }
        app.bs_menu_activate();
        assert!(app.backstage.as_ref().unwrap().name_focus);
        // simulate the geometry draw() would store for the name box
        app.bs_name_top = 20;
        app.bs_name_y = 21;
        app.bs_name_x0 = 16;
        // click inside the name box, 3 cells into the text -> caret at char 3
        app.bs_mouse(app.bs_name_x0 + 3, 21);
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
        app.bs_menu_activate();
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
    fn file_menu_exit_asks_to_confirm_then_quits() {
        let mut app = app_with(&["doc"]);
        app.open_backstage();
        if let Some(b) = app.backstage.as_mut() {
            b.item = backstage::Item::Exit;
            b.pane = backstage::Pane::Menu;
        }
        // Activating Exit opens a Yes/No modal instead of quitting outright.
        let quit = app.bs_menu_activate();
        assert!(!quit);
        assert!(!app.quit_requested);
        assert!(app.backstage.is_none());
        assert!(app.confirm.is_some());
        assert!(app.confirm.as_ref().unwrap().yes, "Yes is the default");
        // Esc dismisses without quitting regardless of the selection.
        app.on_key(key(KeyCode::Esc));
        assert!(app.confirm.is_none());
        assert!(!app.quit_requested);
        // Reopen and confirm with 'y'.
        app.open_backstage();
        if let Some(b) = app.backstage.as_mut() {
            b.item = backstage::Item::Exit;
        }
        app.bs_menu_activate();
        let quit = app.on_key(key(KeyCode::Char('y')));
        assert!(quit);
        assert!(app.quit_requested);
    }

    #[test]
    fn preview_pane_scrolls_and_clamps() {
        let mut app = app_with(&["doc"]);
        app.open_backstage();
        app.bs_preview_h = 5;
        if let Some(b) = app.backstage.as_mut() {
            b.preview = (0..20).map(|i| format!("line {i}")).collect();
            b.pane = backstage::Pane::Preview;
        }
        app.bs_scroll_preview(3);
        assert_eq!(app.backstage.as_ref().unwrap().preview_scroll, 3);
        // clamps at the bottom: max = len(20) - height(5) = 15
        app.bs_scroll_preview(1000);
        assert_eq!(app.backstage.as_ref().unwrap().preview_scroll, 15);
        // and at the top
        app.bs_scroll_preview(-1000);
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
        app.backstage = Some(backstage::Backstage::open(tmp.clone()));
        app.bs_list_start = 0;
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
        app.bs_menu_activate();
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
        assert!(app.confirm.as_ref().unwrap().prompt.contains("Unsaved"));
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
