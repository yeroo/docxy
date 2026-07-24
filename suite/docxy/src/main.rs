//! docxy — the doc-centric desktop suite (docs / sheets / mail in tabs), on GPUI.
//!
//! Custom title bar (min/max/close), a tab strip, and per-tab surfaces. Session
//! hot-exit: which tabs and which files are open is written to
//! `<config>/docxy/session.json` and restored on launch; closing never prompts.
//!
//! The docx surface is a THIN view over `docxcore::editor::Editor` — the exact
//! lossless engine the terminal docxy uses. Phase 1 rendered it; Phase 2 makes it
//! editable: a focusable view with a caret, routing keys into the engine's ops.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use docxcore::editor::{Caret, Editor};
use docxcore::model::{Align, Block, Document, Inline, Paragraph, RunProps, Table};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    Root, Sizable, TitleBar,
    button::{Button, ButtonVariants},
    h_flex,
    tab::{Tab, TabBar},
    v_flex,
};
use serde::{Deserialize, Serialize};

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

#[derive(Serialize, Deserialize)]
struct PersistTab {
    kind: Kind,
    title: String,
    path: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct Session {
    tabs: Vec<PersistTab>,
    active: usize,
}

fn session_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("docxy")
        .join("session.json")
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
}

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

impl Docxy {
    fn new(cx: &mut Context<Self>) -> Self {
        let session: Session = std::fs::read(session_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();

        let mut tabs = Vec::new();
        for t in &session.tabs {
            let path = t.path.as_ref().map(PathBuf::from);
            let (surface, status) = build_surface(t.kind, path.as_ref());
            tabs.push(DocTab {
                kind: t.kind,
                title: t.title.clone().into(),
                path,
                surface,
                dirty: false,
                status,
            });
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
        let this = Self { tabs, active, focus: cx.focus_handle(), focused: false };
        this.persist();
        this
    }

    fn persist(&self) {
        let tabs = self
            .tabs
            .iter()
            .map(|t| PersistTab {
                kind: t.kind,
                title: t.title.to_string(),
                path: t.path.as_ref().map(|p| p.display().to_string()),
            })
            .collect();
        let session = Session { tabs, active: self.active };
        if let Ok(json) = serde_json::to_string_pretty(&session) {
            let p = session_path();
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, json);
        }
    }

    fn add_tab(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let (title, surface): (SharedString, Surface) = match kind {
            Kind::Docx => ("Untitled.docx".into(), Surface::Doc(Editor::new(empty_doc()))),
            Kind::Xlsx => ("Untitled.xlsx".into(), Surface::Placeholder),
            Kind::Look => ("Inbox".into(), Surface::Placeholder),
        };
        self.tabs.push(DocTab { kind, title, path: None, surface, dirty: false, status: "new".into() });
        self.active = self.tabs.len() - 1;
        self.persist();
        cx.notify();
    }

    fn select_tab(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.tabs.len() {
            self.active = i;
            self.persist();
            cx.notify();
        }
    }

    fn close_tab(&mut self, i: usize, cx: &mut Context<Self>) {
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
        cx.notify();
    }

    fn save_active(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Surface::Doc(editor) = &tab.surface else { return };
        let pkg = docxcore::package::new_package(editor.doc.clone());
        let bytes = docxcore::package::save_package(&pkg);
        let path = tab
            .path
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default().join(tab.title.to_string()));
        match std::fs::write(&path, &bytes) {
            Ok(()) => {
                tab.path = Some(path.clone());
                tab.dirty = false;
                tab.status = format!("saved {} bytes → {}", bytes.len(), path.display()).into();
            }
            Err(e) => tab.status = format!("save failed: {e}").into(),
        }
        self.persist();
        cx.notify();
    }

    /// Route a keystroke into the active doc's editor engine.
    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        let ctrl = m.control || m.platform;
        // Ctrl+S saves (needs &mut self; handle before borrowing the editor).
        if ctrl && ev.keystroke.key == "s" {
            self.save_active(cx);
            return;
        }
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Surface::Doc(ed) = &mut tab.surface else { return };
        let key = ev.keystroke.key.as_str();
        let shift = m.shift;

