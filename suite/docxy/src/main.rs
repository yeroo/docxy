//! docxy — the doc-centric desktop suite (docs / sheets / mail in tabs), on GPUI.
//!
//! A thin GPUI view over `docxcore::editor::Editor` — the lossless engine the
//! terminal docxy uses. Custom title bar hosting the document tabs + window
//! controls, an Office-style ribbon (File backstage + Home/Styles/Insert/Review/
//! View tabs of titled command groups), and a rich editable document surface.
//! Theming (Auto/Light/Dark) follows gpui-component's theme; session hot-exit
//! persists open tabs/files + the theme choice to `<config>/docxy/session.json`.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use docxcore::comments::Comment;
use docxcore::editor::{Caret, Clip, Editor};
use docxcore::package::Package;
use docxcore::model::{Align, Block, BorderKind, Document, Inline, ParBorders, Paragraph, RunProps, Table, VertAlign};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Root, Sizable, Theme, ThemeMode, TitleBar,
    button::{Button, ButtonVariants},
    h_flex,
    tooltip::Tooltip,
    v_flex,
};
use ribbonspec::{self as rs, Control};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

// ---- assets: bundle our MIT Fluent icons + gpui-component's own assets -------

#[derive(RustEmbed)]
#[folder = "assets/icons"]
#[prefix = "icons/"]
struct Icons;

/// Serves our embedded Fluent icon SVGs (`icons/*.svg`), falling back to
/// gpui-component's bundled assets for everything else.
struct DocxyAssets;
impl AssetSource for DocxyAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        if let Some(f) = Icons::get(path) {
            return Ok(Some(f.data));
        }
        gpui_component_assets::Assets.load(path)
    }
    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        gpui_component_assets::Assets.list(path)
    }
}

fn icon_svg(name: &str, size: f32, color: Hsla) -> Svg {
    svg().path(SharedString::from(format!("icons/{name}.svg"))).size(px(size)).text_color(color).flex_none()
}

/// A small Quick-Access-Toolbar icon button (Undo/Redo in the title bar).
fn qat_btn(id: &'static str, icon: &'static str, tip: &'static str, pal: Pal, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(20.))
        .rounded(px(3.))
        .cursor_pointer()
        .hover(|d| d.bg(pal.hover))
        .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
        .child(icon_svg(icon, 14., pal.fg))
        .tooltip(move |w, cx| Tooltip::new(tip).build(w, cx))
        .on_click(on_click)
}

// ---- session model ---------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
enum Kind {
    Docx,
    Xlsx,
    Look,
}

impl Kind {
    fn glyph(self) -> &'static str {
        match self {
            Kind::Docx => "\u{1F4C4}",
            Kind::Xlsx => "\u{1F4CA}",
            Kind::Look => "\u{2709}",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
enum ThemePref {
    #[default]
    Auto,
    Light,
    Dark,
}

impl ThemePref {
    fn label(self) -> &'static str {
        match self {
            ThemePref::Auto => "\u{25D1} Auto",
            ThemePref::Light => "\u{2600} Light",
            ThemePref::Dark => "\u{263D} Dark",
        }
    }
    fn next(self) -> Self {
        match self {
            ThemePref::Auto => ThemePref::Light,
            ThemePref::Light => ThemePref::Dark,
            ThemePref::Dark => ThemePref::Auto,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct PersistTab {
    kind: Kind,
    title: String,
    path: Option<String>,
    #[serde(default)]
    dirty: bool,
    /// Hot-exit sidecar `.docx` holding this tab's current (possibly unsaved)
    /// content. Restored in preference to `path` so edits survive a restart.
    #[serde(default)]
    hot: Option<String>,
    #[serde(default)]
    markdown: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct Session {
    tabs: Vec<PersistTab>,
    active: usize,
    #[serde(default)]
    theme: ThemePref,
}

fn session_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("docxy").join("session.json")
}

// ---- ribbon tabs -----------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum RibbonTab {
    Home,
    Insert,
    Review,
    View,
    /// Contextual Table Tools tab — only reachable while the caret is in a table.
    Table,
}

// ---- runtime ---------------------------------------------------------------

enum Surface {
    Doc(Editor),
    Placeholder,
}

struct DocTab {
    kind: Kind,
    title: SharedString,
    path: Option<PathBuf>,
    surface: Surface,
    dirty: bool,
    status: SharedString,
    /// Review comments anchored in this document (markers live in the body; the
    /// text/author is stored here and written to comments.xml on save).
    comments: Vec<Comment>,
    /// The original package this doc was loaded from, kept so a save re-serializes
    /// only document.xml back into it and preserves every other part (footnotes,
    /// headers/footers, images, themes, …). `None` for a new empty document.
    pkg: Option<Package>,
    /// Footnotes / endnotes parsed from the package (display-only side panel).
    notes: Vec<docxcore::notes::Note>,
    /// This tab is Markdown-backed (opened from a `.md`); Save writes Markdown.
    markdown: bool,
    /// When `Some`, the header or footer is being edited (Word's header/footer
    /// edit mode): keystrokes/clicks route to this editor instead of the body,
    /// and its blocks are serialized back into the package part on exit/save.
    hf_edit: Option<HfEdit>,
}

/// Live header/footer edit session: an editor over the parsed header/footer
/// blocks, plus the package part they came from so edits can be written back.
struct HfEdit {
    editor: Editor,
    part_name: String,
    is_header: bool,
    /// Which reference type is being edited: `"default"`, `"first"`, or `"even"`.
    variant: &'static str,
}

struct Docxy {
    tabs: Vec<DocTab>,
    active: usize,
    focus: FocusHandle,
    focused: bool,
    ribbon_tab: RibbonTab,
    ribbon_min: bool,
    backstage: bool,
    bs_new: bool,
    clip: Option<Clip>,
    theme_pref: ThemePref,
    applied: Option<ThemeMode>,
    // Find & replace bar (Ctrl+F). Self-managed text fields (no gpui-component
    // InputState entity) — keystrokes route here while `find_open`.
    find_open: bool,
    find_query: String,
    replace_text: String,
    find_field: FindField,
    find_case: bool,
    // Font-colour / highlight swatch picker (None = closed).
    picker: Option<PickKind>,
    // Scroll handle for the document body, so the caret can be kept in view.
    doc_scroll: ScrollHandle,
    // New-comment entry bar (Review ▸ New comment); routes keys while open.
    comment_open: bool,
    comment_text: String,
    // Show formatting marks (¶, tab arrows) — View ▸ Show/Hide.
    show_marks: bool,
    // Comments review side panel (View / Review ▸ Comments).
    show_comments: bool,
    // Navigation (heading outline) side panel — View ▸ Navigation.
    show_nav: bool,
    // Footnotes/endnotes side panel — Review ▸ Notes.
    show_notes: bool,
    // Print Layout: render the document on a page sheet with margins (View).
    page_view: bool,
    // Horizontal ruler with margin/indent/tab markers (View ▸ Ruler).
    show_ruler: bool,
    // In-progress drag of a ruler marker.
    ruler_drag: Option<RulerDrag>,
    // The tab-stop type placed when clicking the ruler (cycled via the corner box).
    ruler_tab: docxcore::model::TabAlign,
    // The ruler content area's left-margin screen x, written by the ruler canvas
    // each paint and read by click handlers to map a click to a tab position.
    ruler_x0: std::rc::Rc<std::cell::Cell<f32>>,
    // A text drag-selection is in progress (mouse down in the doc, not yet up).
    selecting: bool,
    // KeyTips (Alt access keys): Off, tab letters, or the active tab's commands.
    keytips: KeyTip,
    // Right-click context menu position (window coords), if open.
    context_menu: Option<Point<Pixels>>,
    // Floating mini formatting toolbar shown after a drag-selection (window coords).
    mini_bar: Option<Point<Pixels>>,
    // Document zoom factor (1.0 = 100%), controlled from the status bar.
    zoom: f32,
    // While a ruler marker is being dragged, the screen x of a vertical guide
    // line drawn down the page (Word's drag guide). None when not dragging.
    ruler_guide: Option<f32>,
}

// gpui reserves Tab / Shift-Tab for focus traversal and never delivers them to
// on_key_down, so a tab must be inserted through a bound action instead.
actions!(docxy, [InsertTabAction, OutdentAction]);

#[derive(Clone, Copy, PartialEq)]
enum KeyTip {
    Off,
    Tabs,
    Commands,
}

#[derive(Clone, Copy, PartialEq)]
enum RulerHandle {
    FirstLine,
    Left,
    Right,
    MarginLeft,
    MarginRight,
}

#[derive(Clone, Copy)]
struct RulerDrag {
    handle: RulerHandle,
    start_x: f32,
    start_indent: i32,
    start_first: i32,
    start_right: i32,
    start_ml: i32,
    start_mr: i32,
}

#[derive(Clone, Copy, PartialEq)]
enum FindField {
    Query,
    Replace,
}

#[derive(Clone, Copy, PartialEq)]
enum PickKind {
    Color,
    Highlight,
    FontName,
    FontSize,
    Field,
    Table,
    Symbol,
}

/// Table sizes offered by the Insert ▸ Table picker: (label, rows, cols).
const TABLE_PRESETS: &[(&str, usize, usize)] = &[("2×2", 2, 2), ("3×2", 3, 2), ("3×3", 3, 3), ("4×3", 4, 3), ("5×3", 5, 3), ("5×5", 5, 5)];

/// The characters offered by the Insert ▸ Symbol picker — Word's common set:
/// typographic punctuation, currency, arrows, and maths.
const SYMBOLS: &[&str] = &[
    "\u{2014}", "\u{2013}", "\u{2026}", "\u{2022}", "\u{00B7}", "\u{00A9}", "\u{00AE}", "\u{2122}",
    "\u{00B0}", "\u{00B1}", "\u{00D7}", "\u{00F7}", "\u{2260}", "\u{2248}", "\u{2264}", "\u{2265}",
    "\u{221E}", "\u{00A7}", "\u{00B6}", "\u{20AC}", "\u{00A3}", "\u{00A5}", "\u{00A2}", "\u{2190}",
    "\u{2192}", "\u{2191}", "\u{2193}", "\u{201C}", "\u{201D}", "\u{2018}", "\u{2019}", "\u{03B1}",
    "\u{03B2}", "\u{03C0}", "\u{03BC}", "\u{03A9}", "\u{2211}", "\u{221A}", "\u{2212}", "\u{2605}",
];

/// The fields offered by the Insert ▸ Field picker: (label, instruction, fallback).
const FIELDS: &[(&str, &str, &str)] = &[
    ("Date", "DATE \\@ \"M/d/yyyy\"", ""),
    ("Time", "TIME \\@ \"h:mm AM/PM\"", ""),
    ("Page", "PAGE", "1"),
    ("Pages", "NUMPAGES", "1"),
    ("Author", "AUTHOR", "docxy"),
    ("File name", "FILENAME", ""),
];

/// Everything a rendered word/atom needs to turn a click into a caret move: the
/// view handle to update and the paragraph's block path. Cheap to copy (borrows).
#[derive(Clone, Copy)]
struct Click<'a> {
    ent: &'a Entity<Docxy>,
    path: &'a [usize],
}

/// Shared state for the recursive document renderer, so caret/selection/clicks
/// resolve correctly at any nesting depth (top-level blocks and table cells).
#[derive(Clone, Copy)]
struct RenderCtx<'a> {
    caret_path: &'a [usize],
    caret_off: usize,
    spans: &'a [(Vec<usize>, usize, usize)],
    ent: &'a Entity<Docxy>,
    pal: Pal,
    marks: bool,
    zoom: f32,
    /// Whether this surface currently has the caret. When false (e.g. the body
    /// while a header/footer is being edited) no caret is drawn and clicks are
    /// inert, so the inactive surface reads as dimmed and untouchable.
    active: bool,
    /// Text measurer for tab-stop positioning.
    meas: &'a Measurer,
    /// Content width (px) when rendering header/footer paragraphs, enabling the
    /// implicit centre/right tab stops. `None` for body paragraphs.
    hf_width: Option<f32>,
}

/// Colours the document renderer needs, pulled from the active theme.
#[derive(Clone, Copy)]
struct Pal {
    fg: Hsla,
    dim: Hsla,
    border: Hsla,
    panel: Hsla,
    hover: Hsla,
    sel: Hsla,
}

const BRAND: u32 = 0x2AA79B; // teal wordmark/accent (reads on light + dark)
const LINK: u32 = 0x2f6fdb;
const FILE_FG: u32 = 0xffffff;

fn empty_doc() -> Document {
    docxcore::markdown::from_markdown("# Untitled\n\n")
}

/// A loaded document with everything a tab needs to hold and re-save it losslessly.
struct Loaded {
    doc: Document,
    comments: Vec<Comment>,
    notes: Vec<docxcore::notes::Note>,
    pkg: Option<Package>,
    markdown: bool,
    status: SharedString,
}

impl Loaded {
    fn empty(status: impl Into<SharedString>) -> Self {
        Loaded { doc: empty_doc(), comments: vec![], notes: vec![], pkg: None, markdown: false, status: status.into() }
    }
    fn into_tab(self, kind: Kind, title: SharedString, path: Option<PathBuf>, dirty: bool) -> DocTab {
        DocTab { kind, title, path, surface: Surface::Doc(Editor::new(self.doc)), dirty, status: self.status, comments: self.comments, pkg: self.pkg, notes: self.notes, markdown: self.markdown, hf_edit: None }
    }
}

fn is_markdown_path(path: &std::path::Path) -> bool {
    let l = path.to_string_lossy().to_lowercase();
    l.ends_with(".md") || l.ends_with(".markdown") || l.ends_with(".mdown")
}

/// Load a `.docx` from bytes, keeping the whole package so save stays lossless.
fn load_bytes(bytes: &[u8]) -> Loaded {
    match docxcore::package::load_package(bytes) {
        Ok(pkg) => Loaded {
            doc: pkg.document.clone(),
            comments: docxcore::comments::parse_comments(&pkg),
            notes: docxcore::notes::parse_notes(&pkg),
            pkg: Some(pkg),
            markdown: false,
            status: "loaded".into(),
        },
        Err(e) => Loaded::empty(format!("load error: {e:?}")),
    }
}

fn doc_from_path(path: &PathBuf) -> Loaded {
    match std::fs::read(path) {
        Ok(bytes) if is_markdown_path(path) => Loaded {
            doc: docxcore::markdown::from_markdown(&String::from_utf8_lossy(&bytes)),
            comments: vec![],
            notes: vec![],
            pkg: None,
            markdown: true,
            status: "loaded (markdown)".into(),
        },
        Ok(bytes) => load_bytes(&bytes),
        Err(e) => Loaded::empty(format!("read error: {e}")),
    }
}

fn sample_doc() -> Loaded {
    load_bytes(include_bytes!("../../../assets/sample.docx"))
}

fn build_surface(kind: Kind, path: Option<&PathBuf>) -> (Surface, Vec<Comment>, Vec<docxcore::notes::Note>, Option<Package>, SharedString) {
    match kind {
        Kind::Docx => match path {
            Some(p) => {
                let l = doc_from_path(p);
                (Surface::Doc(Editor::new(l.doc)), l.comments, l.notes, l.pkg, l.status)
            }
            None => (Surface::Doc(Editor::new(empty_doc())), vec![], vec![], None, "untitled".into()),
        },
        _ => (Surface::Placeholder, vec![], vec![], None, "".into()),
    }
}

fn file_name(path: &PathBuf) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled.docx".into())
}

/// Directory holding the hot-exit sidecars — one `.docx` per open Doc tab, kept in
/// sync on each persist so unsaved edits survive a restart.
fn hot_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("docxy").join("hot")
}

/// Serialize a document to `.docx` bytes, adding a numbering part when it uses
/// lists (so markers survive the round-trip and open correctly in Word).
fn doc_to_docx(doc: &Document, comments: &[Comment], base: Option<&Package>) -> Vec<u8> {
    use std::collections::HashSet;
    // With the original package in hand, re-serialize just document.xml back into
    // it — every other part (footnotes, headers/footers, images, themes, …) is
    // preserved. Otherwise build a minimal package (new/empty documents).
    let mut pkg = match base {
        Some(p) => {
            let mut p = p.clone();
            p.document = doc.clone();
            p
        }
        None => {
            let has_list = doc.body.iter().any(|b| matches!(b, Block::Paragraph(p) if p.props.num_id.is_some()));
            if has_list { docxcore::package::new_markdown_package(doc.clone()) } else { docxcore::package::new_package(doc.clone()) }
        }
    };
    // Reconcile comments.xml with the tab's comment list: the base already holds the
    // comments it was loaded with, so only remove the deleted ones and add the new.
    let existing: Vec<i32> = base.map(|p| docxcore::comments::parse_comments(p).iter().filter_map(|c| c.id.parse().ok()).collect()).unwrap_or_default();
    let current: HashSet<i32> = comments.iter().filter_map(|c| c.id.parse().ok()).collect();
    for id in &existing {
        if !current.contains(id) {
            pkg.remove_comment(*id);
        }
    }
    let existing_set: HashSet<i32> = existing.iter().copied().collect();
    for c in comments {
        if let Ok(id) = c.id.parse::<i32>() {
            if !existing_set.contains(&id) {
                pkg.add_comment(id, &c.author, &c.initials, &c.date, &c.text);
            }
        }
    }
    docxcore::package::save_package(&pkg)
}

