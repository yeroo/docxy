//! officexy — the suite host: docxy / xlsxy / lookxy in tabs, on GPUI.
//!
//! One window with a custom title bar (minimize / maximize / close), a tab strip,
//! and a content area. Tabs AND their unsaved content survive restart: the whole
//! session (which tabs, which files, the current editor text) is written to
//! `<config>/officexy/session.json` on every edit and structural change, and
//! restored on launch — closing never prompts to save (hot-exit).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Root, Sizable, TitleBar,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputEvent, InputState},
    tab::{Tab, TabBar},
    v_flex,
};
use serde::{Deserialize, Serialize};

// ---- session model (persisted) --------------------------------------------

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
enum Kind {
    Docx,
    Xlsx,
    Look,
}

impl Kind {
    fn glyph(self) -> &'static str {
        match self {
            Kind::Docx => "\u{1F4C4}", // 📄
            Kind::Xlsx => "\u{1F4CA}", // 📊
            Kind::Look => "\u{2709}",  // ✉
        }
    }
}

#[derive(Serialize, Deserialize)]
struct PersistTab {
    kind: Kind,
    title: String,
    path: Option<String>,
    content: String,
    dirty: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct Session {
    tabs: Vec<PersistTab>,
    active: usize,
}

fn session_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("officexy")
        .join("session.json")
}

fn load_session() -> Session {
    std::fs::read(session_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

// ---- runtime ---------------------------------------------------------------

struct DocTab {
    kind: Kind,
    title: SharedString,
    path: Option<PathBuf>,
    /// The markdown editor (Docx only); None for the placeholder surfaces.
    editor: Option<Entity<InputState>>,
    /// Last known text — the source of truth for non-editor tabs and the value
    /// persisted for editor tabs (refreshed from the editor at persist time).
    content: String,
    dirty: bool,
    /// Keeps the change subscription alive for as long as the tab exists.
    _sub: Option<Subscription>,
}

struct Officexy {
    tabs: Vec<DocTab>,
    active: usize,
}

impl Officexy {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let session = load_session();
        let mut this = Self { tabs: Vec::new(), active: 0 };
        for t in session.tabs {
            let path = t.path.map(PathBuf::from);
            let tab = this.make_tab(t.kind, t.title.into(), path, t.content, t.dirty, window, cx);
            this.tabs.push(tab);
        }
        if this.tabs.is_empty() {
            // First run: open the bundled sample as a docx tab.
            let md = docxcore::load::load(include_bytes!("../../../assets/sample.docx"))
                .map(|d| docxcore::markdown::to_markdown(&d))
                .unwrap_or_else(|e| format!("failed to load sample: {e:?}"));
            let tab = this.make_tab(
                Kind::Docx,
                "sample.docx".into(),
                None,
                md,
                false,
                window,
                cx,
            );
            this.tabs.push(tab);
        }
        this.active = session.active.min(this.tabs.len().saturating_sub(1));
        this.persist(cx); // ensure the session file exists from first run
        this
    }

    /// Build a runtime tab; for Docx this creates the code editor and wires its
    /// change subscription to persist (so unsaved text survives a restart/kill).
    fn make_tab(
        &mut self,
        kind: Kind,
        title: SharedString,
        path: Option<PathBuf>,
        content: String,
        dirty: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> DocTab {
        let (editor, sub) = if kind == Kind::Docx {
            let content = content.clone();
            let editor = cx.new(|cx| {
                InputState::new(window, cx)
                    .code_editor("markdown")
                    .multi_line(true)
                    .line_number(true)
                    .soft_wrap(true)
                    .default_value(content)
            });
            let sub = cx.subscribe(&editor, |this, changed, ev: &InputEvent, cx| {
                if let InputEvent::Change = ev {
                    if let Some(tab) =
                        this.tabs.iter_mut().find(|t| {
                            t.editor.as_ref().map(|e| e.entity_id()) == Some(changed.entity_id())
                        })
                    {
                        tab.dirty = true;
                    }
                    this.persist(cx);
                    cx.notify();
                }
            });
            (Some(editor), Some(sub))
        } else {
            (None, None)
        };
        DocTab { kind, title, path, editor, content, dirty, _sub: sub }
    }

    /// Snapshot the whole session to disk (called on every edit + structural change).
    fn persist(&self, cx: &App) {
        let tabs = self
            .tabs
            .iter()
            .map(|t| PersistTab {
                kind: t.kind,
                title: t.title.to_string(),
                path: t.path.as_ref().map(|p| p.display().to_string()),
                content: t
                    .editor
                    .as_ref()
                    .map(|e| e.read(cx).value().to_string())
                    .unwrap_or_else(|| t.content.clone()),
                dirty: t.dirty,
            })
            .collect();
        let session = Session { tabs, active: self.active };
        if let Ok(json) = serde_json::to_string_pretty(&session) {
            let path = session_path();
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, json);
        }
    }

    fn add_tab(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        let (title, content): (SharedString, String) = match kind {
            Kind::Docx => ("Untitled.docx".into(), "# Untitled\n\n".into()),
            Kind::Xlsx => ("Untitled.xlsx".into(), String::new()),
            Kind::Look => ("Inbox".into(), String::new()),
        };
        let tab = self.make_tab(kind, title, None, content, false, window, cx);
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
        self.persist(cx);
        cx.notify();
    }

    fn select_tab(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.tabs.len() {
            self.active = i;
            self.persist(cx);
            cx.notify();
        }
    }

    fn close_tab(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i); // drops the editor + its subscription
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if i < self.active {
            self.active -= 1;
        }
        self.persist(cx);
        cx.notify();
    }

    /// Save the active docx tab back to a .docx via the real engine.
    fn save_active(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let Some(editor) = &tab.editor else { return };
        let md = editor.read(cx).value().to_string();
        let doc = docxcore::markdown::from_markdown(&md);
        let pkg = docxcore::package::new_package(doc);
        let bytes = docxcore::package::save_package(&pkg);
        let path = tab.path.clone().unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_default().join(tab.title.to_string())
        });
        if std::fs::write(&path, &bytes).is_ok() {
            tab.path = Some(path);
            tab.dirty = false;
        }
        self.persist(cx);
        cx.notify();
    }
}

