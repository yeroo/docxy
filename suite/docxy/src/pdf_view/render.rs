//! Drawing the PDF view (#2245): its toolbar, the Find bar, the outline
//! pane and the page column. Each page is its raster (stretched while one
//! at the new zoom is drawn), or a placeholder that says why it is not;
//! the selection and Find's hits are boxes over the glyphs. Only the pages
//! on screen (and one either side) are drawn.

use super::{Field, PX_PER_PT, PdfView};
use crate::*;

/// A toolbar or Find bar command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PdfCmd {
    Outline,
    ZoomOut,
    ZoomIn,
    Fit,
    Find,
    Convert,
    FindNext,
    FindPrev,
    FindCase,
    FindWhole,
    FindClose,
}

/// The selection's box colour: readable over a white page in either theme.
const SEL: u32 = 0x3B82F6;
const HIT: u32 = 0xFACC15;
const HIT_NOW: u32 = 0xF97316;

fn rgba(rgb: u32, a: f32) -> Hsla {
    let mut c: Hsla = rgb_u(rgb);
    c.a = a;
    c
}

fn rgb_u(rgb: u32) -> Hsla {
    gpui::rgb(rgb).into()
}

impl Docxy {
    pub(crate) fn pdf_cmd(&mut self, cmd: PdfCmd, window: &mut Window, cx: &mut Context<Self>) {
        match cmd {
            PdfCmd::Outline => self.pdf_toggle_outline(cx),
            PdfCmd::ZoomOut => self.with_pdf(cx, |v| v.zoom_in(false)),
            PdfCmd::ZoomIn => self.with_pdf(cx, |v| v.zoom_in(true)),
            PdfCmd::Fit => self.with_pdf(cx, PdfView::fit_width),
            PdfCmd::Find => self.with_pdf(cx, |v| {
                v.find.open = true;
                v.page_box = Field::default();
                v.find.field.focus_all();
            }),
            PdfCmd::Convert => self.pdf_convert(window, cx),
            PdfCmd::FindNext => self.with_pdf(cx, |v| v.find_step(true)),
            PdfCmd::FindPrev => self.with_pdf(cx, |v| v.find_step(false)),
            PdfCmd::FindCase => self.with_pdf(cx, |v| {
                v.find.case = !v.find.case;
                v.find_changed();
            }),
            PdfCmd::FindWhole => self.with_pdf(cx, |v| {
                v.find.whole = !v.find.whole;
                v.find_changed();
            }),
            PdfCmd::FindClose => self.with_pdf(cx, |v| {
                v.find.open = false;
                v.find.field.focused = false;
            }),
        }
    }

    pub(crate) fn pdf_body(
        &self,
        v: &PdfView,
        pal: Pal,
        bg: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // A scroll asked for since the last frame (gpui places it now).
        if let Some((x, y)) = v.scroll_to.take() {
            v.scroll.set_offset(point(px(-x), px(-y)));
        }
        v_flex()
            .flex_1()
            .min_h(px(0.))
            .size_full()
            .bg(bg)
            .text_color(pal.fg)
            .child(self.pdf_toolbar(v, pal, cx))
            .when(v.find.open, |d| d.child(self.pdf_find_bar(v, pal, cx)))
            .child(
                h_flex()
                    .flex_1()
                    .min_h(px(0.))
                    .w_full()
                    .when(v.outline_shown, |d| d.child(self.pdf_outline(v, pal, cx)))
                    .child(self.pdf_pages(v, pal, cx)),
            )
            .into_any_element()
    }