impl Docxy {
    fn new(cx: &mut Context<Self>) -> Self {
        let session: Session = std::fs::read(session_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();

        let mut tabs = Vec::new();
        for t in &session.tabs {
            let path = t.path.as_ref().map(PathBuf::from);
            // Prefer the hot-exit sidecar (current, possibly unsaved content); fall
            // back to the real file on disk, then to an empty doc.
            let hot = t.hot.as_ref().map(PathBuf::from).filter(|p| p.exists());
            let mut tab = match (t.kind, &hot) {
                (Kind::Docx, Some(hp)) => {
                    let mut l = doc_from_path(hp);
                    l.status = if t.dirty { "unsaved — restored".into() } else { "loaded".into() };
                    l.into_tab(t.kind, t.title.clone().into(), path, t.dirty)
                }
                _ => {
                    let (surface, comments, notes, pkg, status) = build_surface(t.kind, path.as_ref());
                    let markdown = path.as_deref().map(is_markdown_path).unwrap_or(false);
                    DocTab { kind: t.kind, title: t.title.clone().into(), path, surface, dirty: t.dirty, status, comments, pkg, notes, markdown, hf_edit: None }
                }
            };
            // The hot sidecar is always .docx; restore the Markdown flag from session.
            tab.markdown = t.markdown || tab.markdown;
            tabs.push(tab);
        }
        if tabs.is_empty() {
            tabs.push(sample_doc().into_tab(Kind::Docx, "sample.docx".into(), None, false));
        }
        let active = session.active.min(tabs.len().saturating_sub(1));
        let this = Self::build(tabs, active, session.theme, cx);
        this.persist();
        this
    }

    /// Assemble the app state from ready tabs, with everything else at defaults.
    /// The disk-free half of `new`, so tests can seed a known document without
    /// touching the user's session.
    fn build(tabs: Vec<DocTab>, active: usize, theme_pref: ThemePref, cx: &mut Context<Self>) -> Self {
        Self {
            tabs,
            active,
            focus: cx.focus_handle(),
            focused: false,
            ribbon_tab: RibbonTab::Home,
            ribbon_min: false,
            backstage: false,
            bs_new: false,
            clip: None,
            theme_pref,
            applied: None,
            find_open: false,
            find_query: String::new(),
            replace_text: String::new(),
            find_field: FindField::Query,
            find_case: false,
            picker: None,
            doc_scroll: ScrollHandle::new(),
            comment_open: false,
            comment_text: String::new(),
            show_marks: false,
            show_comments: false,
            show_nav: false,
            show_notes: false,
            page_view: false,
            show_ruler: false,
            ruler_drag: None,
            ruler_tab: docxcore::model::TabAlign::Left,
            ruler_x0: std::rc::Rc::new(std::cell::Cell::new(0.0)),
            selecting: false,
            keytips: KeyTip::Off,
            context_menu: None,
            mini_bar: None,
            zoom: 1.0,
            ruler_guide: None,
        }
    }


    fn persist(&self) {
        let hd = hot_dir();
        let _ = std::fs::create_dir_all(&hd);
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                // Write the tab's live content to a sidecar so unsaved edits are
                // held across a restart (closing never prompts to save).
                let hot = if let Surface::Doc(ed) = &t.surface {
                    let p = hd.join(format!("tab-{i}.docx"));
                    std::fs::write(&p, doc_to_docx(&ed.doc, &t.comments, t.pkg.as_ref())).ok().map(|_| p.display().to_string())
                } else {
                    None
                };
                PersistTab {
                    kind: t.kind,
                    title: t.title.to_string(),
                    path: t.path.as_ref().map(|p| p.display().to_string()),
                    dirty: t.dirty,
                    hot,
                    markdown: t.markdown,
                }
            })
            .collect();
        let session = Session { tabs, active: self.active, theme: self.theme_pref };
        if let Ok(json) = serde_json::to_string_pretty(&session) {
            let p = session_path();
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, json);
        }
    }

    fn refocus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn cycle_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.theme_pref = self.theme_pref.next();
        self.applied = None; // force re-apply on next render
        self.persist();
        self.refocus(window, cx);
    }

    fn add_tab(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        let (title, surface): (SharedString, Surface) = match kind {
            Kind::Docx => ("Untitled.docx".into(), Surface::Doc(Editor::new(empty_doc()))),
            Kind::Xlsx => ("Untitled.xlsx".into(), Surface::Placeholder),
            Kind::Look => ("Inbox".into(), Surface::Placeholder),
        };
        self.tabs.push(DocTab { kind, title, path: None, surface, dirty: false, status: "new".into(), comments: vec![], pkg: None, notes: vec![], markdown: false, hf_edit: None });
        self.active = self.tabs.len() - 1;
        self.backstage = false;
        self.bs_new = false;
        self.persist();
        self.refocus(window, cx);
    }

    fn select_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i < self.tabs.len() {
            self.active = i;
            self.persist();
            self.refocus(window, cx);
        }
    }

    fn close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if i < self.active {
            self.active -= 1;
        }
        self.persist();
        self.refocus(window, cx);
    }

    fn active_editor(&mut self) -> Option<&mut Editor> {
        match self.tabs.get_mut(self.active).map(|t| &mut t.surface) {
            Some(Surface::Doc(ed)) => Some(ed),
            _ => None,
        }
    }

    /// The editor keystrokes/clicks currently drive: the header/footer editor when
    /// in HF edit mode, otherwise the document body. All editing routes through
    /// this so the same machinery serves both surfaces.
    fn edit_target(&mut self) -> Option<&mut Editor> {
        let tab = self.tabs.get_mut(self.active)?;
        if let Some(hf) = tab.hf_edit.as_mut() {
            return Some(&mut hf.editor);
        }
        match &mut tab.surface {
            Surface::Doc(ed) => Some(ed),
            _ => None,
        }
    }

    /// Whether the active tab is currently in header/footer edit mode.
    fn hf_active(&self) -> bool {
        self.tabs.get(self.active).is_some_and(|t| t.hf_edit.is_some())
    }

    /// Enter header (or footer) edit mode: resolve the existing part or create a
    /// fresh one, parse its blocks into an editor, and switch to Print Layout so
    /// the margin area is visible. No-op for markdown/package-less tabs.
    fn enter_hf(&mut self, is_header: bool, variant: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_hf(); // commit any header/footer already open
        let idx = self.active;
        let Some(tab) = self.tabs.get_mut(idx) else { return };
        if !matches!(tab.surface, Surface::Doc(_)) {
            return;
        }
        let Some(pkg) = tab.pkg.as_mut() else {
            tab.status = "Headers/footers need a .docx (not a Markdown document)".into();
            return self.refocus(window, cx);
        };
        let part_name = match hf_part_name_typed(pkg, is_header, variant) {
            Some(n) => n,
            None => match pkg.create_hf(is_header, variant) {
                Some(n) => {
                    tab.dirty = true;
                    n
                }
                None => {
                    tab.status = "Could not create the header/footer part".into();
                    return self.refocus(window, cx);
                }
            },
        };
        let blocks = parse_hf_part(pkg, &part_name);
        let doc = docxcore::model::Document { body: blocks };
        tab.hf_edit = Some(HfEdit { editor: Editor::new(doc), part_name, is_header, variant });
        self.page_view = true;
        if let Some(t) = self.tabs.get_mut(idx) {
            let region = if is_header { "header" } else { "footer" };
            let vlabel = match variant { "first" => "first-page ", "even" => "even-page ", _ => "" };
            t.status = format!("Editing {vlabel}{region} — press Esc to return to the document").into();
        }
        self.refocus(window, cx);
    }

    /// Serialize the open header/footer editor back into its package part (called
    /// on exit and before every save) so edits persist. Leaves the session open.
    fn flush_hf(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Some(hf) = tab.hf_edit.as_ref() else { return };
        let inner = docxcore::serialize::blocks_to_xml(&hf.editor.doc.body);
        let tag = if hf.is_header { "w:hdr" } else { "w:ftr" };
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
             <{tag} xmlns:w=\"{W_NS}\" xmlns:r=\"{R_NS}\" xmlns:m=\"{M_NS}\">{inner}</{tag}>"
        );
        let part_name = hf.part_name.clone();
        if let Some(pkg) = tab.pkg.as_mut() {
            pkg.set_part(&part_name, xml.into_bytes());
        }
        tab.dirty = true;
    }

    /// Leave header/footer edit mode, committing edits to the package part.
    fn exit_hf(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_hf();
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.hf_edit = None;
            tab.status = "Closed header/footer".into();
        }
        self.refocus(window, cx);
    }

    /// Toggle "Different First Page" (`<w:titlePg/>`); off while editing the
    /// first-page variant drops the edit session back to the default variant.
    fn toggle_title_pg(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_hf();
        let idx = self.active;
        let mut on = false;
        if let Some(tab) = self.tabs.get_mut(idx) {
            if let Some(pkg) = tab.pkg.as_mut() {
                on = !pkg.has_title_pg();
                pkg.set_title_pg(on);
                tab.dirty = true;
            }
        }
        if !on {
            if let Some((is_h, "first")) = self.tabs.get(idx).and_then(|t| t.hf_edit.as_ref()).map(|h| (h.is_header, h.variant)) {
                return self.enter_hf(is_h, "default", window, cx);
            }
        }
        self.refocus(window, cx);
    }

    /// Toggle "Different Odd & Even Pages" (`<w:evenAndOddHeaders/>`); off while
    /// editing the even variant drops back to the default variant.
    fn toggle_even_odd(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_hf();
        let idx = self.active;
        let mut on = false;
        if let Some(tab) = self.tabs.get_mut(idx) {
            if let Some(pkg) = tab.pkg.as_mut() {
                on = !pkg.has_even_odd();
                pkg.set_even_odd(on);
                tab.dirty = true;
            }
        }
        if !on {
            if let Some((is_h, "even")) = self.tabs.get(idx).and_then(|t| t.hf_edit.as_ref()).map(|h| (h.is_header, h.variant)) {
                return self.enter_hf(is_h, "default", window, cx);
            }
        }
        self.refocus(window, cx);
    }

    /// The contextual Header & Footer toolbar, shown while editing one. Lets the
    /// user switch region (header/footer) and variant (default/first/even) and
    /// toggle the "Different First Page" / "Different Odd & Even" section options.
    fn hf_bar(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.tabs.get(self.active);
        let hf = tab.and_then(|t| t.hf_edit.as_ref());
        let (is_header, variant) = hf.map(|h| (h.is_header, h.variant)).unwrap_or((true, "default"));
        let title_pg = tab.and_then(|t| t.pkg.as_ref()).is_some_and(|p| p.has_title_pg());
        let even_odd = tab.and_then(|t| t.pkg.as_ref()).is_some_and(|p| p.has_even_odd());
        let pill = move |id: &'static str, label: SharedString, active: bool| {
            div()
                .id(id)
                .px_2()
                .h(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .text_size(px(12.))
                .text_color(if active { hsla_u(BRAND) } else { pal.fg })
                .border_1()
                .border_color(if active { hsla_u(BRAND) } else { pal.border })
                .cursor_pointer()
                .hover(|d| d.bg(pal.hover))
                .child(label)
        };
        let check = move |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_1()
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(pal.fg)
                .hover(|d| d.text_color(hsla_u(BRAND)))
                .child(
                    div()
                        .size(px(13.))
                        .rounded(px(2.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .border_1()
                        .border_color(if on { hsla_u(BRAND) } else { pal.border })
                        .bg(if on { hsla_u(BRAND) } else { Hsla { a: 0., ..pal.fg } })
                        .when(on, |d| d.child(div().text_size(px(9.)).text_color(rgb(0xffffff)).child("\u{2713}"))),
                )
                .child(label)
        };
        let sep = || div().w(px(1.)).h(px(16.)).bg(pal.border);
        h_flex()
            .w_full()
            .items_center()
            .flex_wrap()
            .gap_2()
            .px_3()
            .py_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(div().text_size(px(11.)).text_color(pal.dim).min_w(px(96.)).child("Header & Footer"))
            .child(pill("hf-hdr", "Header".into(), is_header).on_click(cx.listener(move |t, _, w, c| t.enter_hf(true, variant, w, c))))
            .child(pill("hf-ftr", "Footer".into(), !is_header).on_click(cx.listener(move |t, _, w, c| t.enter_hf(false, variant, w, c))))
            .child(sep())
            .child(pill("hf-def", "Default".into(), variant == "default").on_click(cx.listener(move |t, _, w, c| t.enter_hf(is_header, "default", w, c))))
            .when(title_pg, |d| d.child(pill("hf-first", "First page".into(), variant == "first").on_click(cx.listener(move |t, _, w, c| t.enter_hf(is_header, "first", w, c)))))
            .when(even_odd, |d| d.child(pill("hf-even", "Even".into(), variant == "even").on_click(cx.listener(move |t, _, w, c| t.enter_hf(is_header, "even", w, c)))))
            .child(sep())
            .child(check("hf-tp", "Different First Page", title_pg).on_click(cx.listener(|t, _, w, c| t.toggle_title_pg(w, c))))
            .child(check("hf-eo", "Different Odd & Even", even_odd).on_click(cx.listener(|t, _, w, c| t.toggle_even_odd(w, c))))
            .child(div().flex_1())
            .child(pill("hf-close", "Close".into(), false).on_click(cx.listener(|t, _, w, c| t.exit_hf(w, c))))
            .into_any_element()
    }

    fn save_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flush_hf(); // commit any open header/footer edits into the package first
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Surface::Doc(editor) = &tab.surface else { return };
        // Markdown-backed tabs save as Markdown; everything else as lossless .docx.
        let bytes = if tab.markdown {
            docxcore::markdown::to_markdown(&editor.doc).into_bytes()
        } else {
            doc_to_docx(&editor.doc, &tab.comments, tab.pkg.as_ref())
        };
        let path = tab
            .path
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().join(tab.title.to_string()));
        match std::fs::write(&path, &bytes) {
            Ok(()) => {
                tab.title = file_name(&path).into();
                tab.path = Some(path.clone());
                tab.dirty = false;
                tab.status = format!("saved {} bytes → {}", bytes.len(), path.display()).into();
            }
            Err(e) => tab.status = format!("save failed: {e}").into(),
        }
        self.backstage = false;
        self.bs_new = false;
        self.persist();
        self.refocus(window, cx);
    }

    fn save_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let start = self.tabs.get(self.active).map(|t| t.title.to_string()).unwrap_or_else(|| "Untitled.docx".into());
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Word document", &["docx"])
            .add_filter("Markdown", &["md", "markdown"])
            .set_file_name(start)
            .save_file()
        {
            if let Some(tab) = self.tabs.get_mut(self.active) {
                // Choosing a .md name switches the tab to Markdown, and vice-versa.
                tab.markdown = is_markdown_path(&path);
                tab.path = Some(path);
            }
            self.save_active(window, cx);
        } else {
            self.refocus(window, cx);
        }
    }

    fn open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = rfd::FileDialog::new().add_filter("Word or Markdown", &["docx", "md", "markdown"]).add_filter("Word document", &["docx"]).add_filter("Markdown", &["md", "markdown"]).pick_file() {
            let title = file_name(&path).into();
            let tab = doc_from_path(&path).into_tab(Kind::Docx, title, Some(path), false);
            self.tabs.push(tab);
            self.active = self.tabs.len() - 1;
        }
        self.backstage = false;
        self.bs_new = false;
        self.persist();
        self.refocus(window, cx);
    }

    /// Scroll the document so the caret's top-level block is in view (keyboard
    /// navigation/typing in a long document shouldn't let the caret drift off).
    fn scroll_to_caret(&self) {
        if let Some(t) = self.tabs.get(self.active) {
            if let Surface::Doc(ed) = &t.surface {
                if let Some(&b) = ed.caret.path.first() {
                    self.doc_scroll.scroll_to_item(b);
                }
            }
        }
    }

    /// Place the caret at an explicit paragraph path + char offset (used by
    /// click-to-caret). With `extend` (Shift-click) it keeps/starts an anchor so
    /// the click extends the selection; otherwise it collapses any selection.
    fn set_caret(&mut self, path: Vec<usize>, offset: usize, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ed) = self.edit_target() {
            if extend {
                ed.extend_selection(true); // anchor at the current caret if none, else keep it
            } else {
                ed.clear_selection();
            }
            ed.caret = Caret::at(path, offset);
            ed.clamp();
        }
        self.focus.focus(window, cx);
        self.focused = true;
        cx.notify();
    }

    /// Start a mouse drag-selection at a click. Without Shift it plants a fresh
    /// anchor at the click; with Shift it extends the existing selection.
    fn begin_select(&mut self, path: Vec<usize>, offset: usize, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.mini_bar = None;
        self.context_menu = None;
        if let Some(ed) = self.edit_target() {
            if extend {
                ed.extend_selection(true); // anchor at the current caret if none
            } else {
                ed.clear_selection();
            }
            ed.caret = Caret::at(path, offset);
            if !extend {
                ed.extend_selection(true); // plant the anchor here so a drag extends from it
            }
            ed.clamp();
        }
        self.selecting = true;
        self.focus.focus(window, cx);
        self.focused = true;
        cx.notify();
    }

    /// Extend the drag-selection to the character under the cursor (anchor stays).
    fn extend_select(&mut self, path: Vec<usize>, offset: usize, cx: &mut Context<Self>) {
        if let Some(ed) = self.edit_target() {
            ed.caret = Caret::at(path, offset);
            ed.clamp();
        }
        cx.notify();
    }

    fn with_editor(&mut self, window: &mut Window, cx: &mut Context<Self>, f: impl FnOnce(&mut Editor)) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            // Route to the header/footer editor while it's open, else the body.
            if let Some(hf) = tab.hf_edit.as_mut() {
                f(&mut hf.editor);
                tab.dirty = true;
            } else if let Surface::Doc(ed) = &mut tab.surface {
                f(ed);
                tab.dirty = true;
            }
        }
        self.refocus(window, cx);
    }

    fn do_copy(&mut self, cut: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut dirty = false;
        if let Some(ed) = self.edit_target() {
            let c = if cut { dirty = true; ed.cut() } else { ed.copy() };
            if c.is_some() {
                self.clip = c;
            }
        }
        if dirty {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.dirty = true;
            }
        }
        self.refocus(window, cx);
    }

    fn do_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(clip) = self.clip.clone() {
            self.with_editor(window, cx, |e| e.paste(&clip));
        } else {
            self.refocus(window, cx);
        }
    }

    // ---- find & replace ----------------------------------------------------

    /// Open the find bar (focused on the query field) or close it.
    fn toggle_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find_open = !self.find_open;
        if self.find_open {
            self.find_field = FindField::Query;
            // Seed from the current selection, if any, for a quick "find selected".
            if let Some(ed) = self.active_editor() {
                let sel = ed.selection_text();
                if !sel.is_empty() && !sel.contains('\n') {
                    self.find_query = sel;
                }
            }
            self.find_step(false, false, cx); // highlight the first match
            self.refocus(window, cx);
        } else {
            self.refocus(window, cx);
        }
    }

    /// Number of matches for the current query in the active document.
    fn match_count(&self) -> usize {
        if self.find_query.is_empty() {
            return 0;
        }
        match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => ed.find_all(&self.find_query, self.find_case).len(),
            _ => 0,
        }
    }

    /// Move to the next/previous match and select it (so it highlights).
    /// `from_start` restarts the search from the document top (used as-you-type).
    fn find_step(&mut self, reverse: bool, from_start: bool, cx: &mut Context<Self>) {
        let q = self.find_query.clone();
        let cs = self.find_case;
        if let Some(ed) = self.active_editor() {
            if from_start {
                ed.move_doc_start();
                ed.clear_selection();
            }
            if let Some(m) = ed.find_next(&q, cs, reverse) {
                ed.select_match(&m);
            }
        }
        cx.notify();
    }

    /// Replace the current match (if one is selected) and advance to the next.
    fn replace_one(&mut self, cx: &mut Context<Self>) {
        let with = self.replace_text.clone();
        let q = self.find_query.clone();
        let cs = self.find_case;
        let mut changed = false;
        if let Some(ed) = self.active_editor() {
            if ed.has_selection() {
                ed.replace_current_with(&with);
                changed = true;
            }
            if let Some(m) = ed.find_next(&q, cs, false) {
                ed.select_match(&m);
            }
        }
        if changed {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.dirty = true;
            }
        }
        cx.notify();
    }

    /// Replace every match; report the count in the status line.
    fn replace_all_now(&mut self, cx: &mut Context<Self>) {
        let with = self.replace_text.clone();
        let q = self.find_query.clone();
        let cs = self.find_case;
        let mut n = 0;
        if let Some(ed) = self.active_editor() {
            n = ed.replace_all(&q, &with, cs);
        }
        if n > 0 {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.dirty = true;
                t.status = format!("replaced {n}").into();
            }
        }
        cx.notify();
    }

    // ---- font colour / highlight pickers -----------------------------------

    fn toggle_picker(&mut self, kind: PickKind, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = if self.picker == Some(kind) { None } else { Some(kind) };
        self.refocus(window, cx);
    }

    fn apply_color(&mut self, hex: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.with_editor(window, cx, |e| e.set_color(hex));
    }

    fn apply_highlight(&mut self, name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.with_editor(window, cx, |e| e.set_highlight(name));
    }

    fn apply_font(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.with_editor(window, cx, |e| e.set_font(&name));
    }

    fn apply_size(&mut self, pts: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.with_editor(window, cx, |e| e.set_font_size(pts * 2));
    }

    /// Field evaluation context: the current clock, author and file name.
    fn field_context(&self) -> docxcore::field::FieldContext {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| docxcore::field::civil_from_unix(d.as_secs() as i64));
        let filename = self.tabs.get(self.active).map(|t| t.title.to_string()).unwrap_or_default();
        let mut props = docxcore::field::DocProps::default();
        props.author = "docxy".to_string();
        docxcore::field::FieldContext { now, props, filename }
    }

    /// Insert a field (`<w:fldSimple>`) with its computed value at the caret.
    fn insert_field(&mut self, instr: &'static str, fallback: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        let ctx = self.field_context();
        let val = docxcore::field::eval_field_ctx(instr, &ctx).unwrap_or_else(|| fallback.to_string());
        let raw = format!("<w:fldSimple w:instr=\"{}\"><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:fldSimple>", xml_escape(instr), xml_escape(&val));
        self.with_editor(window, cx, |e| e.paste(&Clip { paras: vec![vec![Inline::Field { raw, text: val }]] }));
    }

    fn insert_page_break(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.with_editor(window, cx, |e| e.paste(&Clip { paras: vec![vec![Inline::Break(docxcore::model::BreakKind::Page)]] }));
    }

    /// Insert one symbol/special character at the caret (Insert ▸ Symbol).
    fn insert_symbol(&mut self, s: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        let s = s.to_string();
        self.with_editor(window, cx, move |e| e.insert_str(&s));
    }

    /// If the caret is inside a table, the (block index, row, cell).
    fn caret_table(&self) -> Option<(usize, usize, usize)> {
        match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => {
                let p = &ed.caret.path;
                (p.len() >= 3 && matches!(ed.doc.body.get(p[0]), Some(Block::Table(_)))).then(|| (p[0], p[1], p[2]))
            }
            _ => None,
        }
    }

    /// An empty table cell (one blank paragraph).
    fn empty_cell() -> docxcore::model::Cell {
        docxcore::model::Cell { grid_span: 1, v_merge: docxcore::model::VMerge::None, blocks: vec![Block::Paragraph(Paragraph::default())], raw_tcpr: None }
    }

    /// Run a Table Tools operation relative to the caret's cell.
    fn table_op(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        use Act::*;
        let Some((tb, row, col)) = self.caret_table() else { return self.refocus(window, cx) };
        let idx = self.active;
        if let Some(t) = self.tabs.get_mut(idx) {
            if let Surface::Doc(ed) = &mut t.surface {
                if let Some(Block::Table(table)) = ed.doc.body.get_mut(tb) {
                    let ncols = table.grid.len().max(table.rows.first().map_or(0, |r| r.cells.len()));
                    match act {
                        RowAbove | RowBelow => {
                            let at = if matches!(act, RowAbove) { row } else { row + 1 };
                            let new = docxcore::model::Row { cells: (0..ncols).map(|_| Self::empty_cell()).collect(), raw_props: vec![] };
                            table.rows.insert(at.min(table.rows.len()), new);
                            ed.caret = Caret::at(vec![tb, at.min(table.rows.len() - 1), col.min(ncols - 1), 0], 0);
                        }
                        ColLeft | ColRight => {
                            let at = if matches!(act, ColLeft) { col } else { col + 1 };
                            for r in &mut table.rows {
                                r.cells.insert(at.min(r.cells.len()), Self::empty_cell());
                            }
                            let w = table.grid.first().copied().unwrap_or(2340);
                            table.grid.insert(at.min(table.grid.len()), w);
                            ed.caret = Caret::at(vec![tb, row, at, 0], 0);
                        }
                        DelRow => {
                            if table.rows.len() > 1 {
                                table.rows.remove(row);
                                let nr = table.rows.len();
                                ed.caret = Caret::at(vec![tb, row.min(nr - 1), col.min(ncols - 1), 0], 0);
                            }
                        }
                        DelCol => {
                            if ncols > 1 {
                                for r in &mut table.rows {
                                    if col < r.cells.len() {
                                        r.cells.remove(col);
                                    }
                                }
                                if col < table.grid.len() {
                                    table.grid.remove(col);
                                }
                                ed.caret = Caret::at(vec![tb, row, col.min(ncols - 2), 0], 0);
                            }
                        }
                        DelTable => {
                            ed.doc.body.remove(tb);
                            let at = tb.min(ed.doc.body.len().saturating_sub(1));
                            ed.caret = Caret::at(vec![at], 0);
                        }
                        _ => {}
                    }
                    ed.clear_selection();
                    ed.clamp();
                }
            }
            t.dirty = true;
        }
        self.refocus(window, cx);
    }

    /// Insert an empty `rows`×`cols` bordered table after the caret's block, and
    /// move the caret into its first cell.
    fn insert_table(&mut self, rows: usize, cols: usize, window: &mut Window, cx: &mut Context<Self>) {
        use docxcore::model::{Cell, Row, Table, VMerge};
        self.picker = None;
        const TBLPR: &str = "<w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblBorders>\
<w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
<w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"auto\"/>\
</w:tblBorders></w:tblPr>";
        let col_w = (9360 / cols.max(1)) as u32;
        let mk_cell = || Cell { grid_span: 1, v_merge: VMerge::None, blocks: vec![Block::Paragraph(Paragraph::default())], raw_tcpr: None };
        let mk_row = || Row { cells: (0..cols).map(|_| mk_cell()).collect(), raw_props: vec![] };
        let table = Table { grid: vec![col_w; cols], rows: (0..rows).map(|_| mk_row()).collect(), raw_tblpr: Some(TBLPR.to_string()) };
        let idx = self.active;
        if let Some(t) = self.tabs.get_mut(idx) {
            if let Surface::Doc(ed) = &mut t.surface {
                let at = ed.caret.path.first().copied().unwrap_or(0).min(ed.doc.body.len().saturating_sub(1));
                let pos = (at + 1).min(ed.doc.body.len());
                ed.doc.body.insert(pos, Block::Table(table));
                ed.clear_selection();
                ed.caret = Caret::at(vec![pos, 0, 0, 0], 0);
                ed.clamp();
            }
            t.dirty = true;
        }
        self.refocus(window, cx);
    }

    /// The swatch strip shown under the ribbon while a picker is open.
    fn picker_bar(&self, kind: PickKind, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let swatch = |bg: Hsla, ring: bool| {
            div().size(px(20.)).rounded(px(3.)).border_1().border_color(if ring { pal.fg } else { pal.border }).bg(bg).cursor_pointer().hover(|d| d.border_color(hsla_u(BRAND)))
        };
        let mut row = h_flex().w_full().items_center().flex_wrap().gap_1p5().px_3().py_1().bg(pal.panel).border_b_1().border_color(pal.border);
        row = row.child(div().text_size(px(11.)).text_color(pal.dim).min_w(px(78.)).child(match kind {
            PickKind::Color => "Font colour",
            PickKind::Highlight => "Highlight",
            PickKind::FontName => "Font",
            PickKind::FontSize => "Size",
            PickKind::Field => "Field",
            PickKind::Table => "Table",
            PickKind::Symbol => "Symbol",
        }));
        let chip = |id_key: usize, label: SharedString, tag: &'static str| {
            div().id((tag, id_key)).flex().items_center().px_2().h(px(22.)).rounded(px(3.)).text_size(px(12.)).text_color(pal.fg).border_1().border_color(pal.border).cursor_pointer().hover(|d| d.bg(pal.hover).border_color(hsla_u(BRAND))).child(label)
        };
        match kind {
            PickKind::Color => {
                // Automatic (clear) chip.
                row = row.child(
                    div()
                        .id("col-auto")
                        .px_2()
                        .h(px(20.))
                        .rounded(px(3.))
                        .text_size(px(11.))
                        .text_color(pal.fg)
                        .border_1()
                        .border_color(pal.border)
                        .cursor_pointer()
                        .hover(|d| d.bg(pal.hover))
                        .child("Automatic")
                        .on_click(cx.listener(|this, _, window, cx| this.apply_color(None, window, cx))),
                );
                for &c in COLOR_SWATCHES {
                    let hex = format!("{c:06X}");
                    row = row.child(swatch(hsla_u(c), c == 0xFFFFFF).id(("col", c as usize)).on_click(cx.listener(move |this, _, window, cx| this.apply_color(Some(hex.clone()), window, cx))));
                }
            }
            PickKind::Highlight => {
                row = row.child(
                    div()
                        .id("hl-none")
                        .px_2()
                        .h(px(20.))
                        .rounded(px(3.))
                        .text_size(px(11.))
                        .text_color(pal.fg)
                        .border_1()
                        .border_color(pal.border)
                        .cursor_pointer()
                        .hover(|d| d.bg(pal.hover))
                        .child("None")
                        .on_click(cx.listener(|this, _, window, cx| this.apply_highlight(None, window, cx))),
                );
                for (i, &name) in HIGHLIGHT_SWATCHES.iter().enumerate() {
                    let (c, _) = highlight_rgb(name);
                    row = row.child(swatch(hsla_u(c), false).id(("hl", i)).on_click(cx.listener(move |this, _, window, cx| this.apply_highlight(Some(name.to_string()), window, cx))));
                }
            }
            PickKind::FontName => {
                for (i, &name) in FONT_NAMES.iter().enumerate() {
                    // Preview each name in its own font family.
                    row = row.child(chip(i, name.into(), "fn").font_family(name).on_click(cx.listener(move |this, _, window, cx| this.apply_font(name.to_string(), window, cx))));
                }
            }
            PickKind::FontSize => {
                for (i, &pts) in FONT_SIZES.iter().enumerate() {
                    row = row.child(chip(i, pts.to_string().into(), "fs").on_click(cx.listener(move |this, _, window, cx| this.apply_size(pts, window, cx))));
                }
            }
            PickKind::Field => {
                for (i, &(label, instr, fallback)) in FIELDS.iter().enumerate() {
                    row = row.child(chip(i, label.into(), "fld").on_click(cx.listener(move |this, _, window, cx| this.insert_field(instr, fallback, window, cx))));
                }
            }
            PickKind::Table => {
                for (i, &(label, r, c)) in TABLE_PRESETS.iter().enumerate() {
                    row = row.child(chip(i, label.into(), "tbl").on_click(cx.listener(move |this, _, window, cx| this.insert_table(r, c, window, cx))));
                }
            }
            PickKind::Symbol => {
                for (i, &s) in SYMBOLS.iter().enumerate() {
                    // A slightly larger, fixed-width chip so each glyph reads clearly.
                    row = row.child(
                        div()
                            .id(("sym", i))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(24.))
                            .rounded(px(3.))
                            .text_size(px(15.))
                            .text_color(pal.fg)
                            .border_1()
                            .border_color(pal.border)
                            .cursor_pointer()
                            .hover(|d| d.bg(pal.hover).border_color(hsla_u(BRAND)))
                            .child(SharedString::from(s))
                            .on_click(cx.listener(move |this, _, window, cx| this.insert_symbol(s, window, cx))),
                    );
                }
            }
        }
        row.into_any_element()
    }

    // ---- comments ----------------------------------------------------------

    /// The next numeric comment id for the active tab (max existing + 1).
    fn next_comment_id(&self) -> i32 {
        self.tabs
            .get(self.active)
            .map(|t| t.comments.iter().filter_map(|c| c.id.parse::<i32>().ok()).max().map(|m| m + 1).unwrap_or(1))
            .unwrap_or(1)
    }

    /// Begin a new comment on the current selection (opens the comment entry bar).
    fn start_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_sel = matches!(self.tabs.get(self.active).map(|t| &t.surface), Some(Surface::Doc(ed)) if ed.has_selection());
        if !has_sel {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.status = "Select text first, then add a comment".into();
            }
            return self.refocus(window, cx);
        }
        self.comment_open = true;
        self.comment_text.clear();
        self.picker = None;
        self.find_open = false;
        self.refocus(window, cx);
    }

    /// Commit the pending comment: wrap the selection in markers, store the text.
    fn commit_comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.comment_text.trim().to_string();
        self.comment_open = false;
        if text.is_empty() {
            return self.refocus(window, cx);
        }
        let id = self.next_comment_id();
        let idx = self.active;
        if let Some(t) = self.tabs.get_mut(idx) {
            if let Surface::Doc(ed) = &mut t.surface {
                let quoted = ed.selection_text();
                if !ed.add_comment(&id.to_string()) {
                    t.status = "No selection to comment on".into();
                    return self.refocus(window, cx);
                }
                let author = "docxy".to_string();
                t.comments.push(Comment { id: id.to_string(), author, initials: "D".into(), date: String::new(), text, quoted });
                t.dirty = true;
                t.status = format!("Comment {id} added").into();
            }
        }
        self.refocus(window, cx);
    }

    /// Route a keystroke to the comment entry bar while it is open.
    fn comment_key(&mut self, ev: &KeyDownEvent, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        match key {
            "escape" => {
                self.comment_open = false;
                self.comment_text.clear();
                return self.refocus(window, cx);
            }
            "enter" => return self.commit_comment(window, cx),
            "backspace" => {
                self.comment_text.pop();
                return cx.notify();
            }
            _ => {}
        }
        if let Some(c) = ev.keystroke.key_char.as_deref() {
            if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                self.comment_text.push_str(c);
                cx.notify();
            }
        }
    }

    /// Begin dragging a ruler marker.
    fn ruler_drag_start(&mut self, handle: RulerHandle, x: f32, cx: &mut Context<Self>) {
        let (indent, first, right) = match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => {
                let (i, f) = ed.caret_para_indent();
                (i, f, ed.caret_para_right_indent())
            }
            _ => (0, 0, 0),
        };
        let geom = self.tabs.get(self.active).and_then(|t| t.pkg.as_ref()).map(|p| p.page_geom()).unwrap_or_default();
        self.ruler_drag = Some(RulerDrag { handle, start_x: x, start_indent: indent, start_first: first, start_right: right, start_ml: geom.ml, start_mr: geom.mr });
        self.ruler_guide = Some(x); // guide starts under the pointer
        cx.notify();
    }

    /// Update the dragged ruler marker (x is the live pointer position; only the
    /// delta from the drag start is used). Markers snap to the 1/8" ruler grid
    /// (Word's sticky ruler), and a vertical guide line is tracked down the page.
    fn ruler_drag_move(&mut self, x: f32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.ruler_drag else { return };
        let delta = ((x - d.start_x) * 15.0).round() as i32; // px → twips
        let x0 = self.ruler_x0.get(); // left-margin screen x
        let geom = self.tabs.get(self.active).and_then(|t| t.pkg.as_ref()).map(|p| p.page_geom()).unwrap_or_default();
        let content_w = (geom.w - geom.ml - geom.mr).max(0);
        let origin = x0 - geom.ml as f32 / 15.0; // page's left-edge screen x
        let tw = |t: i32| t as f32 / 15.0;
        let guide = match d.handle {
            RulerHandle::FirstLine => {
                // Snap the marker (indent + first) to the grid, keep indent fixed.
                let marker = snap_twips(d.start_indent + d.start_first + delta);
                let first = marker - d.start_indent;
                self.with_editor(window, cx, |e| e.set_first_line(first));
                x0 + tw(d.start_indent + first)
            }
            RulerHandle::Left => {
                let ind = snap_twips((d.start_indent + delta).max(0));
                let first = d.start_first;
                self.with_editor(window, cx, |e| e.set_indent(ind, first));
                x0 + tw(ind)
            }
            // Dragging the right marker left (negative delta) increases the indent.
            RulerHandle::Right => {
                let ri = snap_twips((d.start_right - delta).max(0));
                self.with_editor(window, cx, |e| e.set_right_indent(ri));
                x0 + tw(content_w - ri)
            }
            RulerHandle::MarginLeft | RulerHandle::MarginRight => {
                let (ml, mr) = match d.handle {
                    RulerHandle::MarginLeft => (snap_twips((d.start_ml + delta).max(0)), geom.mr),
                    _ => (geom.ml, snap_twips((d.start_mr - delta).max(0))),
                };
                if let Some(t) = self.tabs.get_mut(self.active) {
                    if let Some(pkg) = t.pkg.as_mut() {
                        pkg.set_page_margins(geom.mt, mr, geom.mb, ml);
                        t.dirty = true;
                    }
                }
                match d.handle {
                    RulerHandle::MarginLeft => origin + tw(ml),
                    _ => origin + tw(geom.w - mr),
                }
            }
        };
        self.ruler_guide = Some(guide);
        cx.notify();
    }

    /// Add a tab stop at the clicked ruler position (mapped from the stored
    /// left-margin origin), or remove one if the click lands on an existing stop.
    fn ruler_click_tab(&mut self, x: f32, window: &mut Window, cx: &mut Context<Self>) {
        let pos = ((x - self.ruler_x0.get()) * 15.0).round() as i32;
        if pos < 0 {
            return;
        }
        let align = self.ruler_tab;
        self.with_editor(window, cx, |e| {
            if !e.remove_tab_stop_near(pos, 90) {
                e.add_tab_stop(pos, align);
            }
        });
    }

    /// Cycle the tab-stop type set by clicking the ruler (Left → Center → Right).
    fn cycle_ruler_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use docxcore::model::TabAlign::*;
        self.ruler_tab = match self.ruler_tab {
            Left => Center,
            Center => Right,
            Right => Left,
        };
        self.refocus(window, cx);
    }

    fn ruler_drag_end(&mut self, cx: &mut Context<Self>) {
        if self.ruler_drag.take().is_some() {
            self.ruler_guide = None;
            cx.notify();
        }
    }

    /// A Word-style horizontal ruler: the page/margins, tick marks, tab stops and
    /// the current paragraph's first-line / left (other-rows) indent markers.
    fn ruler(&self, cx: &mut Context<Self>) -> AnyElement {
        use docxcore::model::{TabAlign, TabStop};
        let tab = self.tabs.get(self.active);
        let geom = tab.and_then(|t| t.pkg.as_ref()).map(|p| p.page_geom()).unwrap_or_default();
        let (indent, first_line, indent_right, tabs): (i32, i32, i32, Vec<TabStop>) = match tab.map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => {
                let (i, f) = ed.caret_para_indent();
                (i, f, ed.caret_para_right_indent(), ed.caret_para_props().tabs.clone())
            }
            _ => (0, 0, 0, vec![]),
        };
        let d = 15.0_f32; // twips → px at ~96dpi
        let pw = geom.w as f32 / d;
        let ml = geom.ml as f32 / d;
        let mr = geom.mr as f32 / d;
        let ind = indent as f32 / d;
        let fl = first_line as f32 / d;
        let rind = indent_right as f32 / d;
        let content_r = pw - mr;
        let right_marker = content_r - rind;
        let h = 22.0_f32;
        let x0_cell = self.ruler_x0.clone();

        let paint = canvas(
            move |_b, _w, _a| {},
            move |b: Bounds<Pixels>, _s, window: &mut Window, _a: &mut App| {
                // Record the left-margin screen x so click handlers can map a click
                // to a tab position (twips from the left margin).
                x0_cell.set(f32::from(b.origin.x) + ml);
                let x = |v: f32| b.origin.x + px(v);
                let top = b.origin.y;
                let base = hsla_u(0xb8b8b8);
                let white = hsla_u(0xffffff);
                let tick = hsla_u(0x707070);
                let brand = hsla_u(BRAND);
                let dim = hsla_u(0x555555);
                window.paint_quad(fill(b, base));
                window.paint_quad(fill(Bounds::from_corners(point(x(ml), top + px(3.)), point(x(content_r), top + px(h - 3.))), white));
                // Tick marks every 1/8", taller each inch, from the left margin.
                let step = 96.0 / 8.0;
                let mut i = 0;
                let mut xx = ml;
                while xx <= content_r + 0.5 {
                    let major = i % 8 == 0;
                    let th = if major { h * 0.34 } else if i % 4 == 0 { h * 0.24 } else { h * 0.15 };
                    window.paint_quad(fill(Bounds::from_corners(point(x(xx), top + px((h - th) * 0.5)), point(x(xx + 1.0), top + px((h + th) * 0.5))), tick));
                    xx += step;
                    i += 1;
                }
                // Default tab stops (every 0.5") as tiny ticks along the baseline.
                let mut tx = ml + 48.0;
                while tx <= content_r {
                    window.paint_quad(fill(Bounds::from_corners(point(x(tx), top + px(h - 4.)), point(x(tx + 1.0), top + px(h - 2.))), hsla_u(0x999999)));
                    tx += 48.0;
                }
                // Custom tab stops (from the paragraph) as L / ⊥ / ⌐ markers.
                for t in &tabs {
                    let sx = x(ml + t.pos as f32 / d);
                    let yb = top + px(h - 5.);
                    // vertical stem
                    window.paint_quad(fill(Bounds::from_corners(point(sx, top + px(h - 11.)), point(sx + px(1.5), yb)), dim));
                    // foot direction encodes alignment
                    let (fx0, fx1) = match t.align {
                        TabAlign::Left => (0.0, 5.0),
                        TabAlign::Right => (-5.0, 0.0),
                        TabAlign::Center => (-3.0, 3.0),
                    };
                    window.paint_quad(fill(Bounds::from_corners(point(sx + px(fx0), yb - px(1.5)), point(sx + px(fx1), yb)), dim));
                }
                let z = point(0.0_f32, 0.0);
                // First-line indent — downward triangle at the top.
                let flx = ml + ind + fl;
                let mut t1 = Path::new(point(x(flx - 5.0), top + px(1.)));
                t1.push_triangle((point(x(flx - 5.0), top + px(1.)), point(x(flx + 5.0), top + px(1.)), point(x(flx), top + px(8.))), (z, z, z));
                window.paint_path(t1, brand);
                // Left / other-rows indent — upward triangle + a square below it.
                let lx = ml + ind;
                let by = top + px(h - 1.);
                let mut t2 = Path::new(point(x(lx - 5.0), by - px(4.)));
                t2.push_triangle((point(x(lx - 5.0), by - px(4.)), point(x(lx + 5.0), by - px(4.)), point(x(lx), by - px(11.))), (z, z, z));
                window.paint_path(t2, brand);
                window.paint_quad(fill(Bounds::from_corners(point(x(lx - 4.0), by - px(4.)), point(x(lx + 4.0), by)), brand));
                // Right indent — upward triangle, positioned in from the right
                // margin by the paragraph's right indent.
                let rx = right_marker;
                let mut t3 = Path::new(point(x(rx - 5.0), by));
                t3.push_triangle((point(x(rx - 5.0), by), point(x(rx + 5.0), by), point(x(rx), top + px(h - 8.))), (z, z, z));
                window.paint_path(t3, brand);
            },
        );

        // Measurement numbers (1, 2, 3 …) at each inch from the left margin.
        let mut numbers = div().absolute().size_full();
        let mut inch = 1;
        loop {
            let xx = ml + inch as f32 * 96.0;
            if xx > content_r - 6.0 {
                break;
            }
            numbers = numbers.child(div().absolute().left(px(xx - 3.0)).top(px(4.0)).text_size(px(8.)).text_color(hsla_u(0x555555)).child(SharedString::from(inch.to_string())));
            inch += 1;
        }

        // Draggable indent handles, split into a TOP band (first-line marker) and
        // a BOTTOM band (left / right markers) so they never overlap where they
        // share an x (e.g. a paragraph with no indent) and each stays grabbable.
        let handle = |id: &'static str, cx_px: f32, top_px: f32, h_px: f32, which: RulerHandle, cxx: &mut Context<Self>| {
            div()
                .id(id)
                .absolute()
                .left(px(cx_px - 6.0))
                .top(px(top_px))
                .w(px(12.))
                .h(px(h_px))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, cxx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.ruler_drag_start(which, f32::from(ev.position.x), cx);
                }))
        };

        // Margin grab strips sit in the grey zone just OUTSIDE the white content
        // area (Word's margin boundary), clear of the indent markers.
        let margin_handle = |id: &'static str, left_px: f32, which: RulerHandle, cxx: &mut Context<Self>| {
            div()
                .id(id)
                .absolute()
                .left(px(left_px))
                .top(px(0.))
                .w(px(8.))
                .h(px(h))
                .cursor_col_resize()
                .on_mouse_down(MouseButton::Left, cxx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.ruler_drag_start(which, f32::from(ev.position.x), cx);
                }))
        };

        let container = div()
            .relative()
            .w(px(pw))
            .h(px(h))
            .child(paint.size_full())
            .child(numbers)
            // Click the content area to add/remove a tab stop of the current type.
            .on_mouse_down(MouseButton::Left, cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                this.ruler_click_tab(f32::from(ev.position.x), window, cx);
            }))
            .child(margin_handle("rh-mleft", ml - 8.0, RulerHandle::MarginLeft, cx))
            .child(margin_handle("rh-mright", content_r, RulerHandle::MarginRight, cx))
            .child(handle("rh-first", ml + ind + fl, 0.0, h * 0.5, RulerHandle::FirstLine, cx))
            .child(handle("rh-left", ml + ind, h * 0.5, h * 0.5, RulerHandle::Left, cx))
            .child(handle("rh-right", right_marker, h * 0.5, h * 0.5, RulerHandle::Right, cx));

        // The tab-type selector box at the far left (click to cycle L/Centre/Right).
        let tab_glyph = match self.ruler_tab {
            docxcore::model::TabAlign::Left => "L",
            docxcore::model::TabAlign::Center => "\u{22A5}",
            docxcore::model::TabAlign::Right => "\u{2510}",
        };
        // Same width + gap as the vertical ruler so the horizontal ruler content
        // lines up with the page sheet (this box sits in the corner, Word-style).
        let selector = div()
            .id("ruler-tabtype")
            .w(px(18.))
            .h(px(18.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(2.))
            .bg(hsla_u(0xffffff))
            .border_1()
            .border_color(hsla_u(0xb0b0b0))
            .text_size(px(11.))
            .text_color(hsla_u(0x333333))
            .cursor_pointer()
            .child(tab_glyph)
            .tooltip(|w, cx| Tooltip::new("Tab stop type — click to cycle").build(w, cx))
            .on_click(cx.listener(|this, _, window, cx| this.cycle_ruler_tab(window, cx)));

        // Drag move/end are handled at the window root (so a drag survives the
        // pointer leaving this thin strip); the ruler itself only starts drags.
        h_flex()
            .w_full()
            .items_center()
            .justify_center()
            .gap(px(3.))
            .bg(hsla_u(0xdedede))
            .py_0p5()
            .child(selector)
            .child(container)
            .into_any_element()
    }

    /// The vertical ruler on the left of the page (Print Layout): top/bottom
    /// margins shaded, content white, tick marks every inch from the top margin.
    fn vruler(&self) -> AnyElement {
        let geom = self.tabs.get(self.active).and_then(|t| t.pkg.as_ref()).map(|p| p.page_geom()).unwrap_or_default();
        let d = 15.0_f32;
        let mt = geom.mt as f32 / d;
        let mb = geom.mb as f32 / d;
        let paint = canvas(
            move |_b, _w, _a| {},
            move |b: Bounds<Pixels>, _s, window: &mut Window, _a: &mut App| {
                let y = |v: f32| b.origin.y + px(v);
                let left = b.origin.x;
                let w = f32::from(b.size.width);
                let hh = f32::from(b.size.height);
                let base = hsla_u(0xb8b8b8);
                let white = hsla_u(0xffffff);
                let tick = hsla_u(0x707070);
                // ground + white content strip between top and bottom margins.
                window.paint_quad(fill(b, base));
                window.paint_quad(fill(Bounds::from_corners(point(left + px(3.), y(mt)), point(left + px(w - 3.), y((hh - mb).max(mt)))), white));
                // ticks every 1/8", taller each inch, measured from the top margin.
                let step = 96.0 / 8.0;
                let mut i = 0;
                let mut yy = mt;
                while yy <= hh - mb + 0.5 {
                    let major = i % 8 == 0;
                    let tw = if major { w * 0.42 } else if i % 4 == 0 { w * 0.30 } else { w * 0.18 };
                    let x1 = left + px((w - tw) * 0.5);
                    let x2 = left + px((w + tw) * 0.5);
                    window.paint_quad(fill(Bounds::from_corners(point(x1, y(yy)), point(x2, y(yy + 1.0))), tick));
                    yy += step;
                    i += 1;
                }
            },
        );
        // Measurement numbers down the ruler at each inch from the top margin.
        let mut numbers = div().absolute().size_full();
        let mut inch = 1;
        loop {
            let yy = mt + inch as f32 * 96.0;
            // stop numbering a little before the bottom margin using a generous page.
            if yy > (geom.h as f32 / d) - mb - 6.0 {
                break;
            }
            numbers = numbers.child(div().absolute().top(px(yy - 5.0)).left(px(4.0)).text_size(px(8.)).text_color(hsla_u(0x555555)).child(SharedString::from(inch.to_string())));
            inch += 1;
        }
        div().relative().w(px(18.)).flex_none().child(paint.size_full()).child(numbers).into_any_element()
    }

    /// The comment entry bar, shown under the ribbon while `comment_open`.
    fn comment_bar(&self, pal: Pal, _cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(icon_svg("comment-add", 14., hsla_u(BRAND)))
            .child(div().text_size(px(11.)).text_color(pal.dim).child("New comment"))
            .child(
                h_flex()
                    .flex_1()
                    .items_center()
                    .px_2()
                    .h(px(24.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(hsla_u(BRAND))
                    .bg(pal.panel)
                    .child(div().text_size(px(13.)).text_color(pal.fg).child(SharedString::from(self.comment_text.clone())))
                    .child(caret_bar()),
            )
            .child(div().text_size(px(11.)).text_color(pal.dim).child("Enter to add · Esc to cancel"))
            .into_any_element()
    }

    /// Delete a comment: strip its markers from the body and drop it from the list.
    fn delete_comment(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.tabs.get_mut(self.active) {
            if let Surface::Doc(ed) = &mut t.surface {
                ed.remove_comment_markers(&id);
            }
            t.comments.retain(|c| c.id != id);
            t.dirty = true;
        }
        self.refocus(window, cx);
    }

    /// Select the text a comment is anchored to (find its quoted run).
    fn goto_comment(&mut self, quoted: String, window: &mut Window, cx: &mut Context<Self>) {
        if !quoted.is_empty() {
            if let Some(ed) = self.active_editor() {
                if let Some(m) = ed.find_next(&quoted, true, false) {
                    ed.select_match(&m);
                }
            }
        }
        self.scroll_to_caret();
        self.refocus(window, cx);
    }

    /// The comments review side panel (toggled from Review/View ▸ Comments pane).
    fn comments_panel(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let comments = self.tabs.get(self.active).map(|t| t.comments.clone()).unwrap_or_default();
        let mut list = v_flex().id("cmt-list").flex_1().overflow_y_scroll().gap_2().p_2();
        if comments.is_empty() {
            list = list.child(div().text_size(px(12.)).text_color(pal.dim).p_2().child("No comments. Select text, then Review \u{203A} New comment."));
        }
        for c in &comments {
            let id = c.id.clone();
            let quoted = c.quoted.clone();
            list = list.child(
                v_flex()
                    .id(("cmt", id.parse::<usize>().unwrap_or(0)))
                    .gap_1()
                    .p_2()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(pal.border)
                    .bg(pal.panel)
                    .cursor_pointer()
                    .hover(|d| d.border_color(hsla_u(BRAND)))
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(hsla_u(BRAND)).child(SharedString::from(c.author.clone())))
                            .child(
                                div()
                                    .id(("cmtx", id.parse::<usize>().unwrap_or(0)))
                                    .px_1()
                                    .rounded_sm()
                                    .text_color(pal.dim)
                                    .hover(|d| d.bg(pal.hover))
                                    .child("\u{00d7}")
                                    .on_click(cx.listener({
                                        let id = id.clone();
                                        move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.delete_comment(id.clone(), window, cx);
                                        }
                                    })),
                            ),
                    )
                    .when(!c.quoted.is_empty(), |d| d.child(div().text_size(px(11.)).italic().text_color(pal.dim).child(SharedString::from(format!("\u{201C}{}\u{201D}", c.quoted)))))
                    .child(div().text_size(px(13.)).text_color(pal.fg).child(SharedString::from(c.text.clone())))
                    .on_click(cx.listener(move |this, _, window, cx| this.goto_comment(quoted.clone(), window, cx))),
            );
        }
        v_flex()
            .w(px(280.))
            .h_full()
            .border_l_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .child(div().px_3().py_2().text_size(px(13.)).font_weight(FontWeight::BOLD).text_color(pal.fg).border_b_1().border_color(pal.border).child(SharedString::from(format!("Comments ({})", comments.len()))))
            .child(list)
            .into_any_element()
    }

    /// Navigate the caret to the start of a top-level block and scroll to it.
    fn goto_block(&mut self, block: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.set_caret(vec![block], 0, false, window, cx);
        self.scroll_to_caret();
    }

    /// The navigation (heading outline) side panel — click a heading to jump.
    fn nav_panel(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let headings: Vec<(usize, u8, String)> = match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => ed
                .doc
                .body
                .iter()
                .enumerate()
                .filter_map(|(i, b)| match b {
                    Block::Paragraph(p) => p.props.heading_level.map(|lvl| (i, lvl as u8, p.plain_text())),
                    _ => None,
                })
                .filter(|(_, _, t)| !t.trim().is_empty())
                .collect(),
            _ => vec![],
        };
        let mut list = v_flex().id("nav-list").flex_1().overflow_y_scroll().gap_0p5().p_2();
        if headings.is_empty() {
            list = list.child(div().text_size(px(12.)).text_color(pal.dim).p_2().child("No headings."));
        }
        for (i, lvl, text) in &headings {
            let block = *i;
            let indent = (lvl.saturating_sub(1)) as f32 * 12.0;
            list = list.child(
                div()
                    .id(("nav", block))
                    .pl(px(8.0 + indent))
                    .pr_2()
                    .py_1()
                    .rounded(px(3.))
                    .text_size(px(if *lvl <= 1 { 13. } else { 12. }))
                    .text_color(if *lvl <= 1 { pal.fg } else { pal.dim })
                    .when(*lvl <= 1, |d| d.font_weight(FontWeight::MEDIUM))
                    .cursor_pointer()
                    .hover(|d| d.bg(pal.hover))
                    .child(SharedString::from(text.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| this.goto_block(block, window, cx))),
            );
        }
        v_flex()
            .w(px(240.))
            .h_full()
            .border_r_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .child(div().px_3().py_2().text_size(px(13.)).font_weight(FontWeight::BOLD).text_color(pal.fg).border_b_1().border_color(pal.border).child("Navigation"))
            .child(list)
            .into_any_element()
    }

    /// The footnotes/endnotes side panel (display-only).
    fn notes_panel(&self, pal: Pal, _cx: &mut Context<Self>) -> AnyElement {
        let notes = self.tabs.get(self.active).map(|t| t.notes.clone()).unwrap_or_default();
        let mut list = v_flex().id("notes-list").flex_1().overflow_y_scroll().gap_2().p_2();
        if notes.is_empty() {
            list = list.child(div().text_size(px(12.)).text_color(pal.dim).p_2().child("No footnotes or endnotes."));
        }
        for n in &notes {
            let tag = if n.endnote { "endnote" } else { "footnote" };
            list = list.child(
                v_flex()
                    .gap_1()
                    .p_2()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(pal.border)
                    .bg(pal.panel)
                    .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(hsla_u(BRAND)).child(SharedString::from(format!("{tag} {}", n.id))))
                    .child(div().text_size(px(13.)).text_color(pal.fg).child(SharedString::from(n.text.clone()))),
            );
        }
        v_flex()
            .w(px(280.))
            .h_full()
            .border_l_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .child(div().px_3().py_2().text_size(px(13.)).font_weight(FontWeight::BOLD).text_color(pal.fg).border_b_1().border_color(pal.border).child(SharedString::from(format!("Notes ({})", notes.len()))))
            .child(list)
            .into_any_element()
    }

    /// Route a keystroke to the find bar while it is open.
    fn find_key(&mut self, ev: &KeyDownEvent, shift: bool, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        match key {
            "escape" => return self.toggle_find(window, cx),
            "enter" => {
                if self.find_field == FindField::Replace {
                    self.replace_one(cx);
                } else {
                    self.find_step(shift, false, cx);
                }
                return;
            }
            // Tab is swallowed by gpui's focus traversal, so also accept Up/Down to
            // move between the Find and Replace fields (Tab still works if delivered).
            "tab" | "down" | "up" => {
                self.find_field = match (self.find_field, key) {
                    (FindField::Query, "up") => FindField::Query,
                    (FindField::Replace, "down") => FindField::Replace,
                    (FindField::Query, _) => FindField::Replace,
                    (FindField::Replace, _) => FindField::Query,
                };
                cx.notify();
                return;
            }
            "backspace" => {
                let f = self.find_field;
                let field = if f == FindField::Query { &mut self.find_query } else { &mut self.replace_text };
                field.pop();
                if f == FindField::Query {
                    self.find_step(false, true, cx);
                } else {
                    cx.notify();
                }
                return;
            }
            _ => {}
        }
        if let Some(c) = ev.keystroke.key_char.as_deref() {
            if !c.is_empty() && !c.chars().next().unwrap().is_control() {
                let f = self.find_field;
                if f == FindField::Query {
                    self.find_query.push_str(c);
                    self.find_step(false, true, cx);
                } else {
                    self.replace_text.push_str(c);
                    cx.notify();
                }
            }
        }
    }

    /// The find & replace bar, shown under the ribbon while `find_open`.
    fn find_bar(&self, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let count = self.match_count();
        let count_txt = if self.find_query.is_empty() {
            String::new()
        } else if count == 0 {
            "no matches".to_string()
        } else {
            format!("{count} match{}", if count == 1 { "" } else { "es" })
        };

        let field = |label: &'static str, text: &str, active: bool| {
            h_flex()
                .items_center()
                .gap_1p5()
                .px_2()
                .h(px(24.))
                .min_w(px(150.))
                .rounded(px(4.))
                .border_1()
                .border_color(if active { hsla_u(BRAND) } else { pal.border })
                .bg(pal.panel)
                .child(div().text_size(px(10.)).text_color(pal.dim).child(label))
                .child(div().text_size(px(13.)).text_color(pal.fg).child(SharedString::from(text.to_string())))
                .when(active, |d| d.child(caret_bar()))
        };

        let icon_btn = |id: &'static str, glyph: &'static str, on: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(14.))
                .text_color(pal.fg)
                .when(on, |d| d.bg(pal.hover))
                .hover(|d| d.bg(pal.hover))
                .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
                .child(glyph)
        };
        let text_btn = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .px_2()
                .h(px(24.))
                .rounded(px(4.))
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(pal.fg)
                .hover(|d| d.bg(pal.hover))
                .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
                .child(label)
        };

        h_flex()
            .w_full()
            .items_center()
            .flex_wrap()
            .gap_2()
            .px_3()
            .py_1()
            .bg(pal.panel)
            .border_b_1()
            .border_color(pal.border)
            .child(field("Find", &self.find_query, self.find_field == FindField::Query).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.find_field = FindField::Query;
                cx.notify();
            })))
            .child(div().text_size(px(11.)).text_color(pal.dim).min_w(px(68.)).child(SharedString::from(count_txt)))
            .child(icon_btn("f-prev", "\u{2191}", false).on_click(cx.listener(|this, _, _, cx| this.find_step(true, false, cx))))
            .child(icon_btn("f-next", "\u{2193}", false).on_click(cx.listener(|this, _, _, cx| this.find_step(false, false, cx))))
            .child(div().w(px(1.)).h(px(18.)).bg(pal.border))
            .child(field("Replace", &self.replace_text, self.find_field == FindField::Replace).on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| {
                this.find_field = FindField::Replace;
                cx.notify();
            })))
            .child(text_btn("f-rep", "Replace").on_click(cx.listener(|this, _, _, cx| this.replace_one(cx))))
            .child(text_btn("f-all", "All").on_click(cx.listener(|this, _, _, cx| this.replace_all_now(cx))))
            .child(div().flex_1())
            .child(icon_btn("f-case", "Aa", self.find_case).on_click(cx.listener(|this, _, _, cx| {
                this.find_case = !this.find_case;
                this.find_step(false, true, cx);
            })))
            .child(icon_btn("f-close", "\u{00d7}", false).on_click(cx.listener(|this, _, window, cx| this.toggle_find(window, cx))))
            .into_any_element()
    }

    /// Insert a tab at the caret (bound to the Tab key via an action, since gpui
    /// swallows Tab for focus traversal before on_key_down sees it).
    fn tab_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.keytips != KeyTip::Off || self.find_open || self.comment_open || self.backstage {
            return;
        }
        self.mini_bar = None;
        self.context_menu = None;
        self.with_editor(window, cx, |e| e.insert_tab());
        self.scroll_to_caret();
    }

    /// Shift+Tab decreases the paragraph indent (Word's outdent).
    fn shift_tab_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.keytips != KeyTip::Off || self.find_open || self.comment_open || self.backstage {
            return;
        }
        self.mini_bar = None;
        self.context_menu = None;
        self.with_editor(window, cx, |e| e.change_indent(-720));
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        let ctrl = m.control || m.platform;
        let shift = m.shift;
        let key = ev.keystroke.key.clone();
        // KeyTips (Alt / F10 access keys): toggle the overlay; while it's showing,
        // letters pick a tab / run a command instead of typing.
        if (key == "alt" || key == "f10") && !ctrl {
            self.keytips = if self.keytips == KeyTip::Off { KeyTip::Tabs } else { KeyTip::Off };
            cx.notify();
            return;
        }
        if self.keytips != KeyTip::Off {
            if key == "escape" {
                self.keytips = if self.keytips == KeyTip::Commands { KeyTip::Tabs } else { KeyTip::Off };
                cx.notify();
                return;
            }
            if let Some(c) = ev.keystroke.key_char.as_deref().filter(|c| c.chars().count() == 1 && c.chars().next().is_some_and(|ch| ch.is_ascii_alphanumeric())) {
                return self.keytip_input(c, window, cx);
            }
            return; // swallow other keys while KeyTips are up
        }
        // Any key dismisses the floating mini toolbar / context menu.
        self.mini_bar = None;
        self.context_menu = None;
        // In header/footer edit mode, Esc returns to the document body.
        if key == "escape" && self.hf_active() {
            return self.exit_hf(window, cx);
        }
        // Ctrl+F toggles the find bar; while it's open, all keys go to it.
        if ctrl && key == "f" {
            return self.toggle_find(window, cx);
        }
        if self.comment_open {
            return self.comment_key(ev, key.as_str(), window, cx);
        }
        if self.find_open {
            return self.find_key(ev, shift, key.as_str(), window, cx);
        }
        if ctrl {
            match key.as_str() {
                "s" => return self.save_active(window, cx),
                "c" => return self.do_copy(false, window, cx),
                "x" => return self.do_copy(true, window, cx),
                "v" => return self.do_paste(window, cx),
                "f1" => {
                    self.ribbon_min = !self.ribbon_min;
                    cx.notify();
                    return;
                }
                // Zoom (Ctrl+= / Ctrl+- / Ctrl+0), the browser-standard gesture.
                "=" | "+" => {
                    self.zoom = (self.zoom + 0.1).min(3.0);
                    cx.notify();
                    return;
                }
                "-" => {
                    self.zoom = (self.zoom - 0.1).max(0.5);
                    cx.notify();
                    return;
                }
                "0" => {
                    self.zoom = 1.0;
                    cx.notify();
                    return;
                }
                _ => {}
            }
        }
        let Some(ed) = self.edit_target() else { return };
        if key == "escape" {
            ed.clear_selection();
            cx.notify();
            return;
        }
        // Navigation keys extend the selection when Shift is held and collapse it
        // otherwise; every other key leaves the anchor alone (typing, backspace and
        // delete handle any active selection themselves in the engine).
        if matches!(key.as_str(), "left" | "right" | "home" | "end" | "up" | "down") {
            ed.extend_selection(shift);
        }
        let changed = if ctrl {
            match key.as_str() {
                "b" => yes(|| ed.toggle_bold()),
                "i" => yes(|| ed.toggle_italic()),
                "u" => yes(|| ed.toggle_underline()),
                "z" => {
                    ed.undo();
                    true
                }
                "y" => {
                    ed.redo();
                    true
                }
                "a" => no(|| ed.select_all()),
                // Indent / outdent (Ctrl+M, Ctrl+Shift+M).
                "m" if shift => yes(|| ed.change_indent(-720)),
                "m" => yes(|| ed.change_indent(720)),
                // Word- and document-wise motion (Ctrl+←/→, Ctrl+Home/End).
                "left" => no(|| ed.move_word_left()),
                "right" => no(|| ed.move_word_right()),
                "home" => no(|| ed.move_doc_start()),
                "end" => no(|| ed.move_doc_end()),
                _ => false,
            }
        } else {
            match key.as_str() {
                "backspace" => yes(|| ed.backspace()),
                "delete" => yes(|| ed.delete_forward()),
                "enter" => yes(|| ed.insert_newline()),
                "tab" => yes(|| ed.insert_tab()),
                "left" => no(|| ed.move_left()),
                "right" => no(|| ed.move_right()),
                "home" => no(|| ed.move_home()),
                "end" => no(|| ed.move_end()),
                "up" => no(|| move_vert(ed, false)),
                "down" => no(|| move_vert(ed, true)),
                _ => match ev.keystroke.key_char.as_deref() {
                    Some(c) if !c.is_empty() && !c.chars().next().unwrap().is_control() => {
                        ed.insert_str(c);
                        true
                    }
                    _ => false,
                },
            }
        };
        if changed {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.dirty = true;
            }
        }
        self.scroll_to_caret();
        cx.notify();
    }
}

