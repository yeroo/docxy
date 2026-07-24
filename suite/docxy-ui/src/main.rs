//! docxy (GPUI) — milestone 2: an editable .docx, as markdown.
//!
//! Loads a .docx via `docxcore`, projects it to markdown, and puts it in a real
//! code editor (gpui-component `InputState::code_editor("markdown")` — tree-sitter
//! highlighting, line numbers, soft wrap). Save projects the edited markdown back
//! through `docxcore::markdown::from_markdown` -> `new_package` -> `save_package`
//! and writes a .docx. The developer-facing, no-WYSIWYG word processor: edit the
//! document as text, round-trip through the real docx engine.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

use gpui::*;
use gpui_component::{
    ActiveTheme, Root,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    v_flex,
};

struct DocxyApp {
    editor: Entity<InputState>,
    save_path: PathBuf,
    title: SharedString,
    status: SharedString,
}

impl DocxyApp {
    fn new(
        markdown: String,
        save_path: PathBuf,
        title: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            InputState::new(window, cx)
                .code_editor("markdown")
                .multi_line(true)
                .line_number(true)
                .soft_wrap(true)
                .default_value(markdown)
        });
        Self {
            editor,
            save_path,
            title,
            status: SharedString::from("ready — edit the markdown, then Save"),
        }
    }

    /// markdown -> Document -> .docx bytes -> file, via the real docx engine.
    fn save(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let md = self.editor.read(cx).value().to_string();
        let doc = docxcore::markdown::from_markdown(&md);
        let pkg = docxcore::package::new_package(doc);
        let bytes = docxcore::package::save_package(&pkg);
        self.status = match std::fs::write(&self.save_path, &bytes) {
            Ok(()) => {
                SharedString::from(format!("saved {} bytes → {}", bytes.len(), self.save_path.display()))
            }
            Err(e) => SharedString::from(format!("save failed: {e}")),
        };
        cx.notify();
    }
}

const PANEL: u32 = 0x252526;
const FG: u32 = 0xd4d4d4;
const DIM: u32 = 0x858585;
const ACCENT: u32 = 0x4ec9b0;

impl Render for DocxyApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = h_flex()
            .w_full()
            .px_4()
            .py_2()
            .gap_2()
            .bg(rgb(PANEL))
            .items_center()
            .child(div().text_color(rgb(ACCENT)).font_weight(FontWeight::BOLD).child("docxy"))
            .child(div().text_color(rgb(DIM)).child("—"))
            .child(div().flex_1().text_color(rgb(FG)).child(self.title.clone()))
            .child(
                Button::new("save")
                    .primary()
                    .label("Save .docx")
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            );

        let editor = Input::new(&self.editor)
            .font_family(cx.theme().mono_font_family.clone())
            .text_size(cx.theme().mono_font_size)
            .flex_1();

        let status = h_flex()
            .w_full()
            .px_4()
            .py_1()
            .bg(rgb(PANEL))
            .text_size(px(11.))
            .text_color(rgb(DIM))
            .child(self.status.clone());

        v_flex().size_full().child(header).child(editor).child(status)
    }
}

/// Load `.docx` bytes and project to (markdown, window title). On failure, put the
/// error in the editor so it's visible rather than crashing.
fn to_markdown(bytes: &[u8]) -> Result<String, String> {
    docxcore::load::load(bytes)
        .map(|doc| docxcore::markdown::to_markdown(&doc))
        .map_err(|e| format!("failed to load .docx: {e:?}"))
}

fn main() {
    let (markdown, save_path, title): (String, PathBuf, SharedString) =
        match std::env::args().nth(1) {
            Some(path) => {
                let bytes = std::fs::read(&path).unwrap_or_else(|e| {
                    eprintln!("docxy: cannot read {path}: {e}");
                    std::process::exit(1);
                });
                let md = to_markdown(&bytes).unwrap_or_else(|e| e);
                (md, PathBuf::from(&path), SharedString::from(path))
            }
            None => {
                let md = to_markdown(include_bytes!("../../../assets/sample.docx"))
                    .unwrap_or_else(|e| e);
                let out = std::env::current_dir()
                    .unwrap_or_default()
                    .join("docxy-sample-edited.docx");
                (md, out, SharedString::from("sample.docx (Save writes ./docxy-sample-edited.docx)"))
            }
        };

    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|cx| DocxyApp::new(markdown, save_path, title, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open docxy window");
        })
        .detach();
    });
}
