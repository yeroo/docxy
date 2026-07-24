//! docxy — the doc-centric desktop suite (docs / sheets / mail in tabs), on GPUI.
//!
//! Custom title bar (min/max/close), a tab strip, and per-tab surfaces. Session
//! hot-exit: which tabs and which files are open is written to
//! `<config>/docxy/session.json` and restored on launch; closing never prompts.
//!
//! The docx surface is a THIN view over `docxcore::editor::Editor` — the exact
//! lossless engine the terminal docxy uses — so it renders the real document with
//! its formatting (Phase 1: faithful rendering + lossless save; interactive
//! editing is the next phase).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use docxcore::editor::Editor;
use docxcore::model::{Align, Block, Document, Inline, Paragraph, Run, RunProps, Table};
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
    status: SharedString,
}

struct Docxy {
    tabs: Vec<DocTab>,
    active: usize,
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
    fn new() -> Self {
        let session: Session = std::fs::read(session_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();

        let mut tabs = Vec::new();
        for t in &session.tabs {
            let path = t.path.as_ref().map(PathBuf::from);
            let (surface, status) = build_surface(t.kind, path.as_ref());
            tabs.push(DocTab { kind: t.kind, title: t.title.clone().into(), path, surface, status });
        }
        if tabs.is_empty() {
            tabs.push(DocTab {
                kind: Kind::Docx,
                title: "sample.docx".into(),
                path: None,
                surface: Surface::Doc(Editor::new(sample_doc())),
                status: "loaded".into(),
            });
        }
        let active = session.active.min(tabs.len().saturating_sub(1));
        let this = Self { tabs, active };
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
        self.tabs.push(DocTab { kind, title, path: None, surface, status: "new".into() });
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
        tab.status = match std::fs::write(&path, &bytes) {
            Ok(()) => {
                tab.path = Some(path.clone());
                format!("saved {} bytes → {}", bytes.len(), path.display()).into()
            }
            Err(e) => format!("save failed: {e}").into(),
        };
        self.persist();
        cx.notify();
    }
}

// ---- rich rendering of the docx model --------------------------------------

const BG: u32 = 0x1e1e1e;
const PANEL: u32 = 0x252526;
const FG: u32 = 0xd4d4d4;
const DIM: u32 = 0x858585;
const ACCENT: u32 = 0x4ec9b0;
const LINK: u32 = 0x4ea1f4;

fn hex_rgb(s: &str) -> Option<u32> {
    let s = s.trim_start_matches('#');
    (s.len() == 6).then(|| u32::from_str_radix(s, 16).ok()).flatten()
}

/// One styled word (kept inclusive of its trailing space) as a span.
fn word_span(word: &str, props: &RunProps, base_px: f32, is_link: bool) -> AnyElement {
    let size = props.size_half_pts.map(|h| h as f32 / 2.0 * 1.333).unwrap_or(base_px);
    let color = if is_link {
        LINK
    } else {
        props.color.as_deref().and_then(hex_rgb).unwrap_or(FG)
    };
    div()
        .child(SharedString::from(word.to_string()))
        .text_size(px(size))
        .text_color(rgb(color))
        .when(props.bold, |d| d.font_weight(FontWeight::BOLD))
        .when(props.italic, |d| d.italic())
        .when(props.underline || is_link, |d| d.underline())
        .when(props.strike, |d| d.line_through())
        .when(props.highlight.is_some(), |d| d.bg(rgb(0x5b5b2a)))
        .into_any_element()
}

fn run_spans(runs: impl Iterator<Item = (String, RunProps)>, base_px: f32, is_link: bool) -> Vec<AnyElement> {
    let mut out = Vec::new();
    for (text, props) in runs {
        if text.is_empty() {
            continue;
        }
        for word in text.split_inclusive(' ') {
            out.push(word_span(word, &props, base_px, is_link));
        }
    }
    out
}

fn paragraph_el(p: &Paragraph) -> AnyElement {
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
    if p.props.num_id.is_some() {
        let marker = if p.props.ilvl % 2 == 1 { "\u{25E6} " } else { "\u{2022} " };
        spans.push(word_span(marker, &RunProps::default(), base, false));
    }
    for inline in &p.content {
        match inline {
            Inline::Run(r) => {
                spans.extend(run_spans(std::iter::once((r.text.clone(), r.props.clone())), base, false));
            }
            Inline::Hyperlink(h) => {
                let runs = h.runs.iter().map(|r: &Run| (r.text.clone(), r.props.clone()));
                spans.extend(run_spans(runs, base, true));
            }
            Inline::Tab(_) => spans.push(word_span("    ", &RunProps::default(), base, false)),
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
                    div()
                        .px_1()
                        .rounded_sm()
                        .bg(rgb(PANEL))
                        .text_size(px(12.))
                        .text_color(rgb(DIM))
                        .child(tag)
                        .into_any_element(),
                );
            }
        }
    }

    let mut row = h_flex().w_full().flex_wrap();
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
            let inner: Vec<AnyElement> = cell.blocks.iter().map(block_el).collect();
            cells.push(
                v_flex()
                    .flex_1()
                    .px_2()
                    .py_1()
                    .border_1()
                    .border_color(rgb(0x3a3a3a))
                    .children(inner)
                    .into_any_element(),
            );
        }
        rows.push(h_flex().w_full().children(cells).into_any_element());
    }
    v_flex().w_full().my_2().children(rows).into_any_element()
}

fn block_el(b: &Block) -> AnyElement {
    match b {
        Block::Paragraph(p) => paragraph_el(p),
        Block::Table(t) => table_el(t),
        Block::Raw(_) => div().h(px(0.)).into_any_element(),
    }
}

impl Render for Docxy {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let new_btn = |id: &'static str, label: &'static str, kind: Kind| {
            Button::new(id)
                .small()
                .ghost()
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| this.add_tab(kind, cx)))
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
                        Button::new("save")
                            .small()
                            .primary()
                            .label("Save .docx")
                            .on_click(cx.listener(|this, _, _, cx| this.save_active(cx))),
                    )
                }),
        );

        let tabs = self.tabs.iter().enumerate().map(|(i, t)| {
            let label = format!("{} {}", t.kind.glyph(), t.title);
            Tab::new().child(label).suffix(
                Button::new(("close", i)).xsmall().ghost().label("\u{00d7}").on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.close_tab(i, cx);
                    },
                )),
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
                    let blocks: Vec<AnyElement> = editor.doc.body.iter().map(block_el).collect();
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
            None => v_flex()
                .flex_1()
                .bg(rgb(BG))
                .items_center()
                .justify_center()
                .text_color(rgb(DIM))
                .child("No documents — use + Doc / + Sheet / + Mail")
                .into_any_element(),
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
            .child("Phase 1 · faithful render (read-only)");

        v_flex().size_full().bg(rgb(BG)).child(title_bar).child(tab_bar).child(content).child(status)
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
                let view = cx.new(|_| Docxy::new());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open docxy window");
        });
}