fn yes(mut f: impl FnMut()) -> bool {
    f();
    true
}
fn no(mut f: impl FnMut()) -> bool {
    f();
    false
}

// ---- the ribbon, defined once via ribbonspec (shared model) ----------------

#[derive(Clone, Copy)]
enum Act {
    Bold, Italic, Underline, Strike, Grow, Shrink,
    AlignL, AlignC, AlignR, AlignJ,
    Cut, Copy, Paste,
    Normal, H1, H2, H3, HRule, SelectAll, Case,
    Bullets, Numbers, IndentInc, IndentDec, ClearFmt, Find, FontColor, Highlight, FontName, FontSize, Super, Sub, NewComment,
    Sort, LineSpacing, ParaBorders, Title, Subtitle, ShowHide, ToggleComments, ToggleNav, DarkMode, AutoHideRibbon,
    InsertField, PageBreak, ToggleNotes, InsertTable, InsertSymbol, EditHeader, EditFooter,
    RowAbove, RowBelow, ColLeft, ColRight, DelRow, DelCol, DelTable, PrintLayout, ToggleRuler,
    // Dialog-box launchers (open advanced dialogs — placeholder until we have a
    // dialog system).
    LaunchFont, LaunchParagraph,
}

/// The common Word line-spacing presets, as (multiple, `w:line` twips). The Line
/// Spacing ribbon button cycles through these.
const LINE_SPACINGS: [(f32, i32); 4] = [(1.0, 240), (1.15, 276), (1.5, 360), (2.0, 480)];