    fn pdf_button(
        &self,
        label: String,
        probe_name: &'static str,
        on: bool,
        cmd: PdfCmd,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .relative()
            .px_2()
            .py(px(2.))
            .rounded_sm()
            .border_1()
            .border_color(pal.border)
            .text_size(px(12.))
            .cursor_pointer()
            .hover(|d| d.bg(pal.hover))
            .when(on, |d| d.bg(pal.sel))
            .child(label)
            .child(probe(&self.probes, probe_name))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.pdf_cmd(cmd, window, cx);
                }),
            )
            .into_any_element()
    }

    /// A one-line field: its text with the caret where it is while it has
    /// the keys, all of it shown selected when selected.
    fn pdf_field(
        &self,
        f: &Field,
        shown: String,
        width: f32,
        probe_name: &'static str,
        pal: Pal,
    ) -> Div {
        let text = if f.focused { f.text.clone() } else { shown };
        let caret = f.caret.min(text.chars().count());
        let (before, after): (String, String) = if f.focused && !f.sel {
            (
                text.chars().take(caret).collect(),
                text.chars().skip(caret).collect(),
            )
        } else {
            (text.clone(), String::new())
        };
        div()
            .relative()
            .w(px(width))
            .h(px(22.))
            .px_1()
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded_sm()
            .border_1()
            .border_color(if f.focused { rgb_u(SEL) } else { pal.border })
            .bg(pal.panel)
            .text_size(px(12.))
            .cursor_text()
            .child(
                div()
                    .when(f.focused && f.sel && !text.is_empty(), |d| {
                        d.bg(rgba(SEL, 0.35))
                    })
                    .child(before),
            )
            .when(f.focused && !f.sel, |d| {
                d.child(div().w(px(1.)).h(px(14.)).bg(pal.fg))
            })
            .child(div().child(after))
            .child(probe(&self.probes, probe_name))
    }

    fn pdf_toolbar(&self, v: &PdfView, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let zoom = format!("{}%", (v.zoom * 100.).round() as i32);
        let count = v.page_count();
        let page_box = self
            .pdf_field(
                &v.page_box,
                format!("{}", v.current() + 1),
                44.,
                "pdf-page-box",
                pal,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.pdf_page_box_press(cx);
                }),
            );
        h_flex()
            .w_full()
            .gap_1()
            .px_2()
            .py_1()
            .items_center()
            .border_b_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .text_size(px(12.))
            .child(self.pdf_button(
                "\u{2630} Bookmarks".into(),
                "pdf-outline-toggle",
                v.outline_shown,
                PdfCmd::Outline,
                pal,
                cx,
            ))
            .child(self.pdf_button(
                "\u{2212}".into(),
                "pdf-zoom-out",
                false,
                PdfCmd::ZoomOut,
                pal,
                cx,
            ))
            .child(
                div()
                    .relative()
                    .min_w(px(44.))
                    .flex()
                    .justify_center()
                    .child(zoom)
                    .child(probe(&self.probes, "pdf-zoom")),
            )
            .child(self.pdf_button("+".into(), "pdf-zoom-in", false, PdfCmd::ZoomIn, pal, cx))
            .child(self.pdf_button("Fit width".into(), "pdf-fit", false, PdfCmd::Fit, pal, cx))
            .child(div().w(px(12.)))
            .child("Page")
            .child(page_box)
            .child(format!("/ {count}"))
            .child(div().w(px(12.)))
            .child(self.pdf_button(
                "Find".into(),
                "pdf-find-open",
                v.find.open,
                PdfCmd::Find,
                pal,
                cx,
            ))
            .child(div().flex_1())
            .child(self.pdf_button(
                "Convert to editable document".into(),
                "pdf-convert",
                false,
                PdfCmd::Convert,
                pal,
                cx,
            ))
            .into_any_element()
    }

    fn pdf_find_bar(&self, v: &PdfView, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let field = self
            .pdf_field(
                &v.find.field,
                v.find.field.text.clone(),
                220.,
                "pdf-find",
                pal,
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.pdf_find_press(cx);
                }),
            );
        h_flex()
            .w_full()
            .gap_1()
            .px_2()
            .py_1()
            .items_center()
            .border_b_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .text_size(px(12.))
            .child("Find")
            .child(field)
            .child(
                div()
                    .relative()
                    .min_w(px(120.))
                    .text_color(pal.dim)
                    .child(v.find_status())
                    .child(probe(&self.probes, "pdf-find-status")),
            )
            .child(self.pdf_button(
                "\u{2191}".into(),
                "pdf-find-prev",
                false,
                PdfCmd::FindPrev,
                pal,
                cx,
            ))
            .child(self.pdf_button(
                "\u{2193}".into(),
                "pdf-find-next",
                false,
                PdfCmd::FindNext,
                pal,
                cx,
            ))
            .child(self.pdf_button(
                "Match case".into(),
                "pdf-find-case",
                v.find.case,
                PdfCmd::FindCase,
                pal,
                cx,
            ))
            .child(self.pdf_button(
                "Whole words".into(),
                "pdf-find-whole",
                v.find.whole,
                PdfCmd::FindWhole,
                pal,
                cx,
            ))
            .child(div().flex_1())
            .child(self.pdf_button(
                "\u{2715}".into(),
                "pdf-find-close",
                false,
                PdfCmd::FindClose,
                pal,
                cx,
            ))
            .into_any_element()
    }

    /// The bookmarks, drawn as the Navigation pane (its F1 is that pane's).
    fn pdf_outline(&self, v: &PdfView, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let rows: Vec<AnyElement> = if v.meta.outline.is_empty() {
            vec![
                div()
                    .p_2()
                    .text_color(pal.dim)
                    .child("This document has no bookmarks")
                    .into_any_element(),
            ]
        } else {
            v.meta
                .outline
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    let page = b.page;
                    div()
                        .relative()
                        .pl(px(8. + b.depth.min(8) as f32 * 14.))
                        .pr_2()
                        .py(px(3.))
                        .when(page.is_some(), |d| {
                            d.cursor_pointer().hover(|d| d.bg(pal.hover))
                        })
                        .when(page.is_none(), |d| d.text_color(pal.dim))
                        .child(b.title.clone())
                        .child(probe(&self.probes, format!("pdf-bookmark:{}", i + 1)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                cx.stop_propagation();
                                if let Some(p) = page {
                                    this.with_pdf(cx, |v| v.go_to(p));
                                }
                            }),
                        )
                        .into_any_element()
                })
                .collect()
        };
        v_flex()
            .relative()
            .w(px(240.))
            .h_full()
            .flex_none()
            .border_r_1()
            .border_color(pal.border)
            .bg(pal.panel)
            .text_size(px(12.))
            .child(
                div()
                    .p_2()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Bookmarks"),
            )
            .child(
                div()
                    .id("pdf-outline")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .child(probe(
                &self.probes,
                help_tab::pane_probe(help_tab::Pane::Navigation),
            ))
            .into_any_element()
    }

    fn pdf_pages(&self, v: &PdfView, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let l = v.layout();
        let k = v.zoom * PX_PER_PT;
        let n = v.page_count();
        let from = v.showing.start.saturating_sub(1);
        let to = (v.showing.end + 1).min(n);
        let mut pages: Vec<AnyElement> = Vec::new();
        for p in from..to {
            let r = l.pages[p];
            let boxes = |rects: Vec<[f32; 4]>, color: Hsla| -> Vec<AnyElement> {
                rects
                    .into_iter()
                    .map(|b| {
                        div()
                            .absolute()
                            .left(px(b[0] * k))
                            .top(px(b[1] * k))
                            .w(px(((b[2] - b[0]) * k).max(1.)))
                            .h(px(((b[3] - b[1]) * k).max(1.)))
                            .bg(color)
                            .into_any_element()
                    })
                    .collect()
            };
            let mut marks: Vec<AnyElement> = Vec::new();
            if let Some(t) = &v.texts[p] {
                for (i, h) in v.find.hits.iter().enumerate().filter(|(_, h)| h.page == p) {
                    let now = v.find.current == Some(i);
                    let color = if now {
                        rgba(HIT_NOW, 0.5)
                    } else {
                        rgba(HIT, 0.45)
                    };
                    marks.extend(boxes(t.line_rects(h.glyphs.clone()), color));
                }
                if let Some(range) = v.sel.and_then(|s| s.on_page(p, t.glyphs.len())) {
                    marks.extend(boxes(t.line_rects(range), rgba(SEL, 0.35)));
                }
            }
            let body: AnyElement = match (&v.rasters[p], v.placeholder(p)) {
                (Some(c), _) => img(c.image.clone())
                    .absolute()
                    .left_0()
                    .top_0()
                    .size_full()
                    .object_fit(ObjectFit::Fill)
                    .into_any_element(),
                (None, Some(why)) => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p_4()
                    .text_color(rgb_u(0x7F1D1D))
                    .text_size(px(13.))
                    .child(why)
                    .into_any_element(),
                (None, None) => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb_u(0x6B7280))
                    .text_size(px(12.))
                    .child(format!("Drawing page {}…", p + 1))
                    .into_any_element(),
            };
            pages.push(
                div()
                    .absolute()
                    .left(px(r.x))
                    .top(px(r.y))
                    .w(px(r.w))
                    .h(px(r.h))
                    // A page is paper: white in either theme, as a reader
                    // shows it.
                    .bg(rgb_u(0xFFFFFF))
                    .border_1()
                    .border_color(pal.border)
                    .shadow_sm()
                    .overflow_hidden()
                    .child(body)
                    .children(marks)
                    .child(probe(&self.probes, format!("pdf-page:{}", p + 1)))
                    .into_any_element(),
            );
        }
        let content = div()
            .relative()
            .w(px(l.width))
            .h(px(l.height))
            .cursor_text()
            .children(pages)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _w, cx| {
                    let Some(v) = this.active_pdf() else { return };
                    let (x, y) = content_point(v, ev.position);
                    this.pdf_page_press(x, y, ev.click_count, ev.modifiers.shift, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                if ev.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                let Some(v) = this.active_pdf() else { return };
                if v.drag.is_none() {
                    return;
                }
                let (x, y) = content_point(v, ev.position);
                this.with_pdf(cx, |v| v.drag_to(x, y));
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _w, cx| this.with_pdf(cx, PdfView::release)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _w, cx| this.with_pdf(cx, PdfView::release)),
            )
            // Ctrl+wheel zooms (and does not scroll): the content is drawn
            // over the scroll area, so it hears the wheel first.
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _w, cx| {
                if !(ev.modifiers.control || ev.modifiers.platform) {
                    return;
                }
                let dy = match ev.delta {
                    ScrollDelta::Lines(p) => p.y,
                    ScrollDelta::Pixels(p) => f32::from(p.y) / 40.,
                };
                if dy != 0. {
                    cx.stop_propagation();
                    this.with_pdf(cx, |v| {
                        let z = v.zoom * 1.1f32.powf(dy.clamp(-5., 5.));
                        v.set_zoom(z);
                    });
                }
            }));
        div()
            .id("pdf-scroll")
            .relative()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .overflow_scroll()
            .track_scroll(&v.scroll)
            .bg(pal.panel)
            .child(content)
            .child(probe(&self.probes, "pdf-body"))
            .into_any_element()
    }
}

/// A window position as content coordinates of the page column.
pub(crate) fn content_point(v: &PdfView, at: Point<Pixels>) -> (f32, f32) {
    let b = v.scroll.bounds();
    let o = v.scroll.offset();
    (
        f32::from(at.x - b.origin.x - o.x),
        f32::from(at.y - b.origin.y - o.y),
    )
}

/// Content coordinates of the page column as a window position: what the
/// harness presses for a glyph.
pub(crate) fn window_point(v: &PdfView, x: f32, y: f32) -> Point<Pixels> {
    let b = v.scroll.bounds();
    let o = v.scroll.offset();
    point(b.origin.x + o.x + px(x), b.origin.y + o.y + px(y))
}