        let changed = if ctrl {
            match key {
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
                "a" => {
                    ed.select_all();
                    false
                }
                _ => false,
            }
        } else {
            ed.extend_selection(shift);
            match key {
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
            tab.dirty = true;
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

/// Crude vertical movement: jump to the nearest adjacent top-level paragraph,
/// keeping the column. (True visual up/down needs a layout line-map — later.)
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

// ---- rich rendering of the docx model --------------------------------------

const BG: u32 = 0x1e1e1e;
const PANEL: u32 = 0x252526;
const FG: u32 = 0xd4d4d4;
const DIM: u32 = 0x858585;
const ACCENT: u32 = 0x4ec9b0;
const LINK: u32 = 0x4ea1f4;
const CARET: u32 = 0x4ec9b0;

fn hex_rgb(s: &str) -> Option<u32> {
    let s = s.trim_start_matches('#');
    (s.len() == 6).then(|| u32::from_str_radix(s, 16).ok()).flatten()
}

fn split_at_char(s: &str, n: usize) -> (&str, &str) {
    let idx = s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len());
    s.split_at(idx)
}

fn caret_bar() -> AnyElement {
    div().w(px(2.)).h(px(19.)).bg(rgb(CARET)).into_any_element()
}

fn emit_words(out: &mut Vec<AnyElement>, text: &str, props: &RunProps, base: f32, is_link: bool) {
    for word in text.split_inclusive(' ') {
        if word.is_empty() {
            continue;
        }
        let size = props.size_half_pts.map(|h| h as f32 / 2.0 * 1.333).unwrap_or(base);
        let color = if is_link { LINK } else { props.color.as_deref().and_then(hex_rgb).unwrap_or(FG) };
        out.push(
            div()
                .child(SharedString::from(word.to_string()))
                .text_size(px(size))
                .text_color(rgb(color))
                .when(props.bold, |d| d.font_weight(FontWeight::BOLD))
                .when(props.italic, |d| d.italic())
                .when(props.underline || is_link, |d| d.underline())
                .when(props.strike, |d| d.line_through())
                .when(props.highlight.is_some(), |d| d.bg(rgb(0x5b5b2a)))
                .into_any_element(),
        );
    }
}

/// Emit `text`, inserting the caret bar if `caret` (a paragraph char offset)
/// falls within this run. `idx` tracks the running char offset; `caret` is set to
/// None once consumed so it's drawn exactly once.
fn emit_run(
    out: &mut Vec<AnyElement>,
    text: &str,
    props: &RunProps,
    base: f32,
    is_link: bool,
    idx: &mut usize,
    caret: &mut Option<usize>,
) {
    let len = text.chars().count();
    if let Some(off) = *caret {
        if off >= *idx && off <= *idx + len {
            let (a, b) = split_at_char(text, off - *idx);
            emit_words(out, a, props, base, is_link);
            out.push(caret_bar());
            emit_words(out, b, props, base, is_link);
            *caret = None;
            *idx += len;
            return;
        }
    }
    emit_words(out, text, props, base, is_link);
    *idx += len;
}

fn paragraph_el(p: &Paragraph, mut caret: Option<usize>) -> AnyElement {
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
    if p.props.num_id.is_some() {
        let marker = if p.props.ilvl % 2 == 1 { "\u{25E6} " } else { "\u{2022} " };
        emit_words(&mut spans, marker, &RunProps::default(), base, false);
    }
    for inline in &p.content {
        match inline {
            Inline::Run(r) => emit_run(&mut spans, &r.text, &r.props, base, false, &mut idx, &mut caret),
            Inline::Hyperlink(h) => {
                for r in &h.runs {
                    emit_run(&mut spans, &r.text, &r.props, base, true, &mut idx, &mut caret);
                }
            }
            Inline::Tab(_) => emit_run(&mut spans, "    ", &RunProps::default(), base, false, &mut idx, &mut caret),
            Inline::Break(_) => spans.push(div().w_full().h(px(0.)).into_any_element()),
            other => {
                let tag = match other {
                    Inline::SmartArt { .. } => "[diagram]",
                    Inline::Chart { .. } => "[chart]",
                    Inline::Equation { .. } => "[equation]",
                    Inline::TextBox { .. } => "[textbox]",
                    Inline::Field { .. } => "[field]",
                    _ => "[image]",
                };
                spans.push(
                    div().px_1().rounded_sm().bg(rgb(PANEL)).text_size(px(12.)).text_color(rgb(DIM)).child(tag).into_any_element(),
                );
            }
        }
    }
    // caret at end of paragraph (or empty paragraph)
    if caret.is_some() {
        spans.push(caret_bar());
    }

    let mut row = h_flex().w_full().flex_wrap().min_h(px(base + 6.));
    row = match p.props.align {
        Align::Center => row.justify_center(),
        Align::Right => row.justify_end(),
        _ => row,
    };
    let row = row.children(spans);

    v_flex().w_full().py_0p5().when(is_heading, |d| d.mt_2()).child(row).into_any_element()
}

fn table_el(t: &Table) -> AnyElement {
    let mut rows = Vec::new();
    for row in &t.rows {
        let mut cells = Vec::new();
        for cell in &row.cells {
            let inner: Vec<AnyElement> = cell.blocks.iter().map(|b| block_el(b, None)).collect();
            cells.push(
                v_flex().flex_1().px_2().py_1().border_1().border_color(rgb(0x3a3a3a)).children(inner).into_any_element(),
            );
        }
        rows.push(h_flex().w_full().children(cells).into_any_element());
    }
    v_flex().w_full().my_2().children(rows).into_any_element()
}

fn block_el(b: &Block, caret: Option<usize>) -> AnyElement {
    match b {
        Block::Paragraph(p) => paragraph_el(p, caret),
        Block::Table(t) => table_el(t),
        Block::Raw(_) => div().h(px(0.)).into_any_element(),
    }
}

impl Render for Docxy {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused {
            self.focus.focus(window, cx);
            self.focused = true;
        }

        let new_btn = |id: &'static str, label: &'static str, kind: Kind| {
            Button::new(id).small().ghost().label(label).on_click(cx.listener(move |this, _, _, cx| this.add_tab(kind, cx)))
        };
        let can_save = matches!(self.tabs.get(self.active).map(|t| &t.surface), Some(Surface::Doc(_)));

        let title_bar = TitleBar::new().child(
            h_flex()
                .items_center()
                .gap_2()
                .pl_2()
                .child(div().font_weight(FontWeight::BOLD).text_color(rgb(ACCENT)).child("docxy"))
                .child(new_btn("new-doc", "+ Doc", Kind::Docx))
                .child(new_btn("new-sheet", "+ Sheet", Kind::Xlsx))
                .child(new_btn("new-mail", "+ Mail", Kind::Look))
                .when(can_save, |d| {
                    d.child(
                        Button::new("save").small().primary().label("Save .docx").on_click(cx.listener(|this, _, _, cx| this.save_active(cx))),
                    )
                }),
        );

        let tabs = self.tabs.iter().enumerate().map(|(i, t)| {
            let mark = if t.dirty { " \u{2022}" } else { "" };
            let label = format!("{} {}{}", t.kind.glyph(), t.title, mark);
            Tab::new().child(label).suffix(
                Button::new(("close", i)).xsmall().ghost().label("\u{00d7}").on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.close_tab(i, cx);
                })),
            )
        });
        let tab_bar = TabBar::new("docxy-tabs")
            .w_full()
            .selected_index(self.active)
            .children(tabs)
            .on_click(cx.listener(|this, ix: &usize, _, cx| this.select_tab(*ix, cx)));

        let content: AnyElement = match self.tabs.get(self.active) {
            Some(t) => match &t.surface {
                Surface::Doc(editor) => {
                    let caret_block = if editor.caret.path.len() == 1 { Some(editor.caret.path[0]) } else { None };
                    let caret_off = editor.caret.offset;
                    let blocks: Vec<AnyElement> = editor
                        .doc
                        .body
                        .iter()
                        .enumerate()
                        .map(|(i, b)| block_el(b, (Some(i) == caret_block).then_some(caret_off)))
                        .collect();
                    v_flex()
                        .id("doc-scroll")
                        .flex_1()
                        .overflow_y_scroll()
                        .bg(rgb(BG))
                        .text_color(rgb(FG))
                        .px(px(48.))
                        .py(px(28.))
                        .gap_1()
                        .children(blocks)
                        .into_any_element()
                }
                Surface::Placeholder => placeholder(t.kind).into_any_element(),
            },
            None => v_flex().flex_1().bg(rgb(BG)).items_center().justify_center().text_color(rgb(DIM)).child("No documents — use + Doc / + Sheet / + Mail").into_any_element(),
        };

        let status = h_flex()
            .w_full()
            .px_4()
            .py_1()
            .bg(rgb(PANEL))
            .text_size(px(11.))
            .text_color(rgb(DIM))
            .child(self.tabs.get(self.active).map(|t| t.status.clone()).unwrap_or_default())
            .child(div().flex_1())
            .child("Phase 2 · editable — type, ⌫/⏎, ←→ Home/End, Ctrl+B/I/U, Ctrl+Z/Y, Ctrl+S");

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .bg(rgb(BG))
            .child(title_bar)
            .child(tab_bar)
            .child(content)
            .child(status)
    }
}

fn placeholder(kind: Kind) -> impl IntoElement {
    let (name, blurb) = match kind {
        Kind::Xlsx => ("xlsxy", "the spreadsheet grid (gridcore) lands here next"),
        Kind::Look => ("lookxy", "mail list + reading pane (mailcore) lands here next"),
        Kind::Docx => ("docxy", ""),
    };
    v_flex()
        .flex_1()
        .bg(rgb(BG))
        .items_center()
        .justify_center()
        .gap_2()
        .child(div().text_color(rgb(ACCENT)).font_weight(FontWeight::BOLD).text_size(px(20.)).child(name))
        .child(div().text_color(rgb(DIM)).child(blurb))
}

fn main() {
    gpui_platform::application()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx: &mut App| {
            gpui_component::init(cx);
            let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitleBar::title_bar_options()),
                window_min_size: Some(size(px(640.), px(400.))),
                kind: WindowKind::Normal,
                ..Default::default()
            };
            cx.open_window(options, |window, cx| {
                let view = cx.new(|cx| Docxy::new(cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open docxy window");
        });
}