/// Given the caret paragraph's current line multiple, the `w:line` value of the
/// next preset in the cycle. An unrecognized/absent value jumps to 1.5×, which is
/// a visible change from Word's single-spaced default.
fn next_line_spacing(cur: Option<f32>) -> i32 {
    let idx = cur.and_then(|c| LINE_SPACINGS.iter().position(|(m, _)| (m - c).abs() < 0.03));
    let next = match idx {
        Some(i) => (i + 1) % LINE_SPACINGS.len(),
        None => 2, // 1.5×
    };
    LINE_SPACINGS[next].1
}

// The two numbering ids new_markdown_package defines: 1 = bullets, 2 = decimal.
// Applying a list sets a paragraph's num_id to one of these, and save writes a
// numbering part that defines them (so the list renders in Word too).
const NUM_BULLET: i32 = 1;
const NUM_DECIMAL: i32 = 2;

/// A command with a Fluent icon id + ScreenTip (title = label, + shortcut).
fn cmdt(id: &'static str, icon: &'static str, label: &'static str, act: Act, shortcut: &'static str) -> rs::Cmd<Act> {
    rs::cmd(id, icon, label, act).tip(label, "", shortcut)
}

fn docxy_ribbon() -> rs::Ribbon<Act> {
    use Act::*;
    rs::Ribbon::new(vec![
        rs::tab("Home", "H", vec![
            // Clipboard: a large Paste button + a small Cut/Copy column (Word).
            rs::group("Clipboard", 10, vec![
                Control::Large(cmdt("paste", "paste", "Paste", Paste, "Ctrl+V").key("V")),
                rs::column(vec![
                    cmdt("cut", "cut", "Cut", Cut, "Ctrl+X").key("X"),
                    cmdt("copy", "copy", "Copy", Copy, "Ctrl+C").key("C"),
                ]),
            ]),
            // Font: two rows — combos + size controls on top, character toggles below.
            rs::group("Font", 40, vec![rs::rows(vec![
                vec![
                    rs::combo(cmdt("fontname", "font-name", "Font", FontName, ""), true),
                    rs::combo(cmdt("fontsize", "font-size", "Font size", FontSize, ""), false),
                    rs::btn(cmdt("grow", "font-increase", "Grow font", Grow, "").key("G")),
                    rs::btn(cmdt("shrink", "font-decrease", "Shrink font", Shrink, "").key("K")),
                    rs::btn(cmdt("case", "case", "Change case", Case, "").key("7")),
                    rs::btn(cmdt("clearfmt", "clear-format", "Clear formatting", ClearFmt, "").key("E")),
                ],
                vec![
                    rs::btn(cmdt("b", "bold", "Bold", Bold, "Ctrl+B").key("1")),
                    rs::btn(cmdt("i", "italic", "Italic", Italic, "Ctrl+I").key("2")),
                    rs::btn(cmdt("u", "underline", "Underline", Underline, "Ctrl+U").key("3")),
                    rs::btn(cmdt("s", "strikethrough", "Strikethrough", Strike, "").key("4")),
                    rs::btn(cmdt("sub", "subscript", "Subscript", Sub, "").key("5")),
                    rs::btn(cmdt("sup", "superscript", "Superscript", Super, "").key("6")),
                    rs::btn(cmdt("color", "text-color", "Font colour", FontColor, "").key("8")),
                    rs::btn(cmdt("hl", "highlight", "Text highlight", Highlight, "").key("9")),
                ],
            ])])
            .launcher(LaunchFont),
            // Paragraph: two rows — lists/indent/sort/marks on top, alignment below.
            rs::group("Paragraph", 30, vec![rs::rows(vec![
                vec![
                    rs::btn(cmdt("bullets", "list-bullet", "Bullets", Bullets, "").key("U")),
                    rs::btn(cmdt("numbers", "list-numbered", "Numbering", Numbers, "").key("N")),
                    rs::btn(cmdt("inddec", "indent-decrease", "Decrease indent", IndentDec, "Ctrl+Shift+M").key("O")),
                    rs::btn(cmdt("indinc", "indent-increase", "Increase indent", IndentInc, "Ctrl+M").key("P")),
                    rs::btn(cmdt("linespacing", "line-spacing", "Line and Paragraph Spacing", LineSpacing, "").key("Y")),
                    rs::btn(cmdt("sort", "sort", "Sort", Sort, "").key("S")),
                    rs::btn(cmdt("showhide", "paragraph", "Formatting marks", ShowHide, "").key("H")),
                ],
                vec![
                    rs::btn(cmdt("al", "align-left", "Align left", AlignL, "").key("L")),
                    rs::btn(cmdt("ac", "align-center", "Center", AlignC, "").key("A")),
                    rs::btn(cmdt("ar", "align-right", "Align right", AlignR, "").key("R")),
                    rs::btn(cmdt("aj", "align-justify", "Justify", AlignJ, "").key("J")),
                    rs::btn(cmdt("borders", "border-bottom", "Bottom border", ParaBorders, "").key("B")),
                ],
            ])])
            .launcher(LaunchParagraph),
            // Styles: a gallery of style thumbnails (Word keeps this on Home).
            rs::group("Styles", 35, vec![Control::Gallery(rs::Gallery {
                id: "styles",
                tip: rs::ScreenTip::default(),
                items: vec![
                    rs::GalleryItem { label: "Normal", preview: "normal", act: Normal },
                    rs::GalleryItem { label: "Title", preview: "title", act: Title },
                    rs::GalleryItem { label: "Subtitle", preview: "subtitle", act: Subtitle },
                    rs::GalleryItem { label: "Heading 1", preview: "h1", act: H1 },
                    rs::GalleryItem { label: "Heading 2", preview: "h2", act: H2 },
                    rs::GalleryItem { label: "Heading 3", preview: "h3", act: H3 },
                ],
            })]),
            // Editing: a labelled column (Word: Find / Replace / Select).
            rs::group("Editing", 20, vec![rs::column(vec![
                cmdt("find", "find", "Find & Replace", Find, "Ctrl+F").key("F"),
                cmdt("selall", "select-all", "Select all", SelectAll, "Ctrl+A").key("D"),
            ])]),
        ]),
        // Insert: headline commands as large buttons (Word's Insert tab style).
        rs::tab("Insert", "N", vec![
            rs::group("Pages", 40, vec![Control::Large(cmdt("pagebreak", "rule", "Page Break", PageBreak, "").key("B"))]),
            rs::group("Tables", 35, vec![Control::Large(cmdt("table", "table", "Table", InsertTable, "").key("T"))]),
            rs::group("Header & Footer", 34, vec![
                Control::Large(cmdt("header", "header", "Edit Header", EditHeader, "").key("H")),
                Control::Large(cmdt("footer", "footer", "Edit Footer", EditFooter, "").key("O")),
            ]),
            rs::group("Text", 30, vec![Control::Large(cmdt("field", "case", "Field", InsertField, "").key("Q"))]),
            rs::group("Symbols", 20, vec![
                Control::Large(cmdt("symbol", "symbol", "Symbol", InsertSymbol, "").key("S")),
                Control::Large(cmdt("hr", "rule", "Rule", HRule, "").key("L")),
            ]),
        ]),
        // Review: a large New Comment + a small pane-toggle column, then Editing.
        rs::tab("Review", "R", vec![
            rs::group("Comments", 40, vec![
                Control::Large(cmdt("newcomment", "comment-add", "New Comment", NewComment, "").key("C")),
                rs::column(vec![
                    cmdt("togglecomments", "comment", "Comments pane", ToggleComments, "").key("P"),
                    cmdt("togglenotes", "comment", "Notes pane", ToggleNotes, "").key("O"),
                ]),
            ]),
            rs::group("Editing", 30, vec![rs::column(vec![
                cmdt("find", "find", "Find & Replace", Find, "Ctrl+F").key("F"),
                cmdt("selall", "select-all", "Select all", SelectAll, "Ctrl+A").key("D"),
                cmdt("case", "case", "Change case", Case, "").key("7"),
            ])]),
        ]),
        // View: a large Print Layout toggle, then Show and Appearance columns.
        rs::tab("View", "W", vec![
            rs::group("Views", 40, vec![Control::Large(cmdt("printlayout", "print-layout", "Print Layout", PrintLayout, "").key("P"))]),
            rs::group("Show", 30, vec![rs::column(vec![
                cmdt("ruler", "rule", "Ruler", ToggleRuler, "").key("R"),
                cmdt("showhide", "paragraph", "Formatting marks", ShowHide, "").key("M"),
                cmdt("nav", "select-all", "Navigation", ToggleNav, "").key("N"),
                cmdt("viewcomments", "comment", "Comments pane", ToggleComments, "").key("C"),
            ])]),
            rs::group("Appearance", 20, vec![rs::column(vec![
                cmdt("darkmode", "case", "Theme", DarkMode, "").key("T"),
                cmdt("autohide", "rule", "Collapse ribbon", AutoHideRibbon, "Ctrl+F1").key("A"),
            ])]),
        ]),
    ])
}

