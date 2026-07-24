//! docxy (GPUI) — milestone 1: open a real .docx and show it.
//!
//! Loads a .docx (CLI arg, or the bundled sample) via `docxcore`, projects it to
//! markdown (`docxcore::markdown::to_markdown`), and shows that markdown SOURCE in
//! a code-editor-style monospace view — the developer-facing, no-WYSIWYG take on a
//! word processor. The editable code-editor component + the shared shell come next.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use gpui::*;
use gpui_component::{Root, h_flex, v_flex};

/// A .docx projected to markdown, ready to display.
struct DocxyApp {
    title: SharedString,
    lines: Vec<SharedString>,
    blocks: usize,
    chars: usize,
    error: Option<SharedString>,
}

impl DocxyApp {
    fn from_docx(bytes: &[u8], name: impl Into<SharedString>) -> Self {
        let title = name.into();
        match docxcore::load::load(bytes) {
            Ok(doc) => {
                let md = docxcore::markdown::to_markdown(&doc);
                let chars = md.chars().count();
                let blocks = doc.body.len();
                // Preserve blank lines (empty text collapses to zero height).
                let lines = md
                    .split('\n')
                    .map(|l| {
                        if l.is_empty() {
                            SharedString::from("\u{00a0}")
                        } else {
                            SharedString::from(l.to_string())
                        }
                    })
                    .collect();
                Self { title, lines, blocks, chars, error: None }
            }
            Err(e) => Self {
                title,
                lines: Vec::new(),
                blocks: 0,
                chars: 0,
                error: Some(SharedString::from(format!("failed to load .docx: {e:?}"))),
            },
        }
    }
}

// VS Code-ish dark palette (kept literal for now; moves to the shared theme later).
const BG: u32 = 0x1e1e1e;
const PANEL: u32 = 0x252526;
const FG: u32 = 0xd4d4d4;
const DIM: u32 = 0x858585;
const ACCENT: u32 = 0x4ec9b0;
const ERR: u32 = 0xf48771;

impl Render for DocxyApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let header = h_flex()
            .w_full()
            .px_4()
            .py_2()
            .gap_2()
            .bg(rgb(PANEL))
            .items_center()
            .child(div().text_color(rgb(ACCENT)).font_weight(FontWeight::BOLD).child("docxy"))
            .child(div().text_color(rgb(DIM)).child("—"))
            .child(div().text_color(rgb(FG)).child(self.title.clone()));

        let body: gpui::AnyElement = if let Some(err) = &self.error {
            div()
                .flex_1()
                .p_4()
                .text_color(rgb(ERR))
                .child(err.clone())
                .into_any_element()
        } else {
            div()
                .id("doc-scroll")
                .flex_1()
                .overflow_y_scroll()
                .px_4()
                .py_3()
                .font_family("Consolas")
                .text_size(px(13.))
                .text_color(rgb(FG))
                .children(self.lines.iter().cloned().map(|l| div().child(l)))
                .into_any_element()
        };

        let status = h_flex()
            .w_full()
            .px_4()
            .py_1()
            .gap_3()
            .bg(rgb(PANEL))
            .text_size(px(11.))
            .text_color(rgb(DIM))
            .child(format!("{} blocks", self.blocks))
            .child(format!("{} lines", self.lines.len()))
            .child(format!("{} chars", self.chars))
            .child(div().flex_1())
            .child("markdown view");

        v_flex().size_full().bg(rgb(BG)).text_color(rgb(FG)).child(header).child(body).child(status)
    }
}

fn main() {
    // Open the file named on the command line, else the bundled sample.
    let (bytes, name): (Vec<u8>, String) = match std::env::args().nth(1) {
        Some(path) => match std::fs::read(&path) {
            Ok(b) => (b, path),
            Err(e) => {
                eprintln!("docxy: cannot read {path}: {e}");
                std::process::exit(1);
            }
        },
        None => (
            include_bytes!("../../../assets/sample.docx").to_vec(),
            "assets/sample.docx".to_string(),
        ),
    };

    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        let app = DocxyApp::from_docx(&bytes, name);
        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|_| app);
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open docxy window");
        })
        .detach();
    });
}
