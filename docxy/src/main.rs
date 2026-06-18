//! `docxy` — terminal viewer/**editor** for `.docx`.
//!
//! Usage:
//!   docxy <file.docx>              open in the editor
//!   docxy <file.docx> --pdf <out>  headless: export to PDF and exit
//!
//! The logic lives in the pure `docxcore` crate; this binary is the TUI shell:
//! it maps `docxcore::render` lines onto ratatui, draws a caret via the render
//! line-map, and routes keys into a `docxcore::editor::Editor`.

mod metafile;

use std::collections::HashMap;
use std::io;
use std::process::ExitCode;

use docxcore::editor::{Caret, Clip, Editor, Match};
use docxcore::export::{PdfOptions, to_pdf};
use docxcore::load::{Relationships, parse_header_footer, parse_rels_xml};
use docxcore::model::{Align, Block, Document};
use docxcore::numbering::{Numbering, compute_markers, parse_numbering_xml};
use docxcore::package::{Package, load_package, save_package};
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
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as RLine, Span as RSpan, Text};
use ratatui::widgets::Paragraph;
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
    let Some(input) = parsed.input else {
        print_usage();
        return ExitCode::from(2);
    };

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
           docxy <file.docx>               open in the editor\n  \
           docxy <file.docx> --vim         open with vim keybindings\n  \
           docxy <file.docx> --pdf <out>   export to PDF and exit\n\n\
         EDITOR KEYS:\n  \
           type / Enter / Backspace / Delete    edit text\n  \
           arrows / Home / End / PgUp / PgDn     move   (Ctrl-←/→ by word)\n  \
           Shift + move                          select   (Esc clears)\n  \
           Ctrl-B bold  Ctrl-I italic  Ctrl-U underline   (over selection)\n  \
           Ctrl-L/E/R   align left / center / right\n  \
           Ctrl-A select all   Ctrl-C copy   Ctrl-X cut   Ctrl-V paste\n  \
           Ctrl-F find / replace (Tab toggles replace, Ctrl-A replaces all)\n  \
           Ctrl-S save   Ctrl-Z undo   Ctrl-Y redo   Ctrl-Q / Esc quit\n  \
           F2 page view   F3 show marks   F4 table borders\n  \
           F6 edit header   F7 edit footer   (Esc returns)\n  \
           mouse: click to move · click a link to open it · wheel to scroll"
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