fn ribbon_tab_index(t: RibbonTab) -> usize {
    match t {
        RibbonTab::Home => 0,
        RibbonTab::Insert => 1,
        RibbonTab::Review => 2,
        RibbonTab::View => 3,
        RibbonTab::Table => 0, // handled specially (see ribbon_body / table_tab)
    }
}

/// Find the command in a control whose KeyTip matches `key` (case-insensitive).
fn control_keytip(c: &Control<Act>, key: &str) -> Option<Act> {
    let m = |cmd: &rs::Cmd<Act>| (!cmd.key_tip.is_empty() && cmd.key_tip.eq_ignore_ascii_case(key)).then_some(cmd.act);
    match c {
        Control::Large(cmd) | Control::Toggle(cmd) => m(cmd),
        Control::Column(cmds) => cmds.iter().find_map(m),
        Control::Rows(rows) => rows.iter().flatten().find_map(|cell| match cell {
            rs::Cell::Btn(cmd) => m(cmd),
            rs::Cell::Combo { cmd, .. } => m(cmd),
        }),
        _ => None,
    }
}

/// Find a command in a tab by its KeyTip letter.
fn tab_keytip_cmd(tab: &rs::Tab<Act>, key: &str) -> Option<Act> {
    tab.groups.iter().flat_map(|g| g.items.iter()).find_map(|c| control_keytip(c, key))
}

/// A small KeyTip access-key badge, centred at the bottom of its host element.
fn keytip_badge(text: &str) -> AnyElement {
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_end()
        .justify_center()
        .child(div().px(px(3.)).rounded(px(2.)).bg(hsla_u(0xf2d24b)).text_size(px(9.)).text_color(hsla_u(0x1a1a1a)).child(SharedString::from(text.to_string())))
        .into_any_element()
}

/// The contextual Table Tools tab, shown only while the caret is in a table.
fn table_tab() -> rs::Tab<Act> {
    use Act::*;
    rs::tab("Table", "T", vec![
        rs::group("Rows & Columns", 40, vec![rs::rows(vec![
            vec![
                rs::btn(cmdt("rowabove", "table-insert-row", "Insert row above", RowAbove, "")),
                rs::btn(cmdt("colleft", "table-insert-column", "Insert column left", ColLeft, "")),
                rs::btn(cmdt("delrow", "table-delete-row", "Delete row", DelRow, "")),
            ],
            vec![
                rs::btn(cmdt("rowbelow", "table-insert-row", "Insert row below", RowBelow, "")),
                rs::btn(cmdt("colright", "table-insert-column", "Insert column right", ColRight, "")),
                rs::btn(cmdt("delcol", "table-delete-column", "Delete column", DelCol, "")),
            ],
        ])]),
        rs::group("Table", 20, vec![rs::column(vec![
            cmdt("deltable", "table-dismiss", "Delete table", DelTable, ""),
        ])]),
    ])
}

/// Rough natural width (px) of a group, for responsive collapse decisions.
fn group_est(g: &rs::Group<Act>, icon_only: bool) -> f32 {
    let mut w: f32 = 22.0;
    for c in &g.items {
        w += match c {
            Control::Toggle(_) => 26.0,
            Control::Large(_) => 58.0,
            Control::Column(_) => {
                if icon_only {
                    34.0
                } else {
                    104.0
                }
            }
            // A two-row grid: its width is that of the widest row.
            Control::Rows(rows) => rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| match cell {
                            rs::Cell::Combo { wide, .. } => {
                                if *wide {
                                    106.0
                                } else {
                                    48.0
                                }
                            }
                            rs::Cell::Btn(_) => 25.0,
                        })
                        .sum::<f32>()
                })
                .fold(0.0_f32, f32::max),
            Control::Gallery(gal) => gal.items.len() as f32 * 80.0,
            Control::Separator => 10.0,
            _ => 30.0,
        };
    }
    w.max(44.0)
}

fn move_vert(ed: &mut Editor, down: bool) {
    if ed.caret.path.len() != 1 {
        return;
    }
    let i = ed.caret.path[0];
    let col = ed.caret.offset;
    let n = ed.doc.body.len();
    let candidates: Vec<usize> = if down { (i + 1..n).collect() } else { (0..i).rev().collect() };
    for j in candidates {
        if let Block::Paragraph(p) = &ed.doc.body[j] {
            let len = p.plain_text().chars().count();
            ed.caret = Caret::at(vec![j], col.min(len));
            ed.clear_selection();
            return;
        }
    }
}

// ---- doc rendering (theme-aware) -------------------------------------------

fn hsla_u(c: u32) -> Hsla {
    rgb(c).into()
}

fn hex_rgb(s: &str) -> Option<u32> {
    let s = s.trim_start_matches('#');
    (s.len() == 6).then(|| u32::from_str_radix(s, 16).ok()).flatten()
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Map a Word highlight name (`w:highlight`) to an approximate RGB, and whether
/// its text should read dark (true) or light (false).
fn highlight_rgb(name: &str) -> (u32, bool) {
    match name {
        "yellow" => (0xFFFF00, true),
        "green" => (0x00FF00, true),
        "cyan" => (0x00FFFF, true),
        "magenta" => (0xFF00FF, true),
        "blue" => (0x0000FF, false),
        "red" => (0xFF0000, false),
        "darkYellow" => (0x808000, false),
        "darkGreen" => (0x008000, false),
        "darkCyan" => (0x008080, false),
        "darkMagenta" => (0x800080, false),
        "darkBlue" => (0x000080, false),
        "darkRed" => (0x800000, false),
        "lightGray" => (0xC0C0C0, true),
        "darkGray" => (0x808080, false),
        "black" => (0x000000, false),
        "white" => (0xFFFFFF, true),
        _ => (0xFFF29A, true),
    }
}

/// Office-style font-colour swatches (hex, no `#`). `None` = automatic (clear).
const COLOR_SWATCHES: &[u32] = &[
    0x000000, 0x404040, 0x808080, 0xBFBFBF, 0xFFFFFF, 0xC00000, 0xFF0000, 0xFFC000, 0xFFFF00, 0x92D050, 0x00B050, 0x00B0F0, 0x0070C0, 0x002060, 0x7030A0,
];
/// Highlight swatches (Word highlight names). `None` = no highlight (clear).
const HIGHLIGHT_SWATCHES: &[&str] = &["yellow", "green", "cyan", "magenta", "blue", "red", "darkYellow", "darkGreen", "darkCyan", "darkRed", "darkBlue", "lightGray"];
/// Font families offered in the Font-name picker.
const FONT_NAMES: &[&str] = &["Calibri", "Cambria", "Arial", "Times New Roman", "Georgia", "Verdana", "Tahoma", "Segoe UI", "Courier New", "Consolas", "Comic Sans MS"];
/// Point sizes offered in the Font-size picker.
const FONT_SIZES: &[u32] = &[8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 28, 36, 48, 72];

/// Snap a twips measurement to the nearest 1/8" ruler gridline (180 twips), so
/// dragging a ruler marker sticks to the visible ticks like Word's ruler.
fn snap_twips(v: i32) -> i32 {
    const GRID: i32 = 180; // 1/8 inch
    ((v as f32 / GRID as f32).round() as i32) * GRID
}

fn caret_bar() -> AnyElement {
    // Negative side margins cancel the 2px width so the caret takes no layout
    // space — it sits between glyphs without nudging them apart to make room.
    div().w(px(2.)).h(px(19.)).ml(px(-1.)).mr(px(-1.)).bg(rgb(BRAND)).into_any_element()
}

/// Synchronous text-width measurement (via the window's text system + the ambient
/// base font), so a tab can advance content to an absolute tab-stop column
/// instead of a fixed gap.
struct Measurer {
    ts: std::sync::Arc<WindowTextSystem>,
    base: Font,
}

impl Measurer {
    fn new(window: &Window) -> Self {
        Measurer { ts: window.text_system().clone(), base: window.text_style().font() }
    }
    /// Rendered pixel width of `text` at `size` px in the base font (bold/italic
    /// applied), matching how `emit_words` shapes it.
    fn width(&self, text: &str, size: f32, bold: bool, italic: bool) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let mut font = self.base.clone();
        if bold {
            font.weight = FontWeight::BOLD;
        }
        if italic {
            font.style = FontStyle::Italic;
        }
        let run = TextRun { len: text.len(), font, color: hsla_u(0), ..Default::default() };
        f32::from(self.ts.shape_line(SharedString::from(text.to_string()), px(size), &[run], None).width())
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_words(out: &mut Vec<AnyElement>, text: &str, props: &RunProps, base: f32, is_link: bool, selected: bool, click: Option<Click>, seg_start: usize, pal: Pal) {
    let mut off = seg_start;
    for word in text.split_inclusive(' ') {
        if word.is_empty() {
            continue;
        }
        let word_off = off;
        off += word.chars().count();
        let mut size = props.size_half_pts.map(|h| h as f32 / 2.0 * 1.333).unwrap_or(base);
        // Superscript / subscript render ~0.7× and are nudged up/down.
        let vshift = match props.vert_align {
            VertAlign::Superscript => {
                size *= 0.72;
                Some(-(base * 0.35))
            }
            VertAlign::Subscript => {
                size *= 0.72;
                Some(base * 0.18)
            }
            VertAlign::Baseline => None,
        };
        let color: Hsla = if is_link {
            hsla_u(LINK)
        } else {
            props.color.as_deref().and_then(hex_rgb).map(hsla_u).unwrap_or(pal.fg)
        };
        // Render each word as a StyledText so its TextLayout gives pixel-exact
        // hit-testing (index_for_position), for a true click-anywhere caret.
        let word_str = word.to_string();
        let styled = StyledText::new(word_str.clone());
        let layout = styled.layout().clone();
        out.push(
            div()
                .child(styled)
                .text_size(px(size))
                .text_color(color)
                .when(props.bold, |d| d.font_weight(FontWeight::BOLD))
                .when(props.italic, |d| d.italic())
                .when(props.underline || is_link, |d| d.underline())
                .when(props.strike, |d| d.line_through())
                .when_some(vshift, |d, dy| d.relative().top(px(dy)))
                // Selection wins over any run highlight so the selected range reads
                // as one contiguous band.
                .when_some(props.highlight.as_deref().filter(|_| !selected), |d, name| {
                    let (c, dark) = highlight_rgb(name);
                    d.bg(rgb(c)).text_color(if dark { rgb(0x1a1a1a) } else { rgb(0xf5f5f5) })
                })
                .when(selected, |d| d.bg(pal.sel))
                // Click / drag-to-caret: place the caret at the exact character under
                // the pointer (byte index from the layout → char count within the
                // word), and while the button is held extend the selection.
                .when_some(click, |d, c| {
                    let ent = c.ent.clone();
                    let path = c.path.to_vec();
                    let off_at = {
                        let layout = layout.clone();
                        let word_str = word_str.clone();
                        move |pos| {
                            let byte = layout.index_for_position(pos).unwrap_or_else(|e| e).min(word_str.len());
                            word_off + word_str[..byte].chars().count()
                        }
                    };
                    d.cursor_text()
                        .on_mouse_down(MouseButton::Left, {
                            let ent = ent.clone();
                            let path = path.clone();
                            let off_at = off_at.clone();
                            move |ev, window, cx| {
                                cx.stop_propagation();
                                let extend = ev.modifiers.shift;
                                let off = off_at(ev.position);
                                ent.update(cx, |this, cx| this.begin_select(path.clone(), off, extend, window, cx));
                            }
                        })
                        .on_mouse_move(move |ev, _window, cx| {
                            let off = off_at(ev.position);
                            let path = path.clone();
                            ent.update(cx, move |this, cx| {
                                if this.selecting {
                                    this.extend_select(path, off, cx);
                                }
                            });
                        })
                })
                .into_any_element(),
        );
    }
}

/// Emit one run's text, splicing in the caret bar and shading any part that falls
/// inside the selection range `sel` (both are absolute char offsets within the
/// paragraph). `idx` is advanced by the run's char length.
#[allow(clippy::too_many_arguments)]
fn emit_run(
    out: &mut Vec<AnyElement>,
    text: &str,
    props: &RunProps,
    base: f32,
    is_link: bool,
    idx: &mut usize,
    caret: &mut Option<usize>,
    sel: Option<(usize, usize)>,
    click: Option<Click>,
    pal: Pal,
) {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let start = *idx;
    let end = start + len;
    // Cut the run at the caret and at each selection boundary that lands inside it,
    // so each resulting segment is uniformly selected-or-not.
    let mut cuts: Vec<usize> = vec![0, len];
    let add = |abs: usize, cuts: &mut Vec<usize>| {
        if abs > start && abs < end {
            cuts.push(abs - start);
        }
    };
    if let Some(c) = *caret {
        add(c, &mut cuts);
    }
    if let Some((s, e)) = sel {
        add(s, &mut cuts);
        add(e, &mut cuts);
    }
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if *caret == Some(start + a) {
            out.push(caret_bar());
            *caret = None;
        }
        let seg: String = chars[a..b].iter().collect();
        let selected = sel.map_or(false, |(s, e)| s < e && start + a >= s && start + b <= e);
        emit_words(out, &seg, props, base, is_link, selected, click, start + a, pal);
    }
    if *caret == Some(end) {
        out.push(caret_bar());
        *caret = None;
    }
    *idx = end;
}

/// A tab is ONE char in the engine; render it as a fixed-width spacer that
/// advances to its tab stop (`width` px, computed by the caller). Keeps `idx` in
/// sync and participates in caret/selection like any other char.
#[allow(clippy::too_many_arguments)]
fn emit_tab(out: &mut Vec<AnyElement>, idx: &mut usize, caret: &mut Option<usize>, sel: Option<(usize, usize)>, click: Option<Click>, marks: bool, base: f32, width: f32, pal: Pal) {
    let pos = *idx;
    if *caret == Some(pos) {
        out.push(caret_bar());
        *caret = None;
    }
    let selected = sel.map_or(false, |(s, e)| s < e && s <= pos && pos < e);
    let w = width.max(3.0);
    out.push(
        div()
            .flex_none()
            .w(px(w))
            .h(px(base))
            .overflow_hidden()
            // With formatting marks on, a tab arrow sits at the start of the gap.
            .when(marks, |d| d.flex().items_center().text_size(px(base * 0.9)).text_color(pal.dim).child("\u{2192}"))
            .when(selected, |d| d.bg(pal.sel))
            .when_some(click, |d, c| {
                let ent = c.ent.clone();
                let path = c.path.to_vec();
                d.cursor_text().on_mouse_down(MouseButton::Left, move |ev, window, cx| {
                    cx.stop_propagation();
                    let extend = ev.modifiers.shift;
                    ent.update(cx, |this, cx| this.set_caret(path.clone(), pos, extend, window, cx));
                })
            })
            .into_any_element(),
    );
    *idx += 1;
}

/// A line break is ONE char in the engine; render it as a wrap and keep `idx` in
/// sync (so caret/selection offsets past it stay correct).
fn emit_break(out: &mut Vec<AnyElement>, idx: &mut usize, caret: &mut Option<usize>) {
    if *caret == Some(*idx) {
        out.push(caret_bar());
        *caret = None;
    }
    out.push(div().w_full().h(px(0.)).into_any_element());
    *idx += 1;
}

/// Compute the list marker text for each top-level block (`None` = not a list
/// item). Bullets use •/◦ by level; decimal lists get real ordinals that restart
/// per level and break whenever a non-list block interrupts the run.
/// Extract the header (or footer) block content from a package: resolve the
/// section's header/footerReference rId → part → parse. Empty if none.
// OOXML namespaces used when re-serializing an edited header/footer part.
const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const M_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";

/// Resolve the package part name (e.g. `word/header1.xml`) backing a specific
/// header/footer reference type (`"default"`, `"first"`, `"even"`).
fn hf_part_name_typed(pkg: &Package, is_header: bool, wtype: &str) -> Option<String> {
    let sect = pkg.sect_pr();
    let kind = if is_header { "headerReference" } else { "footerReference" };
    let rid = docxcore::load::header_footer_ref_rid(sect, kind, wtype)?;
    let rels_bytes = pkg.part("word/_rels/document.xml.rels")?;
    let rels = docxcore::load::parse_rels_xml(&String::from_utf8_lossy(rels_bytes));
    let target = rels.target(&rid)?;
    Some(format!("word/{}", target.trim_start_matches('/')))
}

/// Blocks of a specific header/footer variant, or empty if that ref is absent.
fn header_footer_blocks_typed(pkg: &Package, is_header: bool, wtype: &str) -> Vec<Block> {
    match hf_part_name_typed(pkg, is_header, wtype) {
        Some(name) => parse_hf_part(pkg, &name),
        None => vec![],
    }
}

/// Parse the blocks of a specific header/footer part.
fn parse_hf_part(pkg: &Package, part_name: &str) -> Vec<Block> {
    let Some(rels_bytes) = pkg.part("word/_rels/document.xml.rels") else { return vec![] };
    let rels = docxcore::load::parse_rels_xml(&String::from_utf8_lossy(rels_bytes));
    let Some(xml) = pkg.part(part_name) else { return vec![] };
    docxcore::load::parse_header_footer(&String::from_utf8_lossy(xml), &rels)
}


/// Render header/footer blocks read-only (no caret, no click) for the page margins.
fn hf_els(blocks: &[Block], pal: Pal, meas: &Measurer, hf_width: f32) -> Vec<AnyElement> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph(p) => Some(paragraph_el(p, None, None, None, None, false, 1.0, pal, Some(meas), Some(hf_width))),
            _ => None,
        })
        .collect()
}