const BG: u32 = 0x1e1e1e;
const DIM: u32 = 0x858585;
const ACCENT: u32 = 0x4ec9b0;
const DIRTY: u32 = 0xe2c08d;

impl Render for Officexy {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // --- title bar: app name + New buttons (min/max/close added by TitleBar) ---
        let new_btn = |id: &'static str, label: &'static str, kind: Kind| {
            Button::new(id).small().ghost().label(label).on_click(cx.listener(
                move |this, _, window, cx| this.add_tab(kind, window, cx),
            ))
        };
        let can_save = self.tabs.get(self.active).map(|t| t.kind == Kind::Docx).unwrap_or(false);
        let title_bar = TitleBar::new().child(
            h_flex()
                .items_center()
                .gap_2()
                .pl_2()
                .child(div().font_weight(FontWeight::BOLD).text_color(rgb(ACCENT)).child("officexy"))
                .child(new_btn("new-doc", "+ Doc", Kind::Docx))
                .child(new_btn("new-sheet", "+ Sheet", Kind::Xlsx))
                .child(new_btn("new-mail", "+ Mail", Kind::Look))
                .when(can_save, |this| {
                    this.child(
                        Button::new("save")
                            .small()
                            .primary()
                            .label("Save")
                            .on_click(cx.listener(|this, _, _, cx| this.save_active(cx))),
                    )
                }),
        );

        // --- tab strip ---
        let tabs = self.tabs.iter().enumerate().map(|(i, t)| {
            let mark = if t.dirty { " \u{2022}" } else { "" };
            let label = format!("{} {}{}", t.kind.glyph(), t.title, mark);
            Tab::new().child(label).suffix(
                Button::new(("close", i))
                    .xsmall()
                    .ghost()
                    .label("\u{00d7}")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.close_tab(i, cx);
                    })),
            )
        });
        let tab_bar = TabBar::new("officexy-tabs")
            .w_full()
            .selected_index(self.active)
            .children(tabs)
            .on_click(cx.listener(|this, ix: &usize, _, cx| this.select_tab(*ix, cx)));

        // --- content of the active tab ---
        let content: AnyElement = match self.tabs.get(self.active) {
            Some(t) if t.kind == Kind::Docx => Input::new(t.editor.as_ref().unwrap())
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(cx.theme().mono_font_size)
                .flex_1()
                .into_any_element(),
            Some(t) => placeholder(t.kind).into_any_element(),
            None => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .text_color(rgb(DIM))
                .child("No documents open — use + Doc / + Sheet / + Mail")
                .into_any_element(),
        };

        v_flex().size_full().bg(rgb(BG)).child(title_bar).child(tab_bar).child(content)
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
            let view = cx.new(|cx| Officexy::new(window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("failed to open officexy window");
    });
}
