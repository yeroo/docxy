//! docxy — the doc-centric desktop suite (docs / sheets / mail in tabs), on GPUI.
//!
//! A thin GPUI view over `docxcore::editor::Editor` — the lossless engine the
//! terminal docxy uses. Custom title bar (min/max/close), an Office-style ribbon
//! (File backstage + Home/Styles/Insert/Review/View tabs of titled command
//! groups), a document tab strip, and a rich editable document surface. Session
//! hot-exit: open tabs/files persist to `<config>/docxy/session.json`.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use docxcore::editor::{Caret, Clip, Editor};
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
    backstage: bool,
    clip: Option<Clip>,
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

fn file_name(path: &PathBuf) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Untitled.docx".into())
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
            tabs.push(DocTab { kind: t.kind, title: t.title.clone().into(), path, surface, dirty: false, status });
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
            backstage: false,
            clip: None,
        };
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

    fn refocus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        cx.notify();
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
        let bytes = docxcore::package::save_package(&docxcore::package::new_package(editor.doc.clone()));
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
        self.persist();
        self.refocus(window, cx);
    }

    /// Run an editor op on the active doc from a ribbon click, then refocus.
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

    /// Route a keystroke into the active doc's editor engine.
    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let m = &ev.keystroke.modifiers;
        let ctrl = m.control || m.platform;
        let key = ev.keystroke.key.clone();
        if ctrl {
            match key.as_str() {
                "s" => return self.save_active(window, cx),
                "c" => return self.do_copy(false, window, cx),
                "x" => return self.do_copy(true, window, cx),
                "v" => return self.do_paste(window, cx),
                _ => {}
            }
        }
        let shift = m.shift;
        let Some(ed) = self.active_editor() else { return };
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
                _ => false,
            }
        } else {
            ed.extend_selection(shift);
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

/// A ribbon button that runs an editor op on the active doc.
fn rbtn(cx: &mut Context<Docxy>, id: &'static str, label: &'static str, op: impl Fn(&mut Editor) + 'static) -> Button {
    Button::new(id).ghost().xsmall().label(label).on_click(cx.listener(move |this, _, window, cx| this.with_editor(window, cx, |e| op(e))))
}

/// A ribbon button that runs an app-level action.
fn abtn(
    cx: &mut Context<Docxy>,
    id: &'static str,
    label: &'static str,
    f: impl Fn(&mut Docxy, &mut Window, &mut Context<Docxy>) + 'static,
) -> Button {
    Button::new(id).ghost().xsmall().label(label).on_click(cx.listener(move |this, _, window, cx| f(this, window, cx)))
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

// ---- palette + doc rendering -----------------------------------------------

const BG: u32 = 0x1e1e1e;
const PANEL: u32 = 0x252526;
const RIBBON: u32 = 0x2d2d30;
const RAIL: u32 = 0x333337;
const FG: u32 = 0xd4d4d4;
const DIM: u32 = 0x858585;
const DIV: u32 = 0x3a3a3a;
const ACCENT: u32 = 0x4ec9b0;
const LINK: u32 = 0x4ea1f4;

fn hex_rgb(s: &str) -> Option<u32> {
    let s = s.trim_start_matches('#');
    (s.len() == 6).then(|| u32::from_str_radix(s, 16).ok()).flatten()
}

fn split_at_char(s: &str, n: usize) -> (&str, &str) {
    let idx = s.char_indices().nth(n).map(|(i, _)| i).unwrap_or(s.len());
    s.split_at(idx)
}

fn caret_bar() -> AnyElement {
    div().w(px(2.)).h(px(19.)).bg(rgb(ACCENT)).into_any_element()
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

fn emit_run(out: &mut Vec<AnyElement>, text: &str, props: &RunProps, base: f32, is_link: bool, idx: &mut usize, caret: &mut Option<usize>) {
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
                spans.push(div().px_1().rounded_sm().bg(rgb(PANEL)).text_size(px(12.)).text_color(rgb(DIM)).child(tag).into_any_element());
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
    v_flex().w_full().py_0p5().when(is_heading, |d| d.mt_2()).child(row.children(spans)).into_any_element()
}

fn table_el(t: &Table) -> AnyElement {
    let mut rows = Vec::new();
    for row in &t.rows {
        let mut cells = Vec::new();
        for cell in &row.cells {
            let inner: Vec<AnyElement> = cell.blocks.iter().map(|b| block_el(b, None)).collect();
            cells.push(v_flex().flex_1().px_2().py_1().border_1().border_color(rgb(DIV)).children(inner).into_any_element());
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

// ---- ribbon + backstage rendering ------------------------------------------

fn group_box(title: &'static str, buttons: Vec<Button>) -> AnyElement {
    v_flex()
        .items_center()
        .justify_between()
        .h_full()
        .px_2()
        .gap_1()
        .border_r_1()
        .border_color(rgb(DIV))
        .child(h_flex().flex_wrap().items_center().justify_center().gap(px(2.)).max_w(px(190.)).children(buttons))
        .child(div().text_size(px(9.)).text_color(rgb(DIM)).child(title))
        .into_any_element()
}

impl Docxy {
    fn ribbon_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let names = ["File", "Home", "Styles", "Insert", "Review", "View"];
        let mut strip = h_flex().w_full().items_end().gap_1().px_2().pt_1().bg(rgb(RIBBON));
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
                    .text_size(px(12.))
                    .when(is_file, |d| d.bg(rgb(ACCENT)).text_color(rgb(0x102b26)).font_weight(FontWeight::BOLD).rounded_t_sm())
                    .when(active, |d| d.text_color(rgb(ACCENT)).border_b_2().border_color(rgb(ACCENT)))
                    .when(!active && !is_file, |d| d.text_color(rgb(FG)))
                    .child(*name)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if is_file {
                            this.backstage = true;
                            cx.notify();
                        } else if let Some(t) = this_tab {
                            this.ribbon_tab = t;
                            this.refocus(window, cx);
                        }
                    })),
            );
        }
        strip.into_any_element()
    }

    fn ribbon_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let groups: Vec<AnyElement> = match self.ribbon_tab {
            RibbonTab::Home => vec![
                group_box("Clipboard", vec![
                    abtn(cx, "cut", "\u{2702} Cut", |t, w, cx| t.do_copy(true, w, cx)),
                    abtn(cx, "copy", "\u{29C9} Copy", |t, w, cx| t.do_copy(false, w, cx)),
                    abtn(cx, "paste", "\u{1F4CB} Paste", |t, w, cx| t.do_paste(w, cx)),
                ]),
                group_box("Font", vec![
                    rbtn(cx, "b", "B", |e| e.toggle_bold()),
                    rbtn(cx, "i", "I", |e| e.toggle_italic()),
                    rbtn(cx, "u", "U", |e| e.toggle_underline()),
                    rbtn(cx, "s", "S", |e| e.toggle_strike()),
                    rbtn(cx, "grow", "A+", |e| e.resize_font(2)),
                    rbtn(cx, "shr", "A\u{2212}", |e| e.resize_font(-2)),
                ]),
                group_box("Paragraph", vec![
                    rbtn(cx, "al", "\u{2637}L", |e| e.set_align(Align::Left)),
                    rbtn(cx, "ac", "\u{2637}C", |e| e.set_align(Align::Center)),
                    rbtn(cx, "ar", "\u{2637}R", |e| e.set_align(Align::Right)),
                    rbtn(cx, "aj", "\u{2637}J", |e| e.set_align(Align::Justify)),
                    rbtn(cx, "ind", "\u{2192}|", |e| e.change_indent(1)),
                    rbtn(cx, "out", "|\u{2190}", |e| e.change_indent(-1)),
                ]),
                group_box("Editing", vec![
                    rbtn(cx, "undo", "\u{21B6}", |e| {
                        e.undo();
                    }),
                    rbtn(cx, "redo", "\u{21B7}", |e| {
                        e.redo();
                    }),
                ]),
            ],
            RibbonTab::Styles => vec![group_box("Styles", vec![
                rbtn(cx, "normal", "\u{00b6} Normal", |e| e.set_para_style(None)),
                rbtn(cx, "h1", "Heading 1", |e| e.set_para_style(Some("Heading1"))),
                rbtn(cx, "h2", "Heading 2", |e| e.set_para_style(Some("Heading2"))),
                rbtn(cx, "h3", "Heading 3", |e| e.set_para_style(Some("Heading3"))),
            ])],
            RibbonTab::Insert => vec![group_box("Symbols", vec![
                rbtn(cx, "hr", "\u{2015} Horizontal rule", |e| e.insert_hrule()),
                rbtn(cx, "para", "\u{00b6} Paragraph", |e| e.insert_newline()),
            ])],
            RibbonTab::Review => vec![group_box("Editing", vec![
                rbtn(cx, "selall", "Select all", |e| e.select_all()),
                rbtn(cx, "case", "Aa Case", |e| e.cycle_case()),
            ])],
            RibbonTab::View => vec![group_box("Show", vec![abtn(cx, "backstage2", "File \u{2026}", |t, _w, cx| {
                t.backstage = true;
                cx.notify();
            })])],
        };
        h_flex().w_full().h(px(76.)).items_stretch().px_1().bg(rgb(RIBBON)).border_b_1().border_color(rgb(DIV)).children(groups).into_any_element()
    }

    fn backstage_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let rail_item = |cx: &mut Context<Self>, id: &'static str, label: &'static str, f: fn(&mut Docxy, &mut Window, &mut Context<Docxy>)| {
            div()
                .id(id)
                .w_full()
                .px_4()
                .py_2()
                .cursor_pointer()
                .rounded_sm()
                .text_color(rgb(FG))
                .hover(|d| d.bg(rgb(0x3f3f46)))
                .child(label)
                .on_click(cx.listener(move |this, _, window, cx| f(this, window, cx)))
        };

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
                    .w_full()
                    .px_3()
                    .py_1p5()
                    .cursor_pointer()
                    .rounded_sm()
                    .text_color(rgb(FG))
                    .hover(|d| d.bg(rgb(0x2d2d30)))
                    .child(format!("{} {}", t.kind.glyph(), t.title))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.backstage = false;
                        this.select_tab(i, window, cx);
                    }))
                    .into_any_element()
            })
            .collect();

        let rail = v_flex()
            .w(px(220.))
            .h_full()
            .py_3()
            .gap_1()
            .bg(rgb(RAIL))
            .child(
                div()
                    .id("bs-back")
                    .px_4()
                    .py_2()
                    .cursor_pointer()
                    .text_color(rgb(ACCENT))
                    .child("\u{2190} Back")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.backstage = false;
                        this.refocus(window, cx);
                    })),
            )
            .child(rail_item(cx, "new", "New", |t, w, cx| t.add_tab(Kind::Docx, w, cx)))
            .child(rail_item(cx, "open", "Open\u{2026}", |t, w, cx| t.open_file(w, cx)))
            .child(rail_item(cx, "save", "Save", |t, w, cx| t.save_active(w, cx)))
            .child(rail_item(cx, "saveas", "Save As\u{2026}", |t, w, cx| t.save_as(w, cx)))
            .child(rail_item(cx, "close", "Close", |t, w, cx| {
                let a = t.active;
                t.backstage = false;
                t.close_tab(a, w, cx);
            }));

        let pane = v_flex()
            .flex_1()
            .h_full()
            .p_8()
            .gap_4()
            .bg(rgb(BG))
            .child(div().text_size(px(22.)).font_weight(FontWeight::BOLD).text_color(rgb(FG)).child(cur_title))
            .child(div().text_size(px(12.)).text_color(rgb(DIM)).child(cur_path))
            .child(div().text_size(px(13.)).text_color(rgb(ACCENT)).mt_4().child("Open"))
            .child(v_flex().gap_0p5().children(recents));

        h_flex().size_full().child(rail).child(pane).into_any_element()
    }
}

