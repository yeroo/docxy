//! docxy (GPUI) — toolchain-validation window.
//!
//! Milestone 0: prove the GPUI + gpui-component stack builds and opens a window
//! in this environment before any real UI is built on top. The real app shell
//! (dock layout, command palette, the docxcore-backed document view) comes next.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use gpui::*;
use gpui_component::{
    Root,
    button::{Button, ButtonVariants},
    v_flex,
};

struct DocxyRoot;

impl Render for DocxyRoot {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_4()
            .child("docxy — GPUI suite (milestone 0)")
            .child(
                Button::new("go")
                    .primary()
                    .label("It builds!")
                    .on_click(|_, _, _| println!("docxy: window is live")),
            )
    }
}

fn main() {
    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        cx.spawn(async move |cx| {
            cx.open_window(WindowOptions::default(), |window, cx| {
                let view = cx.new(|_| DocxyRoot);
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open docxy window");
        })
        .detach();
    });
}
