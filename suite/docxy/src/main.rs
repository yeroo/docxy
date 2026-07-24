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

use docxcore::editor::{Caret, Clip, Editor};
use docxcore::model::{Align, Block, Document, Inline, Paragraph, RunProps, Table, VertAlign};
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
    Styles,
    Insert,
    Review,
    View,
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
}

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

fn doc_from_path(path: &PathBuf) -> (Document, SharedString) {
    match std::fs::read(path).map(|b| docxcore::load::load(&b)) {
        Ok(Ok(doc)) => (doc, "loaded".into()),
        Ok(Err(e)) => (empty_doc(), format!("load error: {e:?}").into()),
        Err(e) => (empty_doc(), format!("read error: {e}").into()),
    }
}

fn sample_doc() -> Document {
    docxcore::load::load(include_bytes!("../../../assets/sample.docx")).unwrap_or_else(|_| empty_doc())
}

fn build_surface(kind: Kind, path: Option<&PathBuf>) -> (Surface, SharedString) {
    match kind {
        Kind::Docx => match path {
            Some(p) => {
                let (doc, status) = doc_from_path(p);
                (Surface::Doc(Editor::new(doc)), status)
            }
            None => (Surface::Doc(Editor::new(empty_doc())), "untitled".into()),
        },
        _ => (Surface::Placeholder, "".into()),
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
fn doc_to_docx(doc: &Document) -> Vec<u8> {
    let has_list = doc.body.iter().any(|b| matches!(b, Block::Paragraph(p) if p.props.num_id.is_some()));
    let pkg = if has_list { docxcore::package::new_markdown_package(doc.clone()) } else { docxcore::package::new_package(doc.clone()) };
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
            let (surface, status) = match (t.kind, &hot) {
                (Kind::Docx, Some(hp)) => {
                    let (doc, _) = doc_from_path(hp);
                    (Surface::Doc(Editor::new(doc)), if t.dirty { "unsaved — restored".into() } else { "loaded".into() })
                }
                _ => build_surface(t.kind, path.as_ref()),
            };
            tabs.push(DocTab { kind: t.kind, title: t.title.clone().into(), path, surface, dirty: t.dirty, status });
        }
        if tabs.is_empty() {
            tabs.push(DocTab {
                kind: Kind::Docx,
                title: "sample.docx".into(),
                path: None,
                surface: Surface::Doc(Editor::new(sample_doc())),
                dirty: false,
                status: "loaded".into(),
            });
        }
        let active = session.active.min(tabs.len().saturating_sub(1));
        let this = Self {
            tabs,
            active,
            focus: cx.focus_handle(),
            focused: false,
            ribbon_tab: RibbonTab::Home,
            ribbon_min: false,
            backstage: false,
            bs_new: false,
            clip: None,
            theme_pref: session.theme,
            applied: None,
            find_open: false,
            find_query: String::new(),
            replace_text: String::new(),
            find_field: FindField::Query,
            find_case: false,
            picker: None,
        };
        this.persist();
        this
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
                    std::fs::write(&p, doc_to_docx(&ed.doc)).ok().map(|_| p.display().to_string())
                } else {
                    None
                };
                PersistTab {
                    kind: t.kind,
                    title: t.title.to_string(),
                    path: t.path.as_ref().map(|p| p.display().to_string()),
                    dirty: t.dirty,
                    hot,
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
        self.tabs.push(DocTab { kind, title, path: None, surface, dirty: false, status: "new".into() });
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

    fn save_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Surface::Doc(editor) = &tab.surface else { return };
        let bytes = doc_to_docx(&editor.doc);
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
        if let Some(path) =
            rfd::FileDialog::new().add_filter("Word document", &["docx"]).set_file_name(start).save_file()
        {
            if let Some(tab) = self.tabs.get_mut(self.active) {
                tab.path = Some(path);
            }
            self.save_active(window, cx);
        } else {
            self.refocus(window, cx);
        }
    }

    fn open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = rfd::FileDialog::new().add_filter("Word document", &["docx"]).pick_file() {
            let (doc, status) = doc_from_path(&path);
            self.tabs.push(DocTab {
                kind: Kind::Docx,
                title: file_name(&path).into(),
                path: Some(path),
                surface: Surface::Doc(Editor::new(doc)),
                dirty: false,
                status,
            });
            self.active = self.tabs.len() - 1;
        }
        self.backstage = false;
        self.bs_new = false;
        self.persist();
        self.refocus(window, cx);
    }

    /// Place the caret at an explicit paragraph path + char offset (used by
    /// click-to-caret). With `extend` (Shift-click) it keeps/starts an anchor so
    /// the click extends the selection; otherwise it collapses any selection.
    fn set_caret(&mut self, path: Vec<usize>, offset: usize, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ed) = self.active_editor() {
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

    fn with_editor(&mut self, window: &mut Window, cx: &mut Context<Self>, f: impl FnOnce(&mut Editor)) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            if let Surface::Doc(ed) = &mut tab.surface {
                f(ed);
                tab.dirty = true;
            }
        }
        self.refocus(window, cx);
    }

    fn do_copy(&mut self, cut: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut dirty = false;
        if let Some(ed) = self.active_editor() {
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
        }
        row.into_any_element()
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

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        let ctrl = m.control || m.platform;
        let shift = m.shift;
        let key = ev.keystroke.key.clone();
        // Ctrl+F toggles the find bar; while it's open, all keys go to it.
        if ctrl && key == "f" {
            return self.toggle_find(window, cx);
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
                _ => {}
            }
        }
        let Some(ed) = self.active_editor() else { return };
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
                "tab" => yes(|| ed.insert_str("\t")),
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
    Cut, Copy, Paste, Undo, Redo,
    Normal, H1, H2, H3, HRule, SelectAll, Case,
    Bullets, Numbers, IndentInc, IndentDec, ClearFmt, Find, FontColor, Highlight, FontName, FontSize, Super, Sub,
    // Dialog-box launchers (open advanced dialogs — placeholder until we have a
    // dialog system).
    LaunchFont, LaunchParagraph,
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
            rs::group("Clipboard", 10, vec![rs::column(vec![
                cmdt("cut", "cut", "Cut", Cut, "Ctrl+X"),
                cmdt("copy", "copy", "Copy", Copy, "Ctrl+C"),
                cmdt("paste", "paste", "Paste", Paste, "Ctrl+V"),
            ])]),
            rs::group("Font", 40, vec![
                cmdt("fontname", "font-name", "Font", FontName, "").toggle(),
                cmdt("fontsize", "font-size", "Font size", FontSize, "").toggle(),
                Control::Separator,
                cmdt("b", "bold", "Bold", Bold, "Ctrl+B").toggle(),
                cmdt("i", "italic", "Italic", Italic, "Ctrl+I").toggle(),
                cmdt("u", "underline", "Underline", Underline, "Ctrl+U").toggle(),
                cmdt("s", "strikethrough", "Strikethrough", Strike, "").toggle(),
                cmdt("sub", "subscript", "Subscript", Sub, "").toggle(),
                cmdt("sup", "superscript", "Superscript", Super, "").toggle(),
                Control::Separator,
                cmdt("color", "text-color", "Font colour", FontColor, "").toggle(),
                cmdt("hl", "highlight", "Text highlight", Highlight, "").toggle(),
                Control::Separator,
                cmdt("grow", "font-increase", "Grow font", Grow, "").toggle(),
                cmdt("shrink", "font-decrease", "Shrink font", Shrink, "").toggle(),
                Control::Separator,
                cmdt("clearfmt", "clear-format", "Clear formatting", ClearFmt, "").toggle(),
            ])
            .launcher(LaunchFont),
            rs::group("Paragraph", 30, vec![
                cmdt("bullets", "list-bullet", "Bullets", Bullets, "").toggle(),
                cmdt("numbers", "list-numbered", "Numbering", Numbers, "").toggle(),
                cmdt("inddec", "indent-decrease", "Decrease indent", IndentDec, "Ctrl+Shift+M").toggle(),
                cmdt("indinc", "indent-increase", "Increase indent", IndentInc, "Ctrl+M").toggle(),
                Control::Separator,
                cmdt("al", "align-left", "Align left", AlignL, "").toggle(),
                cmdt("ac", "align-center", "Center", AlignC, "").toggle(),
                cmdt("ar", "align-right", "Align right", AlignR, "").toggle(),
                cmdt("aj", "align-justify", "Justify", AlignJ, "").toggle(),
            ])
            .launcher(LaunchParagraph),
            rs::group("Editing", 20, vec![
                cmdt("undo", "undo", "Undo", Undo, "Ctrl+Z").toggle(),
                cmdt("redo", "redo", "Redo", Redo, "Ctrl+Y").toggle(),
            ]),
        ]),
        rs::tab("Styles", "S", vec![rs::group("Styles", 40, vec![rs::column(vec![
            cmdt("normal", "paragraph", "Normal", Normal, ""),
            cmdt("h1", "heading-1", "Heading 1", H1, ""),
            cmdt("h2", "heading-2", "Heading 2", H2, ""),
            cmdt("h3", "heading-3", "Heading 3", H3, ""),
        ])])]),
        rs::tab("Insert", "N", vec![rs::group("Symbols", 40, vec![rs::column(vec![
            cmdt("hr", "rule", "Horizontal rule", HRule, ""),
        ])])]),
        rs::tab("Review", "R", vec![rs::group("Editing", 40, vec![rs::column(vec![
            cmdt("find", "find", "Find & Replace", Find, "Ctrl+F"),
            cmdt("selall", "select-all", "Select all", SelectAll, "Ctrl+A"),
            cmdt("case", "case", "Change case", Case, ""),
        ])])]),
        rs::tab("View", "W", vec![]),
    ])
}