/// Rough rendered height (px) of a block, for paginating Print Layout. Text is
/// estimated from a character-per-line calc; close enough to place page breaks.
fn block_height_est(b: &Block, content_w: f32) -> f32 {
    match b {
        Block::Paragraph(p) => {
            let base = match p.props.heading_level {
                Some(1) => 26.0,
                Some(2) => 22.0,
                Some(3) => 19.0,
                Some(4) => 17.0,
                Some(_) => 15.0,
                None => 14.5,
            };
            let lh = base * 1.4;
            let chars = p.plain_text().chars().count().max(1) as f32;
            let cpl = (content_w / (base * 0.5)).max(1.0);
            let breaks = p.content.iter().filter(|i| matches!(i, Inline::Break(_))).count() as f32;
            let lines = (chars / cpl).ceil().max(1.0) + breaks;
            lines * lh + if p.props.heading_level.is_some() { base } else { 4.0 }
        }
        Block::Table(t) => t.rows.len() as f32 * 30.0 + 8.0,
        Block::Raw(_) => 0.0,
    }
}

/// Does this block force a page break (a `w:br` of type page)?
fn has_page_break(b: &Block) -> bool {
    matches!(b, Block::Paragraph(p) if p.content.iter().any(|i| matches!(i, Inline::Break(docxcore::model::BreakKind::Page))))
}

/// Group top-level block indices into pages by accumulated estimated height and
/// hard page breaks. Returns `[start, end)` block ranges, one per page.
fn paginate(blocks: &[Block], content_h: f32, content_w: f32) -> Vec<(usize, usize)> {
    let mut pages = Vec::new();
    let mut start = 0usize;
    let mut acc = 0.0_f32;
    for (i, b) in blocks.iter().enumerate() {
        let bh = block_height_est(b, content_w);
        if acc + bh > content_h && i > start {
            pages.push((start, i));
            start = i;
            acc = 0.0;
        }
        acc += bh;
        if has_page_break(b) {
            pages.push((start, i + 1));
            start = i + 1;
            acc = 0.0;
        }
    }
    if start < blocks.len() {
        pages.push((start, blocks.len()));
    }
    if pages.is_empty() {
        pages.push((0, blocks.len()));
    }
    pages
}