struct App {
    pkg: Package,
    editor: Editor,
    path: String,
    modified: bool,
    quit_armed: bool,
    status: Option<String>,
    scroll: usize,
    viewport_h: usize,
    page_view: bool,
    invisibles: bool,
    borderless: bool,
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
        let doc = std::mem::take(&mut pkg.document);
        App {
            pkg,
            editor: Editor::new(doc),
            path: path.to_string(),
            modified: false,
            quit_armed: false,
            status: None,
            scroll: 0,
            viewport_h: 1,
            page_view: false,
            invisibles: false,
            borderless: false,
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

    /// Ensure `img_cache[rid]` holds a protocol encoding exactly the visible
    /// window `(wtop, wh, w)` (box-relative cells) of the image scaled to its
    /// full box. `None` is cached when the bytes are missing or undecodable
    /// (e.g. WMF/EMF) so the placeholder box stays.
    fn refresh_image(
        &mut self,
        rid: &str,
        bc: usize,
        br: usize,
        fw: usize,
        fh: usize,
        win: (usize, usize, usize),
    ) {
        let Some(picker) = self.picker.as_ref() else {
            return;
        };
        let (wtop, wh, w) = win;
        let rebuild = match self.img_cache.get(rid) {
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
                self.img_cache.insert(rid.to_string(), None);
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
            self.img_cache.insert(rid.to_string(), entry);
            return;
        }
        if let Some(Some(st)) = self.img_cache.get_mut(rid) {
            if st.win != win {
                if let Some(proto) = encode(picker, &st.resized) {
                    st.proto = proto;
                    st.win = win;
                }
            }
        }
    }

    /// Overlay decoded image pixels onto their placeholder boxes, cropping (not
    /// scaling) at the viewport edges as they scroll.
    fn draw_images(&mut self, f: &mut Frame, content: Rect) {
        let (fw, fh) = match &self.picker {
            Some(p) => {
                let fs = p.font_size();
                (fs.0 as usize, fs.1 as usize)
            }
            None => return,
        };
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
            self.refresh_image(&ib.rid, ib.cols, ib.rows, fw, fh, (wtop, wh, w));
            if let Some(Some(st)) = self.img_cache.get(&ib.rid) {
                let rect = Rect {
                    x,
                    y,
                    width: w as u16,
                    height: wh as u16,
                };
                f.render_widget(Image::new(&st.proto), rect);
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

    fn try_quit(&mut self) -> bool {
        if self.modified && !self.quit_armed {
            self.quit_armed = true;
            self.status = Some(
                "Unsaved changes — Ctrl-S to save, or press quit again to discard".to_string(),
            );
            false
        } else {
            true
        }
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

    /// The caret at a screen position, if it lands on editable text.
    fn click_caret(&self, row: usize, col: usize) -> Option<Caret> {
        let doc_line = self.scroll + row;
        let seg = self.maps.get(doc_line)?.nearest_seg(col)?;
        Some(Caret::at(seg.path.clone(), seg.offset_for_col(col)))
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let row = m.row as usize;
        let col = m.column as usize;
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if row >= self.viewport_h {
                    return; // status bar
                }
                self.follow_caret = true;
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
            MouseEventKind::ScrollDown => {
                // Scrolling changes only the visible slice, not the document, so
                // don't mark dirty (that would re-render the whole doc per tick).
                self.follow_caret = false;
                let max = self.lines.len().saturating_sub(self.viewport_h);
                self.scroll = (self.scroll + 3).min(max);
            }
            MouseEventKind::ScrollUp => {
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
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let is_quit = key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('q'));
        if !is_quit {
            self.quit_armed = false;
        }
        match key.code {
            KeyCode::Esc => {
                if self.editor.has_selection() {
                    self.editor.clear_selection();
                    self.dirty = true;
                    return false;
                }
                return self.try_quit();
            }
            KeyCode::Char('q') if ctrl => return self.try_quit(),
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
                self.dirty = true;
            }
            KeyCode::F(3) => {
                self.invisibles = !self.invisibles;
                self.dirty = true;
            }
            KeyCode::F(4) => {
                self.borderless = !self.borderless;
                self.dirty = true;
            }
            KeyCode::F(6) => self.enter_hf_edit(true),
            KeyCode::F(7) => self.enter_hf_edit(false),
            _ => {}
        }
        false
    }

    fn draw(&mut self, f: &mut Frame) {
        let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(f.area());
        let content = chunks[0];
        let status = chunks[1];

        self.viewport_h = content.height.max(1) as usize;
        self.ensure_rendered(content.width);

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
        let text = Text::from(visible.iter().map(doc_line_to_ratatui).collect::<Vec<_>>());
        f.render_widget(Paragraph::new(text), content);

        // Overlay real image pixels onto the placeholder boxes. Each image is
        // encoded once per visible window and just re-emitted as it moves, so
        // this stays cheap while scrolling (the loop caps the redraw rate).
        self.draw_images(f, content);

        // Caret.
        if let Some((row, col)) = caret {
            if row >= self.scroll && row < self.scroll + self.viewport_h {
                let x = content.x + (col.min(content.width.saturating_sub(1) as usize)) as u16;
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
            if let Some(c) = st.color {
                style = style.fg(map_color(c));
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
            false
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
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
    fn quit_guard_requires_confirmation_when_modified() {
        let mut app = app_with(&["a"]);
        app.editor.move_end();
        app.on_key(key(KeyCode::Char('x')));
        assert!(app.modified);
        // first quit is blocked
        assert!(!app.on_key(KeyCode::Esc.into_key()));
        assert!(app.quit_armed);
        // second quit goes through
        assert!(app.on_key(KeyCode::Esc.into_key()));
    }

    #[test]
    fn clean_quit_is_immediate() {
        let mut app = app_with(&["a"]);
        assert!(app.on_key(KeyCode::Esc.into_key()));
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
    fn esc_clears_selection_then_quits() {
        let mut app = app_with(&["ab"]);
        app.editor.move_home();
        app.on_key(shift(KeyCode::Right));
        assert!(app.editor.has_selection());
        assert!(!app.on_key(KeyCode::Esc.into_key())); // clears selection, does not quit
        assert!(!app.editor.has_selection());
        assert!(app.on_key(KeyCode::Esc.into_key())); // now quits
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