fn ribbon_tab_index(t: RibbonTab) -> usize {
    match t {
        RibbonTab::Home => 0,
        RibbonTab::Styles => 1,
        RibbonTab::Insert => 2,
        RibbonTab::Review => 3,
        RibbonTab::View => 4,
    }
}

/// Rough natural width (px) of a group, for responsive collapse decisions.
fn group_est(g: &rs::Group<Act>, icon_only: bool) -> f32 {
    let mut w: f32 = 22.0;
    for c in &g.items {
        w += match c {
            Control::Toggle(_) => 30.0,
            Control::Large(_) => {
                if icon_only {
                    32.0
                } else {
                    74.0
                }
            }
            Control::Column(_) => {
                if icon_only {
                    34.0
                } else {
                    104.0
                }
            }
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

fn caret_bar() -> AnyElement {
    div().w(px(2.)).h(px(19.)).bg(rgb(BRAND)).into_any_element()
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
        out.push(
            div()
                .child(SharedString::from(word.to_string()))
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
                // Click-to-caret: place the caret at this word's start offset.
                .when_some(click, |d, c| {
                    let ent = c.ent.clone();
                    let path = c.path.to_vec();
                    d.cursor_text().on_mouse_down(MouseButton::Left, move |ev, window, cx| {
                        cx.stop_propagation();
                        let extend = ev.modifiers.shift;
                        ent.update(cx, |this, cx| this.set_caret(path.clone(), word_off, extend, window, cx));
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

/// A tab is ONE char in the engine but rendered as spaces; keep `idx` in sync and
/// let it participate in caret/selection like any other char.
fn emit_tab(out: &mut Vec<AnyElement>, idx: &mut usize, caret: &mut Option<usize>, sel: Option<(usize, usize)>, click: Option<Click>, pal: Pal) {
    let pos = *idx;
    if *caret == Some(pos) {
        out.push(caret_bar());
        *caret = None;
    }
    let selected = sel.map_or(false, |(s, e)| s < e && s <= pos && pos < e);
    out.push(
        div()
            .child(SharedString::from("\u{2003}\u{2003}"))
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

fn paragraph_el(p: &Paragraph, mut caret: Option<usize>, sel: Option<(usize, usize)>, marker: Option<&str>, click: Option<Click>, pal: Pal) -> AnyElement {
    let base = match p.props.heading_level {
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
    if let Some(m) = marker {
        // The marker isn't document content — render it plain (non-clickable) so it
        // never maps clicks to bogus offsets.
        spans.push(div().text_size(px(base)).text_color(pal.dim).child(SharedString::from(m.to_string())).into_any_element());
    }
    for inline in &p.content {
        match inline {
            Inline::Run(r) => emit_run(&mut spans, &r.text, &r.props, base, false, &mut idx, &mut caret, sel, click, pal),
            Inline::Hyperlink(h) => {
                for r in &h.runs {
                    emit_run(&mut spans, &r.text, &r.props, base, true, &mut idx, &mut caret, sel, click, pal);
                }
            }
            Inline::Tab(_) => emit_tab(&mut spans, &mut idx, &mut caret, sel, click, pal),
            Inline::Break(_) => emit_break(&mut spans, &mut idx, &mut caret),
            other => {
                let tag = match other {
                    Inline::SmartArt { .. } => "[diagram]",
                    Inline::Chart { .. } => "[chart]",
                    Inline::Equation { .. } => "[equation]",
                    Inline::TextBox { .. } => "[textbox]",
                    Inline::Field { .. } => "[field]",
                    _ => "[image]",
                };
                spans.push(div().px_1().rounded_sm().bg(pal.panel).text_size(px(12.)).text_color(pal.dim).child(tag).into_any_element());
            }
        }
    }
    if caret.is_some() {
        spans.push(caret_bar());
    }
    let mut row = h_flex().w_full().flex_wrap().min_h(px(base + 6.));
    row = match p.props.align {
        Align::Center => row.justify_center(),
        Align::Right => row.justify_end(),
        _ => row,
    };
    // Leading indent: explicit paragraph indent (twips → px at ~96dpi) plus a step
    // per list nesting level.
    let pad = (p.props.indent.max(0) as f32) / 15.0 + p.props.ilvl.max(0) as f32 * 20.0;
    // The whole paragraph area is a click fallback (empty space past the text, the
    // indent gutter) that drops the caret at the paragraph end. Word clicks fire
    // first and stop propagation, so this only runs on a "past the text" click.
    let para_end = idx;
    v_flex()
        .w_full()
        .py_0p5()
        .pl(px(pad))
        .when(is_heading, |d| d.mt_2())
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
            let caret = (ctx.caret_path == path.as_slice()).then_some(ctx.caret_off);
            let sel = ctx.spans.iter().find(|(pp, _, _)| pp.as_slice() == path.as_slice()).map(|(_, s, e)| (*s, *e));
            let click = Some(Click { ent: ctx.ent, path: &path });
            paragraph_el(p, caret, sel, marker, click, ctx.pal)
        }
        Block::Table(t) => table_el(t, &path, ctx),
        Block::Raw(_) => div().h(px(0.)).into_any_element(),
    }
}

// ---- chrome: ribbon + backstage --------------------------------------------

impl Docxy {
    fn ribbon_tabs(&self, fg: Hsla, dim: Hsla, panel: Hsla, cx: &mut Context<Self>) -> AnyElement {
        let names = ["File", "Home", "Styles", "Insert", "Review", "View"];
        let mut strip = h_flex().w_full().items_end().gap_1().px_2().pt_1().bg(panel);
        for (i, name) in names.iter().enumerate() {
            let is_file = i == 0;
            let this_tab = match i {
                1 => Some(RibbonTab::Home),
                2 => Some(RibbonTab::Styles),
                3 => Some(RibbonTab::Insert),
                4 => Some(RibbonTab::Review),
                5 => Some(RibbonTab::View),
                _ => None,
            };
            let active = !self.backstage && this_tab == Some(self.ribbon_tab);
            strip = strip.child(
                div()
                    .id(("rtab", i))
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
                Undo => {
                    e.undo();
                }
                Redo => {
                    e.redo();
                }
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
                ClearFmt => e.clear_run_formatting(),
                Cut | Copy | Paste | LaunchFont | LaunchParagraph | Find | FontColor | Highlight | FontName | FontSize => {}
            }),
        }
    }

    /// The ribbon body for the active tab, rendered from the shared ribbonspec
    /// model with Fluent icons — and RESPONSIVE: as `width` drops, control labels
    /// are dropped first, then the lowest-`priority` groups collapse into an
    /// overflow indicator (Office-style scaling driven by ribbonspec::priority).
    fn ribbon_body(&self, width: f32, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let ribbon = docxy_ribbon();
        let tab = &ribbon.tabs[ribbon_tab_index(self.ribbon_tab)];
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
            Control::Large(cmd) => self.icon_btn(cmd, !icon_only, pal, cx),
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
            Control::Separator => div().w(px(1.)).h(px(44.)).bg(pal.border).mx_1().into_any_element(),
            _ => div().into_any_element(),
        }
    }

    fn icon_btn(&self, cmd: &rs::Cmd<Act>, show_label: bool, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let act = cmd.act;
        let tip = cmd.tip;
        let tip_text: SharedString = if tip.shortcut.is_empty() {
            tip.title.into()
        } else {
            format!("{}  \u{00b7}  {}", tip.title, tip.shortcut).into()
        };
        div()
            .id(cmd.id)
            .flex()
            .items_center()
            .gap_1p5()
            .px_2()
            .h(px(22.))
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|d| d.bg(pal.hover))
            .active(|d| d.bg(Hsla { a: 0.22, ..pal.fg }))
            .child(icon_svg(cmd.icon.0, 16., pal.fg))
            .when(show_label, |d| d.child(div().text_size(px(12.)).text_color(pal.fg).child(SharedString::from(cmd.label))))
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
        let vw = f32::from(window.viewport_size().width);
        let ribbon_tabs = self.ribbon_tabs(fg, dim, panel, cx);
        let ribbon_body = (is_doc && !self.ribbon_min).then(|| self.ribbon_body(vw, pal, cx));
        let find_bar = (is_doc && self.find_open).then(|| self.find_bar(pal, cx));
        let picker_bar = (is_doc).then_some(self.picker).flatten().map(|k| self.picker_bar(k, pal, cx));

        let content: AnyElement = match self.tabs.get(self.active) {
            Some(tab) => match &tab.surface {
                Surface::Doc(editor) => {
                    let spans = editor.selection_spans();
                    let markers = list_markers(&editor.doc.body);
                    let ent = cx.entity();
                    let ctx = RenderCtx { caret_path: &editor.caret.path, caret_off: editor.caret.offset, spans: &spans, ent: &ent, pal };
                    let blocks: Vec<AnyElement> = editor
                        .doc
                        .body
                        .iter()
                        .enumerate()
                        .map(|(i, b)| block_el(b, vec![i], markers[i].as_deref(), ctx))
                        .collect();
                    v_flex().id("doc-scroll").flex_1().overflow_y_scroll().bg(bg).text_color(fg).px(px(48.)).py(px(28.)).gap_1().children(blocks).into_any_element()
                }
                Surface::Placeholder => placeholder(tab.kind, bg, dim).into_any_element(),
            },
            None => v_flex().flex_1().bg(bg).items_center().justify_center().text_color(dim).child("No documents — File \u{203A} New").into_any_element(),
        };

        let status = h_flex()
            .w_full()
            .px_4()
            .py_1()
            .bg(panel)
            .text_size(px(11.))
            .text_color(dim)
            .child(self.tabs.get(self.active).map(|t| t.status.clone()).unwrap_or_default())
            .child(div().flex_1())
            .child("type · Ctrl+B/I/U · Ctrl+F find · Ctrl+C/X/V · Ctrl+Z/Y · Ctrl+S");

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .bg(bg)
            .child(title_bar)
            .child(ribbon_tabs)
            .when_some(ribbon_body, |d, r| d.child(r))
            .when_some(find_bar, |d, f| d.child(f))
            .when_some(picker_bar, |d, p| d.child(p))
            .child(content)
            .child(status)
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