fn list_markers(body: &[Block]) -> Vec<Option<String>> {
    let mut out = Vec::with_capacity(body.len());
    let mut counts: Vec<u32> = Vec::new();
    for b in body {
        let marker = match b {
            Block::Paragraph(p) if p.props.num_id.is_some() => {
                let ilvl = p.props.ilvl as usize;
                if p.props.num_id == Some(NUM_DECIMAL) {
                    if counts.len() <= ilvl {
                        counts.resize(ilvl + 1, 0);
                    }
                    counts.truncate(ilvl + 1); // returning to a shallower level restarts deeper ones
                    counts[ilvl] += 1;
                    Some(format!("{}. ", counts[ilvl]))
                } else {
                    counts.clear();
                    Some(if ilvl % 2 == 1 { "\u{25E6} ".to_string() } else { "\u{2022} ".to_string() })
                }
            }
            _ => {
                counts.clear();
                None
            }
        };
        out.push(marker);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn paragraph_el(p: &Paragraph, mut caret: Option<usize>, sel: Option<(usize, usize)>, marker: Option<&str>, click: Option<Click>, marks: bool, zoom: f32, pal: Pal, meas: Option<&Measurer>, hf_width: Option<f32>) -> AnyElement {
    use docxcore::model::TabAlign;
    let base = zoom
        * match p.props.heading_level {
            Some(1) => 26.0,
            Some(2) => 22.0,
            Some(3) => 19.0,
            Some(4) => 17.0,
            Some(_) => 15.0,
            None => 14.5,
        };
    let is_heading = p.props.heading_level.is_some();
    let mut spans: Vec<AnyElement> = Vec::new();
    let mut idx = 0usize;

    // Tab-stop geometry. `x` tracks the running pixel width from the row's left
    // edge; a tab advances it to the next stop measured from the text margin. The
    // row's left edge sits `pad_l` px in from the margin (the paragraph indent).
    let pad_l = zoom * ((p.props.indent.max(0) as f32) / 15.0 + p.props.ilvl.max(0) as f32 * 20.0);
    let interval = zoom * 720.0 / 15.0; // Word's default tab stop: every 1/2"
    let mut customs: Vec<(f32, TabAlign)> = p.props.tabs.iter().map(|t| (zoom * t.pos as f32 / 15.0, t.align)).collect();
    // Header/footer paragraphs with no explicit tabs get Word's implicit centre +
    // right stops, so a centred title / right-aligned page number lands correctly.
    if customs.is_empty() {
        if let Some(w) = hf_width {
            customs.push((w * 0.5, TabAlign::Center));
            // A few px inside the edge so a right-aligned segment ending exactly at
            // the content width doesn't trip the row's flex-wrap onto a new line.
            customs.push(((w - 4.0).max(0.0), TabAlign::Right));
        }
    }
    let max_custom = customs.iter().map(|(c, _)| *c).fold(f32::NEG_INFINITY, f32::max);
    let mut x = 0.0_f32;
    // Only paragraphs that actually contain a tab need per-run width measurement.
    let has_tab = p.content.iter().any(|i| matches!(i, Inline::Tab(_)));
    let m = meas.filter(|_| has_tab);
    // Width of a run's text at its effective size (matching emit_words' sizing).
    let run_w = |r: &docxcore::model::Run| -> f32 {
        match m {
            Some(m) => {
                let sz = r.props.size_half_pts.map(|h| h as f32 / 2.0 * 1.333).unwrap_or(base);
                m.width(&r.text, sz, r.props.bold, r.props.italic)
            }
            None => 0.0,
        }
    };
    // The rendered width of a single inline (for tracking x and looking ahead to
    // size centre/right tabs). Inlines with no measurable text contribute 0.
    let inline_w = |it: &Inline| -> f32 {
        match it {
            Inline::Run(r) => run_w(r),
            Inline::Hyperlink(h) => h.runs.iter().map(run_w).sum(),
            Inline::Field { text, .. } => m.map(|m| m.width(if text.is_empty() { "[field]" } else { text }, base, false, false)).unwrap_or(0.0),
            Inline::FootnoteRef { id, .. } => m.map(|m| m.width(&id.to_string(), base * 0.72, false, false)).unwrap_or(0.0),
            _ => 0.0,
        }
    };
    // Total width of content from index `from` up to the next tab / break / end —
    // the segment a centre/right tab must position.
    let seg_width = |from: usize| -> f32 {
        p.content[from..].iter().take_while(|it| !matches!(it, Inline::Tab(_) | Inline::Break(_))).map(&inline_w).sum()
    };
    // The next tab stop strictly past `xm` (twips-px from the margin) and its
    // alignment: the nearest custom stop, else the default 1/2" grid (defaults
    // are suppressed up to the last custom stop, as Word does).
    let next_stop = |xm: f32| -> (f32, TabAlign) {
        let mut pos = f32::INFINITY;
        let mut align = TabAlign::Left;
        for &(c, a) in &customs {
            if c > xm + 0.5 && c < pos {
                pos = c;
                align = a;
            }
        }
        let lo = xm.max(if max_custom.is_finite() { max_custom } else { 0.0 });
        let mut d = ((lo / interval).floor() + 1.0) * interval;
        while d <= xm + 0.5 {
            d += interval;
        }
        if d < pos {
            pos = d;
            align = TabAlign::Left;
        }
        (pos, align)
    };

    if let Some(m) = marker {
        // The marker isn't document content — render it plain (non-clickable) so it
        // never maps clicks to bogus offsets.
        spans.push(div().text_size(px(base)).text_color(pal.dim).child(SharedString::from(m.to_string())).into_any_element());
        if let Some(ms) = meas.filter(|_| has_tab) {
            x += ms.width(m, base, false, false);
        }
    }
    for i in 0..p.content.len() {
        let inline = &p.content[i];
        match inline {
            Inline::Run(r) => {
                emit_run(&mut spans, &r.text, &r.props, base, false, &mut idx, &mut caret, sel, click, pal);
                x += run_w(r);
            }
            Inline::Hyperlink(h) => {
                for r in &h.runs {
                    emit_run(&mut spans, &r.text, &r.props, base, true, &mut idx, &mut caret, sel, click, pal);
                    x += run_w(r);
                }
            }
            Inline::Tab(_) => {
                // Advance to the next stop; centre/right stops position the segment
                // that follows (up to the next tab) so it centres on / ends at it.
                let xm = pad_l + x;
                let (stop, align) = next_stop(xm);
                let w = match align {
                    TabAlign::Left => stop - xm,
                    TabAlign::Right => stop - seg_width(i + 1) - xm,
                    TabAlign::Center => stop - seg_width(i + 1) / 2.0 - xm,
                }
                .max(3.0);
                emit_tab(&mut spans, &mut idx, &mut caret, sel, click, marks, base, w, pal);
                x += w;
            }
            Inline::Break(_) => {
                emit_break(&mut spans, &mut idx, &mut caret);
                x = 0.0; // a hard break restarts the line
            }
            Inline::Raw(xml) => {
                // Comment reference → a small badge; other raw XML (range markers,
                // bookmarks, …) is invisible content and renders nothing.
                if xml.contains("commentReference") {
                    spans.push(
                        div()
                            .px(px(3.))
                            .rounded(px(3.))
                            .bg(hsla_u(0xF2C744))
                            .text_size(px(9.))
                            .text_color(rgb(0x1a1a1a))
                            .relative()
                            .top(px(-(base * 0.35)))
                            .child("\u{1F4AC}")
                            .into_any_element(),
                    );
                }
            }
            Inline::Field { text, .. } => {
                // Show the field's cached value with a subtle shade so it reads as a
                // field, not plain text.
                let shown = if text.is_empty() { "[field]".to_string() } else { text.clone() };
                spans.push(div().px(px(2.)).rounded_sm().bg(pal.panel).text_size(px(base)).text_color(pal.fg).child(SharedString::from(shown)).into_any_element());
                x += inline_w(inline);
            }
            Inline::FootnoteRef { id, .. } => {
                // A superscript note number in the brand colour.
                spans.push(div().text_size(px(base * 0.72)).text_color(hsla_u(BRAND)).relative().top(px(-(base * 0.35))).child(SharedString::from(id.to_string())).into_any_element());
                x += inline_w(inline);
            }
            other => {
                let tag = match other {
                    Inline::SmartArt { .. } => "[diagram]",
                    Inline::Chart { .. } => "[chart]",
                    Inline::Equation { .. } => "[equation]",
                    Inline::TextBox { .. } => "[textbox]",
                    _ => "[image]",
                };
                spans.push(div().px_1().rounded_sm().bg(pal.panel).text_size(px(12.)).text_color(pal.dim).child(tag).into_any_element());
            }
        }
    }
    if caret.is_some() {
        spans.push(caret_bar());
    }
    // A pilcrow at the paragraph end when formatting marks are shown.
    if marks {
        spans.push(div().text_size(px(base)).text_color(pal.dim).child("\u{00B6}").into_any_element());
    }
    let has_border = p.props.borders.bottom.is_some();
    // Line spacing (auto-rule multiple; exact/atLeast fall back to single here).
    let line_mult = p.props.spacing.line_multiple().unwrap_or(1.0).clamp(0.5, 4.0);
    let line_h = base * 1.35 * line_mult;
    let mut row = h_flex().w_full().flex_wrap().min_h(px(line_h.max(base + 6.))).line_height(px(line_h));
    row = match p.props.align {
        Align::Center => row.justify_center(),
        Align::Right => row.justify_end(),
        _ => row,
    };
    // Leading indent: explicit paragraph indent (twips → px at ~96dpi) plus a step
    // per list nesting level.
    let pad = zoom * ((p.props.indent.max(0) as f32) / 15.0 + p.props.ilvl.max(0) as f32 * 20.0);
    let pad_r = zoom * (p.props.indent_right.max(0) as f32) / 15.0;
    // The whole paragraph area is a click fallback (empty space past the text, the
    // indent gutter) that drops the caret at the paragraph end. Word clicks fire
    // first and stop propagation, so this only runs on a "past the text" click.
    let para_end = idx;
    // Space before / after the paragraph (`w:spacing` before/after, twips → px).
    let sp_before = zoom * (p.props.spacing.before.unwrap_or(0).max(0) as f32) / 15.0;
    let sp_after = zoom * (p.props.spacing.after.unwrap_or(0).max(0) as f32) / 15.0;
    v_flex()
        .w_full()
        .py_0p5()
        .when(sp_before > 0.5, |d| d.pt(px(sp_before)))
        .when(sp_after > 0.5, |d| d.pb(px(sp_after)))
        .pl(px(pad))
        .pr(px(pad_r))
        .when(is_heading, |d| d.mt_2())
        .when(has_border, |d| d.border_b_1().border_color(pal.fg).pb_1())
        .when_some(click, |d, c| {
            let ent = c.ent.clone();
            let path = c.path.to_vec();
            d.cursor_text().on_mouse_down(MouseButton::Left, move |ev, window, cx| {
                let extend = ev.modifiers.shift;
                ent.update(cx, |this, cx| this.set_caret(path.clone(), para_end, extend, window, cx));
            })
        })
        .child(row.children(spans))
        .into_any_element()
}

fn table_el(t: &Table, path: &[usize], ctx: RenderCtx) -> AnyElement {
    let mut rows = Vec::new();
    for (ri, row) in t.rows.iter().enumerate() {
        let mut cells = Vec::new();
        for (ci, cell) in row.cells.iter().enumerate() {
            let inner: Vec<AnyElement> = cell
                .blocks
                .iter()
                .enumerate()
                .map(|(k, b)| {
                    let mut cp = path.to_vec();
                    cp.extend_from_slice(&[ri, ci, k]);
                    block_el(b, cp, None, ctx)
                })
                .collect();
            cells.push(v_flex().flex_1().px_2().py_1().border_1().border_color(ctx.pal.border).children(inner).into_any_element());
        }
        rows.push(h_flex().w_full().children(cells).into_any_element());
    }
    v_flex().w_full().my_2().children(rows).into_any_element()
}

/// Render one block at absolute `path`, wiring caret/selection/click from `ctx`.
fn block_el(b: &Block, path: Vec<usize>, marker: Option<&str>, ctx: RenderCtx) -> AnyElement {
    match b {
        Block::Paragraph(p) => {
            let caret = (ctx.active && ctx.caret_path == path.as_slice()).then_some(ctx.caret_off);
            let sel = ctx.active.then(|| ctx.spans.iter().find(|(pp, _, _)| pp.as_slice() == path.as_slice()).map(|(_, s, e)| (*s, *e))).flatten();
            let click = ctx.active.then_some(Click { ent: ctx.ent, path: &path });
            paragraph_el(p, caret, sel, marker, click, ctx.marks, ctx.zoom, ctx.pal, Some(ctx.meas), ctx.hf_width)
        }
        Block::Table(t) => table_el(t, &path, ctx),
        Block::Raw(_) => div().h(px(0.)).into_any_element(),
    }
}

// ---- chrome: ribbon + backstage --------------------------------------------

impl Docxy {
    fn ribbon_tabs(&self, fg: Hsla, dim: Hsla, panel: Hsla, cx: &mut Context<Self>) -> AnyElement {
        let names = ["File", "Home", "Insert", "Review", "View"];
        let mut strip = h_flex().w_full().items_end().gap_1().px_2().pt_1().bg(panel);
        for (i, name) in names.iter().enumerate() {
            let is_file = i == 0;
            let this_tab = match i {
                1 => Some(RibbonTab::Home),
                2 => Some(RibbonTab::Insert),
                3 => Some(RibbonTab::Review),
                4 => Some(RibbonTab::View),
                _ => None,
            };
            let active = !self.backstage && this_tab == Some(self.ribbon_tab);
            let tab_key = ["F", "H", "N", "R", "W"][i];
            let show_kt = self.keytips == KeyTip::Tabs;
            strip = strip.child(
                div()
                    .id(("rtab", i))
                    .relative()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .rounded_t_sm()
                    .text_size(px(12.))
                    .when(!is_file, |d| d.hover(|d| d.bg(Hsla { a: 0.10, ..fg })))
                    .when(is_file, |d| d.bg(rgb(BRAND)).text_color(rgb(FILE_FG)).font_weight(FontWeight::BOLD).rounded_t_sm())
                    .when(active, |d| d.text_color(rgb(BRAND)).border_b_2().border_color(rgb(BRAND)))
                    .when(!active && !is_file, |d| d.text_color(fg))
                    .child(*name)
                    .when(show_kt, |d| d.child(keytip_badge(tab_key)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if is_file {
                            this.backstage = true;
                            this.bs_new = false;
                            cx.notify();
                        } else if let Some(t) = this_tab {
                            this.ribbon_tab = t;
                            this.refocus(window, cx);
                        }
                    })),
            );
        }
        // Contextual Table Tools tab — only while the caret is in a table. It reads
        // with a coloured accent (Word shows contextual tabs tinted).
        if self.caret_table().is_some() {
            let active = !self.backstage && self.ribbon_tab == RibbonTab::Table;
            let accent = hsla_u(0xC0_5B_2E); // a warm contextual accent
            strip = strip.child(
                div()
                    .id(("rtab", 99usize))
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .rounded_t_sm()
                    .text_size(px(12.))
                    .text_color(accent)
                    .hover(|d| d.bg(Hsla { a: 0.10, ..accent }))
                    .when(active, |d| d.border_b_2().border_color(accent).font_weight(FontWeight::BOLD))
                    .child("Table")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.ribbon_tab = RibbonTab::Table;
                        this.refocus(window, cx);
                    })),
            );
        }
        // right side: the ribbon collapse/expand chevron
        strip = strip.child(div().flex_1());
        strip = strip.child(
            div()
                .id("ribbon-min")
                .px_2()
                .py_1()
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(fg)
                .child(if self.ribbon_min { "\u{2304}" } else { "\u{2303}" })
                .tooltip(|w, cx| Tooltip::new("Collapse the ribbon  \u{00b7}  Ctrl+F1").build(w, cx))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.ribbon_min = !this.ribbon_min;
                    this.refocus(window, cx);
                })),
        );
        let _ = dim;
        strip.into_any_element()
    }

    /// Dispatch a ribbon command to the engine / app, then refocus the document.
    fn launch_msg(&mut self, msg: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.status = SharedString::from(msg.to_string());
        }
        self.refocus(window, cx);
    }

    /// The right-click context menu (clipboard + quick formatting), anchored at the
    /// click position, over a full-window backdrop that dismisses it.
    fn context_menu_el(&self, at: Point<Pixels>, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let item = |cx: &mut Context<Self>, id: &'static str, label: &'static str, icon: &'static str, act: Act| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1()
                .rounded_sm()
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(pal.fg)
                .hover(|d| d.bg(pal.hover))
                .when(!icon.is_empty(), |d| d.child(icon_svg(icon, 14., pal.fg)))
                .when(icon.is_empty(), |d| d.child(div().w(px(14.))))
                .child(SharedString::from(label))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.context_menu = None;
                    this.dispatch(act, window, cx);
                }))
        };
        let sep = || div().h(px(1.)).mx_2().my_0p5().bg(pal.border);
        let menu = v_flex()
            .absolute()
            .left(at.x)
            .top(at.y)
            .w(px(200.))
            .py_1()
            .rounded_md()
            .bg(pal.panel)
            .border_1()
            .border_color(pal.border)
            .shadow_lg()
            .child(item(cx, "cm-cut", "Cut", "cut", Act::Cut))
            .child(item(cx, "cm-copy", "Copy", "copy", Act::Copy))
            .child(item(cx, "cm-paste", "Paste", "paste", Act::Paste))
            .child(sep())
            .child(item(cx, "cm-bold", "Bold", "bold", Act::Bold))
            .child(item(cx, "cm-italic", "Italic", "italic", Act::Italic))
            .child(item(cx, "cm-underline", "Underline", "underline", Act::Underline))
            .child(sep())
            .child(item(cx, "cm-comment", "New Comment", "comment-add", Act::NewComment));
        // Full-window backdrop to catch outside clicks / right-clicks.
        div()
            .id("cm-backdrop")
            .absolute()
            .inset_0()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _w, cx| {
                this.context_menu = None;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Right, cx.listener(|this, _, _w, cx| {
                this.context_menu = None;
                cx.notify();
            }))
            .child(menu)
            .into_any_element()
    }

    /// The floating mini formatting toolbar, anchored just above the selection.
    fn mini_bar_el(&self, at: Point<Pixels>, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let btn = |cx: &mut Context<Self>, id: &'static str, icon: &'static str, act: Act| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded(px(3.))
                .cursor_pointer()
                .when(self.act_active(act), |d| d.bg(Hsla { a: 0.20, ..hsla_u(BRAND) }))
                .hover(|d| d.bg(pal.hover))
                .child(icon_svg(icon, 15., pal.fg))
                .on_click(cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)))
        };
        h_flex()
            .absolute()
            .left(at.x)
            .top((at.y - px(36.)).max(px(2.)))
            .items_center()
            .gap(px(1.))
            .p_0p5()
            .rounded_md()
            .bg(pal.panel)
            .border_1()
            .border_color(pal.border)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(btn(cx, "mb-b", "bold", Act::Bold))
            .child(btn(cx, "mb-i", "italic", Act::Italic))
            .child(btn(cx, "mb-u", "underline", Act::Underline))
            .child(btn(cx, "mb-s", "strikethrough", Act::Strike))
            .child(div().w(px(1.)).h(px(18.)).mx_0p5().bg(pal.border))
            .child(btn(cx, "mb-grow", "font-increase", Act::Grow))
            .child(btn(cx, "mb-shrink", "font-decrease", Act::Shrink))
            .child(btn(cx, "mb-color", "text-color", Act::FontColor))
            .child(btn(cx, "mb-hl", "highlight", Act::Highlight))
            .into_any_element()
    }

    /// Handle a letter pressed while KeyTips are showing: pick a tab (Tabs level)
    /// or run a command (Commands level).
    fn keytip_input(&mut self, c: &str, window: &mut Window, cx: &mut Context<Self>) {
        match self.keytips {
            KeyTip::Tabs => {
                if c.eq_ignore_ascii_case("F") {
                    self.keytips = KeyTip::Off;
                    self.backstage = true;
                    self.bs_new = false;
                    return cx.notify();
                }
                let ribbon = docxy_ribbon();
                if let Some(i) = ribbon.tabs.iter().position(|t| t.key_tip.eq_ignore_ascii_case(c)) {
                    self.ribbon_tab = match i {
                        0 => RibbonTab::Home,
                        1 => RibbonTab::Insert,
                        2 => RibbonTab::Review,
                        _ => RibbonTab::View,
                    };
                    self.keytips = KeyTip::Commands;
                } else if c.eq_ignore_ascii_case("T") && self.caret_table().is_some() {
                    self.ribbon_tab = RibbonTab::Table;
                    self.keytips = KeyTip::Commands;
                } else {
                    self.keytips = KeyTip::Off;
                }
                cx.notify();
            }
            KeyTip::Commands => {
                let ribbon = docxy_ribbon();
                let table = table_tab();
                let tab = if self.ribbon_tab == RibbonTab::Table { &table } else { &ribbon.tabs[ribbon_tab_index(self.ribbon_tab)] };
                let act = tab_keytip_cmd(tab, c);
                self.keytips = KeyTip::Off;
                if let Some(a) = act {
                    self.dispatch(a, window, cx);
                } else {
                    cx.notify();
                }
            }
            KeyTip::Off => {}
        }
    }

    fn dispatch(&mut self, act: Act, window: &mut Window, cx: &mut Context<Self>) {
        use Act::*;
        match act {
            Cut => self.do_copy(true, window, cx),
            Copy => self.do_copy(false, window, cx),
            Paste => self.do_paste(window, cx),
            LaunchFont => self.launch_msg("Font — advanced dialog coming soon", window, cx),
            LaunchParagraph => self.launch_msg("Paragraph — advanced dialog coming soon", window, cx),
            Find => self.toggle_find(window, cx),
            FontColor => self.toggle_picker(PickKind::Color, window, cx),
            Highlight => self.toggle_picker(PickKind::Highlight, window, cx),
            FontName => self.toggle_picker(PickKind::FontName, window, cx),
            FontSize => self.toggle_picker(PickKind::FontSize, window, cx),
            NewComment => self.start_comment(window, cx),
            ShowHide => {
                self.show_marks = !self.show_marks;
                self.refocus(window, cx);
            }
            ToggleComments => {
                self.show_comments = !self.show_comments;
                self.refocus(window, cx);
            }
            ToggleNav => {
                self.show_nav = !self.show_nav;
                self.refocus(window, cx);
            }
            DarkMode => self.cycle_theme(window, cx),
            AutoHideRibbon => {
                self.ribbon_min = !self.ribbon_min;
                self.refocus(window, cx);
            }
            InsertField => self.toggle_picker(PickKind::Field, window, cx),
            InsertTable => self.toggle_picker(PickKind::Table, window, cx),
            InsertSymbol => self.toggle_picker(PickKind::Symbol, window, cx),
            EditHeader => self.enter_hf(true, "default", window, cx),
            EditFooter => self.enter_hf(false, "default", window, cx),
            RowAbove | RowBelow | ColLeft | ColRight | DelRow | DelCol | DelTable => self.table_op(act, window, cx),
            PrintLayout => {
                self.page_view = !self.page_view;
                self.refocus(window, cx);
            }
            ToggleRuler => {
                self.show_ruler = !self.show_ruler;
                self.refocus(window, cx);
            }
            PageBreak => self.insert_page_break(window, cx),
            ToggleNotes => {
                self.show_notes = !self.show_notes;
                self.refocus(window, cx);
            }
            _ => self.with_editor(window, cx, |e| match act {
                Bold => e.toggle_bold(),
                Italic => e.toggle_italic(),
                Underline => e.toggle_underline(),
                Strike => e.toggle_strike(),
                Super => e.toggle_vert_align(VertAlign::Superscript),
                Sub => e.toggle_vert_align(VertAlign::Subscript),
                Grow => e.resize_font(2),
                Shrink => e.resize_font(-2),
                AlignL => e.set_align(Align::Left),
                AlignC => e.set_align(Align::Center),
                AlignR => e.set_align(Align::Right),
                AlignJ => e.set_align(Align::Justify),
                Normal => e.set_para_style(None),
                H1 => e.set_para_style(Some("Heading1")),
                H2 => e.set_para_style(Some("Heading2")),
                H3 => e.set_para_style(Some("Heading3")),
                HRule => e.insert_hrule(),
                SelectAll => e.select_all(),
                Case => e.cycle_case(),
                // Toggle: if every selected paragraph is already in this list, drop
                // it; otherwise apply it.
                Bullets => e.set_list((!e.all_in_list(NUM_BULLET)).then_some(NUM_BULLET)),
                Numbers => e.set_list((!e.all_in_list(NUM_DECIMAL)).then_some(NUM_DECIMAL)),
                IndentInc => e.change_indent(720),
                IndentDec => e.change_indent(-720),
                Sort => e.sort_paragraphs(),
                // Word's Line Spacing button is a dropdown; we cycle the common
                // presets (1.0 → 1.15 → 1.5 → 2.0 → back), all auto-rule.
                LineSpacing => e.set_line_spacing(next_line_spacing(e.caret_line_multiple()), "auto"),
                ParaBorders => {
                    let has = e.caret_para_props().borders.bottom.is_some();
                    let b = if has { ParBorders::default() } else { ParBorders { top: None, bottom: Some(BorderKind::Single) } };
                    e.set_para_border(b);
                }
                Title => e.set_para_style(Some("Title")),
                Subtitle => e.set_para_style(Some("Subtitle")),
                ClearFmt => e.clear_run_formatting(),
                Cut | Copy | Paste | LaunchFont | LaunchParagraph | Find | FontColor | Highlight | FontName | FontSize | NewComment | ShowHide | ToggleComments | ToggleNav | DarkMode | AutoHideRibbon | InsertField | PageBreak | ToggleNotes | InsertTable | InsertSymbol | EditHeader | EditFooter | RowAbove | RowBelow | ColLeft | ColRight | DelRow | DelCol | DelTable | PrintLayout | ToggleRuler => {}
            }),
        }
    }

    /// The ribbon body for the active tab, rendered from the shared ribbonspec
    /// model with Fluent icons — and RESPONSIVE: as `width` drops, control labels
    /// are dropped first, then the lowest-`priority` groups collapse into an
    /// overflow indicator (Office-style scaling driven by ribbonspec::priority).
    fn ribbon_body(&self, width: f32, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let ribbon = docxy_ribbon();
        let ctx_tab = table_tab();
        let tab = if self.ribbon_tab == RibbonTab::Table { &ctx_tab } else { &ribbon.tabs[ribbon_tab_index(self.ribbon_tab)] };
        let avail = (width - 28.0).max(120.0);

        // 1) drop control labels if the full layout overflows.
        let icon_only = tab.groups.iter().map(|g| group_est(g, false)).sum::<f32>() > avail;
        // 2) collapse lowest-priority groups until what remains fits.
        let mut shown: Vec<usize> = (0..tab.groups.len()).collect();
        loop {
            let total: f32 = shown.iter().map(|&i| group_est(&tab.groups[i], icon_only)).sum();
            if total <= avail || shown.len() <= 1 {
                break;
            }
            let victim = *shown.iter().min_by_key(|&&i| tab.groups[i].priority).unwrap();
            shown.retain(|&i| i != victim);
        }
        let hidden = tab.groups.len() - shown.len();

        let mut groups: Vec<AnyElement> =
            shown.iter().map(|&i| self.render_group(&tab.groups[i], icon_only, pal, cx)).collect();
        if hidden > 0 {
            groups.push(
                v_flex()
                    .items_center()
                    .justify_center()
                    .h_full()
                    .px_2()
                    .gap_1()
                    .text_color(pal.dim)
                    .child(div().text_size(px(18.)).child("\u{22EF}"))
                    .child(div().text_size(px(9.)).child(SharedString::from(format!("{hidden} more"))))
                    .into_any_element(),
            );
        }
        h_flex().w_full().h(px(98.)).items_stretch().px_1().bg(pal.panel).border_b_1().border_color(pal.border).children(groups).into_any_element()
    }

    fn render_group(&self, g: &rs::Group<Act>, icon_only: bool, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let controls: Vec<AnyElement> = g.items.iter().map(|c| self.render_control(c, icon_only, pal, cx)).collect();
        // group title row + optional dialog-box launcher (⤢)
        let title_row = h_flex()
            .items_center()
            .gap_1()
            .child(div().text_size(px(9.)).text_color(pal.dim).child(g.title))
            .when_some(g.launcher, |d, act| {
                d.child(
                    div()
                        .id(SharedString::from(format!("launch-{}", g.title)))
                        .text_size(px(10.))
                        .text_color(pal.dim)
                        .cursor_pointer()
                        .hover(|d| d.text_color(pal.fg))
                        .child("\u{2922}")
                        .tooltip(|w, cx| Tooltip::new("More options").build(w, cx))
                        .on_click(cx.listener(move |this, _, w, cx| this.dispatch(act, w, cx))),
                )
            });
        v_flex()
            .items_center()
            .justify_between()
            .h_full()
            .px_2()
            .py_0p5()
            .gap_0p5()
            .border_r_1()
            .border_color(pal.border)
            .child(h_flex().flex_1().items_center().gap_1().children(controls))
            .child(title_row)
            .into_any_element()
    }

    fn render_control(&self, c: &Control<Act>, icon_only: bool, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        match c {
            Control::Toggle(cmd) => self.icon_btn(cmd, false, pal, cx),
            Control::Large(cmd) => self.large_btn(cmd, pal, cx),
            Control::Column(cmds) => {
                // Office caps a button column at 3 rows; extra buttons wrap into
                // the next column so nothing overflows the ribbon body height.
                let cols: Vec<AnyElement> = cmds
                    .chunks(3)
                    .map(|chunk| {
                        v_flex()
                            .gap(px(1.))
                            .children(chunk.iter().map(|cm| self.icon_btn(cm, !icon_only, pal, cx)))
                            .into_any_element()
                    })
                    .collect();
                h_flex().items_start().gap_1().children(cols).into_any_element()
            }
            // The Office two-row layout: each inner Vec is one left-to-right row.
            Control::Rows(rows) => {
                let rendered: Vec<AnyElement> = rows
                    .iter()
                    .map(|row| {
                        h_flex()
                            .items_center()
                            .gap(px(1.))
                            .children(row.iter().map(|cell| self.render_cell(cell, pal, cx)))
                            .into_any_element()
                    })
                    .collect();
                v_flex().items_start().gap(px(2.)).children(rendered).into_any_element()
            }
            Control::Gallery(gal) => self.style_gallery(gal, pal, cx),
            Control::Separator => div().w(px(1.)).h(px(44.)).bg(pal.border).mx_1().into_any_element(),
            _ => div().into_any_element(),
        }
    }

    /// The Styles gallery: a row of thumbnail boxes, each showing its name in that
    /// style's own weight/size (Word's Style gallery).
    fn style_gallery(&self, gal: &rs::Gallery<Act>, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let cur = self.tabs.get(self.active).and_then(|t| if let Surface::Doc(ed) = &t.surface { ed.caret_para_style() } else { None });
        let boxes: Vec<AnyElement> = gal
            .items
            .iter()
            .map(|it| {
                let act = it.act;
                // Map the preview hint to a thumbnail appearance.
                let (size, weight) = match it.preview {
                    "title" => (16.0, FontWeight::BOLD),
                    "subtitle" => (12.0, FontWeight::NORMAL),
                    "h1" => (14.0, FontWeight::BOLD),
                    "h2" => (13.0, FontWeight::BOLD),
                    "h3" => (12.0, FontWeight::SEMIBOLD),
                    _ => (11.0, FontWeight::NORMAL),
                };
                let style_id = match it.preview {
                    "title" => Some("Title"),
                    "subtitle" => Some("Subtitle"),
                    "h1" => Some("Heading1"),
                    "h2" => Some("Heading2"),
                    "h3" => Some("Heading3"),
                    _ => None,
                };
                let selected = cur.as_deref() == style_id;
                div()
                    .id(it.label)
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(76.))
                    .h(px(40.))
                    .px_1()
                    .rounded(px(3.))
                    .border_1()
                    .border_color(if selected { hsla_u(BRAND) } else { pal.border })
                    .bg(pal.panel)
                    .cursor_pointer()
                    .hover(|d| d.border_color(hsla_u(BRAND)))
                    .child(div().text_size(px(size)).font_weight(weight).text_color(pal.fg).overflow_hidden().child(SharedString::from(it.label)))
                    .tooltip({
                        let label = it.label;
                        move |w, cx| Tooltip::new(label).build(w, cx)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)))
                    .into_any_element()
            })
            .collect();
        h_flex().items_center().gap_1().children(boxes).into_any_element()
    }

    fn render_cell(&self, cell: &rs::Cell<Act>, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        match cell {
            rs::Cell::Btn(cmd) => self.icon_btn(cmd, false, pal, cx),
            rs::Cell::Combo { cmd, wide } => self.combo_box(cmd, *wide, pal, cx),
        }
    }

    /// The current font family / size at the caret (for the Font combos).
    fn caret_run_props(&self) -> Option<RunProps> {
        match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => Some(ed.caret_props()),
            _ => None,
        }
    }

    /// A Font-group combo box showing the current value with a dropdown chevron.
    fn combo_box(&self, cmd: &rs::Cmd<Act>, wide: bool, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let props = self.caret_run_props();
        let value: SharedString = if cmd.id == "fontname" {
            props.and_then(|p| p.font).unwrap_or_else(|| "Calibri".into()).into()
        } else {
            props
                .and_then(|p| p.size_half_pts)
                .map(|h| {
                    let s = h as f32 / 2.0;
                    if s.fract() == 0.0 { format!("{}", s as u32) } else { format!("{s}") }
                })
                .unwrap_or_else(|| "11".into())
                .into()
        };
        let act = cmd.act;
        div()
            .id(cmd.id)
            .flex()
            .items_center()
            .justify_between()
            .gap_1()
            .w(px(if wide { 104. } else { 46. }))
            .h(px(22.))
            .px_1p5()
            .rounded(px(3.))
            .border_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .cursor_pointer()
            .hover(|d| d.border_color(hsla_u(BRAND)))
            .child(div().text_size(px(11.)).text_color(pal.fg).overflow_hidden().child(value))
            .child(div().text_size(px(8.)).text_color(pal.dim).child("\u{25BE}"))
            .on_click(cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)))
            .into_any_element()
    }

    /// A large icon-over-label ribbon button (e.g. Paste, Table).
    fn large_btn(&self, cmd: &rs::Cmd<Act>, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let act = cmd.act;
        let on = self.act_active(act);
        let tip: SharedString = cmd.label.into();
        let keytip = (self.keytips == KeyTip::Commands && !cmd.key_tip.is_empty()).then_some(cmd.key_tip);
        div()
            .id(cmd.id)
            .relative()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_0p5()
            .px_2()
            .h_full()
            .rounded(px(4.))
            .cursor_pointer()
            .when(on, |d| d.bg(Hsla { a: 0.20, ..hsla_u(BRAND) }))
            .hover(|d| d.bg(pal.hover))
            .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
            .child(icon_svg(cmd.icon.0, 26., pal.fg))
            .child(div().text_size(px(11.)).text_color(pal.fg).child(SharedString::from(cmd.label)))
            .when_some(keytip, |d, k| d.child(keytip_badge(k)))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)))
            .into_any_element()
    }

    /// Whether a toggle command is currently "on" for the caret's formatting, so
    /// the ribbon button can show a pressed state (Word highlights e.g. Bold when
    /// the caret sits in bold text).
    fn act_active(&self, act: Act) -> bool {
        use Act::*;
        let doc = match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Doc(ed)) => Some(ed),
            _ => None,
        };
        let rp = doc.map(|ed| ed.caret_props());
        let pp = doc.map(|ed| ed.caret_para_props());
        match act {
            Bold => rp.map_or(false, |p| p.bold),
            Italic => rp.map_or(false, |p| p.italic),
            Underline => rp.map_or(false, |p| p.underline),
            Strike => rp.map_or(false, |p| p.strike),
            Super => rp.map_or(false, |p| p.vert_align == VertAlign::Superscript),
            Sub => rp.map_or(false, |p| p.vert_align == VertAlign::Subscript),
            AlignL => pp.map_or(false, |p| p.align == Align::Left),
            AlignC => pp.map_or(false, |p| p.align == Align::Center),
            AlignR => pp.map_or(false, |p| p.align == Align::Right),
            AlignJ => pp.map_or(false, |p| p.align == Align::Justify),
            Bullets => doc.map_or(false, |ed| ed.all_in_list(NUM_BULLET)),
            Numbers => doc.map_or(false, |ed| ed.all_in_list(NUM_DECIMAL)),
            ParaBorders => pp.map_or(false, |p| p.borders.bottom.is_some()),
            ShowHide => self.show_marks,
            ToggleComments => self.show_comments,
            ToggleNav => self.show_nav,
            ToggleNotes => self.show_notes,
            PrintLayout => self.page_view,
            ToggleRuler => self.show_ruler,
            _ => false,
        }
    }

    fn icon_btn(&self, cmd: &rs::Cmd<Act>, show_label: bool, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let act = cmd.act;
        let tip = cmd.tip;
        let on = self.act_active(act);
        let tip_text: SharedString = if tip.shortcut.is_empty() {
            tip.title.into()
        } else {
            format!("{}  \u{00b7}  {}", tip.title, tip.shortcut).into()
        };
        let keytip = (self.keytips == KeyTip::Commands && !cmd.key_tip.is_empty()).then_some(cmd.key_tip);
        div()
            .id(cmd.id)
            .relative()
            .flex()
            .items_center()
            .gap_1p5()
            .px_2()
            .h(px(22.))
            .rounded(px(4.))
            .cursor_pointer()
            // Pressed/checked state: a soft brand wash + brand border, like Word.
            .when(on, |d| d.bg(Hsla { a: 0.20, ..hsla_u(BRAND) }).border_1().border_color(hsla_u(BRAND)))
            .when(!on, |d| d.border_1().border_color(gpui::transparent_black()))
            .hover(|d| d.bg(pal.hover))
            .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
            .child(icon_svg(cmd.icon.0, 16., pal.fg))
            .when(show_label, |d| d.child(div().text_size(px(12.)).text_color(pal.fg).child(SharedString::from(cmd.label))))
            .when_some(keytip, |d, k| d.child(keytip_badge(k)))
            .tooltip(move |window, cx| Tooltip::new(tip_text.clone()).build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| this.dispatch(act, window, cx)))
            .into_any_element()
    }

    fn backstage_view(&self, bg: Hsla, fg: Hsla, dim: Hsla, sidebar: Hsla, cx: &mut Context<Self>) -> AnyElement {
        let rail_item = |cx: &mut Context<Self>, id: &'static str, label: &'static str, f: fn(&mut Docxy, &mut Window, &mut Context<Docxy>)| {
            div()
                .id(id)
                .w_full()
                .px_4()
                .py_2()
                .cursor_pointer()
                .rounded_sm()
                .text_color(fg)
                .hover(|d| d.bg(rgb(BRAND)).text_color(rgb(FILE_FG)))
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| f(this, window, cx)))
        };

        let rail = v_flex()
            .w(px(220.))
            .h_full()
            .py_3()
            .gap_1()
            .bg(sidebar)
            .child(
                div()
                    .id("bs-back")
                    .px_4()
                    .py_2()
                    .cursor_pointer()
                    .text_color(rgb(BRAND))
                    .child("\u{2190} Back")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.backstage = false;
                        this.bs_new = false;
                        this.refocus(window, cx);
                    })),
            )
            .child(
                div()
                    .id("bs-new")
                    .w_full()
                    .px_4()
                    .py_2()
                    .cursor_pointer()
                    .rounded_sm()
                    .text_color(fg)
                    .hover(|d| d.bg(rgb(BRAND)).text_color(rgb(FILE_FG)))
                    .child("New")
                    .on_click(cx.listener(|this, _, _w, cx| {
                        this.bs_new = true;
                        cx.notify();
                    })),
            )
            .child(rail_item(cx, "bs-open", "Open\u{2026}", |t, w, cx| t.open_file(w, cx)))
            .child(rail_item(cx, "bs-save", "Save", |t, w, cx| t.save_active(w, cx)))
            .child(rail_item(cx, "bs-saveas", "Save As\u{2026}", |t, w, cx| t.save_as(w, cx)))
            .child(rail_item(cx, "bs-close", "Close", |t, w, cx| {
                let a = t.active;
                t.backstage = false;
                t.close_tab(a, w, cx);
            }));

        let pane = if self.bs_new {
            let card = |cx: &mut Context<Self>, id: &'static str, glyph: &'static str, name: &'static str, sub: &'static str, kind: Kind| {
                v_flex()
                    .id(id)
                    .w(px(150.))
                    .h(px(160.))
                    .p_3()
                    .gap_2()
                    .rounded_md()
                    .border_1()
                    .border_color(dim)
                    .cursor_pointer()
                    .hover(|d| d.border_color(rgb(BRAND)))
                    .child(div().text_size(px(40.)).child(glyph))
                    .child(div().text_color(fg).font_weight(FontWeight::BOLD).child(name))
                    .child(div().text_size(px(11.)).text_color(dim).child(sub))
                    .on_click(cx.listener(move |this, _, window, cx| this.add_tab(kind, window, cx)))
            };
            v_flex()
                .flex_1()
                .h_full()
                .p_8()
                .gap_4()
                .bg(bg)
                .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).text_color(fg).child("New"))
                .child(
                    h_flex()
                        .gap_4()
                        .child(card(cx, "new-doc-card", Kind::Docx.glyph(), "Document", "Blank .docx", Kind::Docx))
                        .child(card(cx, "new-xls-card", Kind::Xlsx.glyph(), "Spreadsheet", "Blank .xlsx", Kind::Xlsx))
                        .child(card(cx, "new-mail-card", Kind::Look.glyph(), "Mail", "New message", Kind::Look)),
                )
                .into_any_element()
        } else {
            let active = self.tabs.get(self.active);
            let (cur_title, cur_path) = active
                .map(|t| (t.title.to_string(), t.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "not saved yet".into())))
                .unwrap_or_else(|| ("—".into(), "".into()));
            let recents: Vec<AnyElement> = self
                .tabs
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    div()
                        .id(("recent", i))
                        .px_3()
                        .py_1p5()
                        .cursor_pointer()
                        .rounded_sm()
                        .text_color(fg)
                        .hover(|d| d.bg(sidebar))
                        .child(format!("{} {}", t.kind.glyph(), t.title))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.backstage = false;
                            this.select_tab(i, window, cx);
                        }))
                        .into_any_element()
                })
                .collect();
            v_flex()
                .flex_1()
                .h_full()
                .p_8()
                .gap_4()
                .bg(bg)
                .child(div().text_size(px(22.)).font_weight(FontWeight::BOLD).text_color(fg).child(cur_title))
                .child(div().text_size(px(12.)).text_color(dim).child(cur_path))
                .child(div().text_size(px(13.)).text_color(rgb(BRAND)).mt_4().child("Open"))
                .child(v_flex().gap_0p5().children(recents))
                .into_any_element()
        };

        h_flex().size_full().bg(bg).child(rail).child(pane).into_any_element()
    }
}