impl Render for Docxy {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused {
            self.focus.focus(window, cx);
            self.focused = true;
        }

        let new_btn = |id: &'static str, label: &'static str, kind: Kind| {
            Button::new(id).small().ghost().label(label).on_click(cx.listener(move |this, _, window, cx| this.add_tab(kind, window, cx)))
        };
        let title_bar = TitleBar::new().child(
            h_flex()
                .items_center()
                .gap_2()
                .pl_2()
                .child(div().font_weight(FontWeight::BOLD).text_color(rgb(ACCENT)).child("docxy"))
                .child(new_btn("new-doc", "+ Doc", Kind::Docx))
                .child(new_btn("new-sheet", "+ Sheet", Kind::Xlsx))
                .child(new_btn("new-mail", "+ Mail", Kind::Look)),
        );

        if self.backstage {
            let backstage = self.backstage_view(cx);
            return v_flex().size_full().bg(rgb(BG)).track_focus(&self.focus).child(title_bar).child(backstage).into_any_element();
        }

        let is_doc = matches!(self.tabs.get(self.active).map(|t| &t.surface), Some(Surface::Doc(_)));
        let ribbon_tabs = self.ribbon_tabs(cx);
        let ribbon_body = is_doc.then(|| self.ribbon_body(cx));

        let doc_tabs = self.tabs.iter().enumerate().map(|(i, t)| {
            let mark = if t.dirty { " \u{2022}" } else { "" };
            let label = format!("{} {}{}", t.kind.glyph(), t.title, mark);
            Tab::new().child(label).suffix(Button::new(("close", i)).xsmall().ghost().label("\u{00d7}").on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.close_tab(i, window, cx);
            })))
        });
        let tab_bar = TabBar::new("docxy-tabs").w_full().selected_index(self.active).children(doc_tabs).on_click(cx.listener(|this, ix: &usize, window, cx| this.select_tab(*ix, window, cx)));

        let content: AnyElement = match self.tabs.get(self.active) {
            Some(t) => match &t.surface {
                Surface::Doc(editor) => {
                    let caret_block = (editor.caret.path.len() == 1).then_some(editor.caret.path[0]);
                    let off = editor.caret.offset;
                    let blocks: Vec<AnyElement> = editor.doc.body.iter().enumerate().map(|(i, b)| block_el(b, (Some(i) == caret_block).then_some(off))).collect();
                    v_flex().id("doc-scroll").flex_1().overflow_y_scroll().bg(rgb(BG)).text_color(rgb(FG)).px(px(48.)).py(px(28.)).gap_1().children(blocks).into_any_element()
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
            .child("type · Ctrl+B/I/U · Ctrl+C/X/V · Ctrl+Z/Y · Ctrl+S");

        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .bg(rgb(BG))
            .child(title_bar)
            .child(ribbon_tabs)
            .when_some(ribbon_body, |d, r| d.child(r))
            .child(tab_bar)
            .child(content)
            .child(status)
            .into_any_element()
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
    gpui_platform::application().with_assets(gpui_component_assets::Assets).run(move |cx: &mut App| {
        gpui_component::init(cx);
        let bounds = Bounds::centered(None, size(px(1180.), px(800.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitleBar::title_bar_options()),
            window_min_size: Some(size(px(720.), px(460.))),
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