impl Render for Docxy {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Apply the theme choice (Auto follows the OS appearance).
        let desired = match self.theme_pref {
            ThemePref::Auto => ThemeMode::from(window.appearance()),
            ThemePref::Light => ThemeMode::Light,
            ThemePref::Dark => ThemeMode::Dark,
        };
        if self.applied != Some(desired) {
            Theme::change(desired, Some(window), cx);
            self.applied = Some(desired);
        }
        if !self.focused {
            self.focus.focus(window, cx);
            self.focused = true;
        }

        let t = cx.theme();
        let bg = t.background;
        let fg = t.foreground;
        let dim = t.muted_foreground;
        let border = t.border;
        let panel = t.secondary;
        let sidebar = t.sidebar;
        let tab_active = t.tab_active;
        // A theme-adaptive hover tint: a low-alpha wash of the foreground, so it's
        // clearly visible as a highlight on both light and dark grounds.
        let hover = Hsla { a: 0.12, ..fg };
        let pal = Pal { fg, dim, border, panel, hover, sel: t.selection };

        // --- title bar: wordmark + document tab chips + theme toggle ---
        let theme_pref = self.theme_pref;
        let chips: Vec<AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, tb)| {
                let active = i == self.active && !self.backstage;
                let mark = if tb.dirty { " \u{2022}" } else { "" };
                h_flex()
                    .id(("chip", i))
                    .items_center()
                    .gap_1()
                    .px_2()
                    .h(px(24.))
                    .rounded_sm()
                    .cursor_pointer()
                    .text_size(px(12.))
                    .when(active, |d| d.bg(tab_active).text_color(fg))
                    .when(!active, |d| d.text_color(dim))
                    .child(SharedString::from(format!("{} {}{}", tb.kind.glyph(), tb.title, mark)))
                    .child(
                        div()
                            .id(("chipx", i))
                            .px_1()
                            .rounded_sm()
                            .hover(|d| d.bg(border))
                            .child("\u{00d7}")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(i, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.select_tab(i, window, cx)))
                    .into_any_element()
            })
            .collect();

        // NOTE: the "docxy" brand label and the flex_1 spacer are plain,
        // non-interactive divs, so TitleBar's own drag region shows through them
        // — that idle space is natively draggable and double-click maximizes.
        // Only the *interactive* clusters (chips, theme button) swallow the
        // mouse-down so a drag on them doesn't start a window move.
        let title_bar = TitleBar::new().child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .pl_2()
                .child(div().font_weight(FontWeight::BOLD).text_color(rgb(BRAND)).child("docxy"))
                // Quick Access Toolbar: Undo / Redo (Word keeps these here, not on
                // the ribbon).
                .child(
                    h_flex()
                        .items_center()
                        .gap_0p5()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(qat_btn("qat-undo", "undo", "Undo (Ctrl+Z)", pal, cx.listener(|this, _, window, cx| this.with_editor(window, cx, |e| { e.undo(); }))))
                        .child(qat_btn("qat-redo", "redo", "Redo (Ctrl+Y)", pal, cx.listener(|this, _, window, cx| this.with_editor(window, cx, |e| { e.redo(); })))),
                )
                .child(
                    h_flex()
                        .items_center()
                        .gap_1()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .children(chips),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(Button::new("theme").ghost().xsmall().label(theme_pref.label()).on_click(cx.listener(|this, _, window, cx| this.cycle_theme(window, cx)))),
                ),
        );

        if self.backstage {
            let backstage = self.backstage_view(bg, fg, dim, sidebar, cx);
            return v_flex().size_full().bg(bg).track_focus(&self.focus).child(title_bar).child(backstage).into_any_element();
        }

        let is_doc = matches!(self.tabs.get(self.active).map(|t| &t.surface), Some(Surface::Doc(_)));
        // The contextual Table tab is only valid while the caret is in a table.
        if self.ribbon_tab == RibbonTab::Table && self.caret_table().is_none() {
            self.ribbon_tab = RibbonTab::Home;
        }
        let vw = f32::from(window.viewport_size().width);
        let ribbon_tabs = self.ribbon_tabs(fg, dim, panel, cx);
        let ribbon_body = (is_doc && !self.ribbon_min).then(|| self.ribbon_body(vw, pal, cx));
        let find_bar = (is_doc && self.find_open).then(|| self.find_bar(pal, cx));
        let picker_bar = (is_doc).then_some(self.picker).flatten().map(|k| self.picker_bar(k, pal, cx));
        let comment_bar = (is_doc && self.comment_open).then(|| self.comment_bar(pal, cx));
        let hf_bar = self.hf_active().then(|| self.hf_bar(pal, cx));
        let ruler = (is_doc && self.show_ruler).then(|| self.ruler(cx));

        // Text measurer for tab-stop positioning (shared across the document).
        let measurer = Measurer::new(window);
        let content: AnyElement = match self.tabs.get(self.active) {
            Some(tab) => match &tab.surface {
                Surface::Doc(editor) => {
                    let spans = editor.selection_spans();
                    let markers = list_markers(&editor.doc.body);
                    let ent = cx.entity();
                    // In Print Layout the sheet is always a light page (dark ink on
                    // white) regardless of the app theme, like Word's document surface.
                    let doc_pal = if self.page_view {
                        Pal { fg: hsla_u(0x202020), dim: hsla_u(0x808080), border: hsla_u(0xcccccc), panel: hsla_u(0xf0f0f0), hover: Hsla { a: 0.08, ..hsla_u(0x000000) }, sel: pal.sel }
                    } else {
                        pal
                    };
                    // While a header/footer is being edited the body is inactive
                    // (no caret, clicks inert) so it visually recedes.
                    let hf = tab.hf_edit.as_ref();
                    let ctx = RenderCtx { caret_path: &editor.caret.path, caret_off: editor.caret.offset, spans: &spans, ent: &ent, pal: doc_pal, marks: self.show_marks, zoom: self.zoom, active: hf.is_none(), meas: &measurer, hf_width: None };
                    let body = &editor.doc.body;
                    if self.page_view {
                        // Print Layout: split the body into discrete white page sheets
                        // (section margins), stacked on a grey canvas.
                        let geom = tab.pkg.as_ref().map(|p| p.page_geom()).unwrap_or_default();
                        let zoom = self.zoom;
                        let tw = move |t: i32| px(zoom * (t.max(0) as f32) / 15.0); // twips → px @ ~96dpi, zoomed
                        let canvas = if self.applied == Some(ThemeMode::Dark) { hsla_u(0x2b2b2b) } else { hsla_u(0x9a9a9a) };
                        let content_h = (geom.h - geom.mt - geom.mb).max(1) as f32 / 15.0;
                        let content_w = (geom.w - geom.ml - geom.mr).max(1) as f32 / 15.0;
                        let ranges = paginate(body, content_h, content_w);
                        let show_ruler = self.show_ruler;
                        // Per-page header/footer. A section can carry distinct
                        // first-page (w:titlePg) and even-page (evenAndOddHeaders)
                        // variants; every other page uses the "default" one.
                        let pkg = tab.pkg.as_ref();
                        let title_pg = pkg.is_some_and(|p| p.has_title_pg());
                        let even_odd = pkg.is_some_and(|p| p.has_even_odd());
                        let refp = |kind: &str, wt: &str| pkg.is_some_and(|p| docxcore::load::header_footer_ref_rid(p.sect_pr(), kind, wt).is_some());
                        let (h_first_ref, h_even_ref) = (refp("headerReference", "first"), refp("headerReference", "even"));
                        let (f_first_ref, f_even_ref) = (refp("footerReference", "first"), refp("footerReference", "even"));
                        let parse = |is_h: bool, wt: &str| pkg.map(|p| header_footer_blocks_typed(p, is_h, wt)).unwrap_or_default();
                        let (hdef, hfirst, heven) = (parse(true, "default"), parse(true, "first"), parse(true, "even"));
                        let (fdef, ffirst, feven) = (parse(false, "default"), parse(false, "first"), parse(false, "even"));
                        let variant_for = |page1: usize, is_h: bool| -> &'static str {
                            let (fr, ev) = if is_h { (h_first_ref, h_even_ref) } else { (f_first_ref, f_even_ref) };
                            if page1 == 1 && title_pg && fr {
                                "first"
                            } else if page1 % 2 == 0 && even_odd && ev {
                                "even"
                            } else {
                                "default"
                            }
                        };
                        let pick = |is_h: bool, wt: &str| -> &[Block] {
                            match (is_h, wt) {
                                (true, "first") => &hfirst,
                                (true, "even") => &heven,
                                (true, _) => &hdef,
                                (false, "first") => &ffirst,
                                (false, "even") => &feven,
                                (false, _) => &fdef,
                            }
                        };
                        // Header/footer text-area width, for the implicit centre/right tab stops.
                        let hf_w = self.zoom * (geom.w - geom.ml - geom.mr).max(0) as f32 / 15.0;
                        let hf_spans = hf.map(|h| h.editor.selection_spans()).unwrap_or_default();
                        let hf_ctx = hf.map(|h| RenderCtx { caret_path: &h.editor.caret.path, caret_off: h.editor.caret.offset, spans: &hf_spans, ent: &ent, pal: doc_pal, marks: false, zoom: self.zoom, active: true, meas: &measurer, hf_width: Some(hf_w) });
                        // The first page whose region+variant matches the one being
                        // edited is the editable page (fallback page 0, so the surface
                        // is always visible even for a not-yet-shown variant).
                        let edit_page = hf.map(|h| (0..ranges.len()).find(|&i| variant_for(i + 1, h.is_header) == h.variant).unwrap_or(0));
                        let has_hf = [&hdef, &hfirst, &heven, &fdef, &ffirst, &feven].iter().any(|v| !v.is_empty()) || hf.is_some();
                        // One region's margin content for a given page: the live editor
                        // blocks (editable on the edit page), else the read-only variant.
                        let region_children = |pi: usize, is_h: bool| -> Vec<AnyElement> {
                            let dv = variant_for(pi + 1, is_h);
                            if let (Some(h), Some(ep)) = (hf, edit_page) {
                                if h.is_header == is_h {
                                    if pi == ep {
                                        return h.editor.doc.body.iter().enumerate().map(|(i, b)| block_el(b, vec![i], None, hf_ctx.unwrap())).collect();
                                    }
                                    let blocks: &[Block] = if dv == h.variant { &h.editor.doc.body } else { pick(is_h, dv) };
                                    return hf_els(blocks, doc_pal, &measurer, hf_w);
                                }
                            }
                            hf_els(pick(is_h, dv), doc_pal, &measurer, hf_w)
                        };
                        let sheets: Vec<AnyElement> = ranges
                            .iter()
                            .copied()
                            .enumerate()
                            .map(|(pi, (s, e))| {
                                let page_blocks: Vec<AnyElement> = (s..e).map(|i| block_el(&body[i], vec![i], markers[i].as_deref(), ctx)).collect();
                                let hdr_children = region_children(pi, true);
                                let ftr_children = region_children(pi, false);
                                let edit_hdr_here = hf.is_some_and(|h| h.is_header) && Some(pi) == edit_page;
                                let edit_ftr_here = hf.is_some_and(|h| !h.is_header) && Some(pi) == edit_page;
                                let page_base = v_flex()
                                    .w(tw(geom.w))
                                    .min_h(tw(geom.h))
                                    .bg(hsla_u(0xffffff))
                                    .text_color(doc_pal.fg)
                                    .border_1()
                                    .border_color(hsla_u(0xd0d0d0));
                                // Tint the region actively being edited on this page.
                                let tint = Hsla { a: 0.5, ..hsla_u(0xeef4ff) };
                                let hdr_bg = if edit_hdr_here { tint } else { hsla_u(0xffffff) };
                                let ftr_bg = if edit_ftr_here { tint } else { hsla_u(0xffffff) };
                                let page = if has_hf {
                                    // Header in the top margin, content in the middle, footer
                                    // in the bottom margin. The body area exits header/footer
                                    // editing on click (Word's "click the document to leave").
                                    let mut mid = v_flex().flex_1().pl(tw(geom.ml)).pr(tw(geom.mr)).gap_1().children(page_blocks);
                                    if hf.is_some() {
                                        let ent2 = ent.clone();
                                        mid = mid.cursor_pointer().on_mouse_down(MouseButton::Left, move |_ev, window, cx| {
                                            ent2.update(cx, |this, cx| this.exit_hf(window, cx));
                                        });
                                    }
                                    page_base
                                        .child(div().min_h(tw(geom.mt)).pt(tw(geom.mt / 2)).pl(tw(geom.ml)).pr(tw(geom.mr)).bg(hdr_bg).children(hdr_children))
                                        .child(mid)
                                        .child(div().min_h(tw(geom.mb)).pl(tw(geom.ml)).pr(tw(geom.mr)).bg(ftr_bg).children(ftr_children))
                                } else {
                                    page_base.pt(tw(geom.mt)).pr(tw(geom.mr)).pb(tw(geom.mb)).pl(tw(geom.ml)).gap_1().children(page_blocks)
                                };
                                h_flex()
                                    .items_stretch()
                                    .gap(px(3.))
                                    .when(show_ruler, |d| d.child(self.vruler()))
                                    .child(page)
                                    .into_any_element()
                            })
                            .collect();
                        v_flex()
                            .id("doc-scroll")
                            .track_scroll(&self.doc_scroll)
                            .flex_1()
                            .h_full()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .bg(canvas)
                            .items_center()
                            .gap(px(18.))
                            .py(px(24.))
                            .children(sheets)
                            .into_any_element()
                    } else {
                        let blocks: Vec<AnyElement> = body.iter().enumerate().map(|(i, b)| block_el(b, vec![i], markers[i].as_deref(), ctx)).collect();
                        v_flex().id("doc-scroll").track_scroll(&self.doc_scroll).flex_1().h_full().min_h(px(0.)).overflow_y_scroll().bg(bg).text_color(fg).px(px(48.)).py(px(28.)).gap_1().children(blocks).into_any_element()
                    }
                }
                Surface::Placeholder => placeholder(tab.kind, bg, dim).into_any_element(),
            },
            None => v_flex().flex_1().bg(bg).items_center().justify_center().text_color(dim).child("No documents — File \u{203A} New").into_any_element(),
        };

        // Word-style counts for the status bar's left cluster: total pages
        // (estimated by the same paginator Print Layout uses) and word count.
        let doc_stats: Option<(usize, usize)> = is_doc
            .then(|| self.tabs.get(self.active))
            .flatten()
            .and_then(|tab| match &tab.surface {
                Surface::Doc(ed) => {
                    let words = ed.doc.plain_text().split_whitespace().count();
                    let geom = tab.pkg.as_ref().map(|p| p.page_geom()).unwrap_or_default();
                    let ch = (geom.h - geom.mt - geom.mb).max(1) as f32 / 15.0;
                    let cw = (geom.w - geom.ml - geom.mr).max(1) as f32 / 15.0;
                    let pages = paginate(&ed.doc.body, ch, cw).len().max(1);
                    Some((words, pages))
                }
                _ => None,
            });
        let stats_text = doc_stats.map(|(w, p)| {
            SharedString::from(format!(
                "{} page{} \u{00b7} {} word{}",
                p,
                if p == 1 { "" } else { "s" },
                w,
                if w == 1 { "" } else { "s" }
            ))
        });

        let zoom_btn = |cx: &mut Context<Self>, id: &'static str, glyph: &'static str, delta: f32| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(16.))
                .rounded_sm()
                .cursor_pointer()
                .text_color(fg)
                .hover(|d| d.bg(pal.hover))
                .child(glyph)
                .on_click(cx.listener(move |this, _, window, cx| {
                    let z = if delta == 0.0 { 1.0 } else { this.zoom + delta };
                    this.zoom = z.clamp(0.5, 3.0);
                    this.refocus(window, cx);
                }))
        };
        let status = h_flex()
            .w_full()
            .px_4()
            .py_1()
            .gap_2()
            .bg(panel)
            .text_size(px(11.))
            .text_color(dim)
            .child(self.tabs.get(self.active).map(|t| t.status.clone()).unwrap_or_default())
            .when_some(stats_text, |d, s| d.child(div().text_color(dim).child("·")).child(div().text_color(dim).child(s)))
            .child(div().flex_1())
            .child("type · Ctrl+B/I/U · Ctrl+F find · Ctrl+C/X/V · Ctrl+Z/Y · Ctrl+S")
            // Zoom controls (Word's bottom-right zoom).
            .child(zoom_btn(cx, "zoom-out", "\u{2212}", -0.1))
            .child(div().id("zoom-pct").min_w(px(34.)).flex().justify_center().cursor_pointer().hover(|d| d.text_color(fg)).child(SharedString::from(format!("{}%", (self.zoom * 100.0).round() as i32))).on_click(cx.listener(|this, _, window, cx| { this.zoom = 1.0; this.refocus(window, cx); })))
            .child(zoom_btn(cx, "zoom-in", "+", 0.1));

        // The body is the document, flanked by the navigation and comments panes.
        let nav_panel = (is_doc && self.show_nav).then(|| self.nav_panel(pal, cx));
        let comments_panel = (is_doc && self.show_comments).then(|| self.comments_panel(pal, cx));
        let notes_panel = (is_doc && self.show_notes).then(|| self.notes_panel(pal, cx));
        let body = h_flex()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            // Right-click anywhere in the document body opens the context menu.
            .on_mouse_down(MouseButton::Right, cx.listener(|this, ev: &MouseDownEvent, _w, cx| {
                this.context_menu = Some(ev.position);
                cx.notify();
            }))
            .when_some(nav_panel, |d, n| d.child(n))
            .child(content)
            .when_some(comments_panel, |d, p| d.child(p))
            .when_some(notes_panel, |d, p| d.child(p));
        let context_menu = self.context_menu.map(|at| self.context_menu_el(at, pal, cx));
        let mini_bar = (is_doc && self.context_menu.is_none()).then_some(self.mini_bar).flatten().map(|at| self.mini_bar_el(at, pal, cx));

        v_flex()
            .size_full()
            .relative()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .on_action(cx.listener(|this, _: &InsertTabAction, window, cx| this.tab_key(window, cx)))
            .on_action(cx.listener(|this, _: &OutdentAction, window, cx| this.shift_tab_key(window, cx)))
            // Ruler drags are tracked at the window level so they keep working when
            // the pointer leaves the thin ruler strip (gpui move events are
            // hitbox-scoped, so a ruler-only handler would stop the moment the
            // cursor moved off it).
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| {
                if this.ruler_drag.is_some() {
                    this.ruler_drag_move(f32::from(ev.position.x), window, cx);
                }
            }))
            // End a text drag-selection or a ruler drag wherever the button is
            // released; a non-empty text selection pops the mini formatting toolbar.
            .on_mouse_up(MouseButton::Left, cx.listener(|this, ev: &MouseUpEvent, _w, cx| {
                if this.ruler_drag.is_some() {
                    this.ruler_drag_end(cx);
                }
                if this.selecting {
                    this.selecting = false;
                    let has_sel = matches!(this.tabs.get(this.active).map(|t| &t.surface), Some(Surface::Doc(ed)) if ed.has_selection());
                    this.mini_bar = has_sel.then_some(ev.position);
                    cx.notify();
                }
            }))
            .bg(bg)
            .child(title_bar)
            .child(ribbon_tabs)
            .when_some(ribbon_body, |d, r| d.child(r))
            .when_some(find_bar, |d, f| d.child(f))
            .when_some(picker_bar, |d, p| d.child(p))
            .when_some(comment_bar, |d, c| d.child(c))
            .when_some(hf_bar, |d, b| d.child(b))
            .when_some(ruler, |d, r| d.child(r))
            .child(body)
            .child(status)
            .when_some(mini_bar, |d, m| d.child(m))
            .when_some(context_menu, |d, m| d.child(m))
            // A vertical guide line down the page while a ruler marker is dragged.
            .when_some(self.ruler_guide, |d, gx| {
                d.child(div().absolute().top_0().bottom_0().left(px(gx)).w(px(1.)).bg(Hsla { a: 0.6, ..hsla_u(BRAND) }))
            })
            .into_any_element()
    }
}

fn placeholder(kind: Kind, bg: Hsla, dim: Hsla) -> impl IntoElement {
    let (name, blurb) = match kind {
        Kind::Xlsx => ("xlsxy", "the spreadsheet grid (gridcore) lands here next"),
        Kind::Look => ("lookxy", "mail list + reading pane (mailcore) lands here next"),
        Kind::Docx => ("docxy", ""),
    };
    v_flex()
        .flex_1()
        .bg(bg)
        .items_center()
        .justify_center()
        .gap_2()
        .child(div().text_color(rgb(BRAND)).font_weight(FontWeight::BOLD).text_size(px(20.)).child(name))
        .child(div().text_color(dim).child(blurb))
}

fn main() {
    gpui_platform::application().with_assets(DocxyAssets).run(move |cx: &mut App| {
        gpui_component::init(cx);
        // Tab / Shift-Tab are reserved by gpui's focus system; bind them to
        // actions so the document can insert a tab / outdent instead.
        cx.bind_keys([
            KeyBinding::new("tab", InsertTabAction, None),
            KeyBinding::new("shift-tab", OutdentAction, None),
        ]);
        let bounds = Bounds::centered(None, size(px(1180.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitleBar::title_bar_options()),
            window_min_size: Some(size(px(460.), px(420.))),
            kind: WindowKind::Normal,
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| Docxy::new(cx));
            // Hot-exit: capture the latest (possibly unsaved) content when the
            // window is closed, so a restart restores exactly what was open — no
            // save prompt.
            let on_close = view.clone();
            window.on_window_should_close(cx, move |_window, cx| {
                on_close.update(cx, |this, _| this.persist());
                true
            });
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("failed to open docxy window");
    });
}
