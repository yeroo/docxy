//! Drawing the PDF view (#2245): its toolbar, the Find bar, the outline
//! pane and the page column. Each page is its raster (stretched while one
//! at the new zoom is drawn), or a placeholder that says why it is not;
//! the selection and Find's hits are boxes over the glyphs, form fields'
//! highlights and the focused field's editor over its widget (#2255). A
//! combo box's list, a comment's pop-up and the tooltip are drawn over the
//! column. Only the pages on screen (and one either side) are drawn.

use super::worker::FieldKind;
use super::{Field, PX_PER_PT, PdfView, Under};
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
    /// Open the file with the password typed (#2256).
    Unlock,
}

/// The selection's box colour: readable over a white page in either theme.
const SEL: u32 = 0x3B82F6;
const HIT: u32 = 0xFACC15;
const HIT_NOW: u32 = 0xF97316;
/// A fillable field's highlight and a required empty one's outline (#2255):
/// fixed colours, as the page is white in either theme.
const FIELD: u32 = 0x3B82F6;
const REQUIRED: u32 = 0xDC2626;
/// A combo box's open list: one row's height (logical px).
pub(crate) const OPTION_ROW: f32 = 20.;

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
            PdfCmd::Unlock => self.pdf_unlock(cx),
        }
    }

    pub(crate) fn pdf_body(
        &self,
        v: &PdfView,
        pal: Pal,
        bg: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(lock) = &v.lock {
            return self.pdf_password_prompt(v, lock, pal, bg, cx);
        }
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

    /// An encrypted PDF's password prompt (#2256), in the tab's body as a
    /// browser's viewer asks: the file's name, a masked field, Open, and
    /// after a password that does not open it, that it is incorrect.
    fn pdf_password_prompt(
        &self,
        v: &PdfView,
        lock: &super::PdfLock,
        pal: Pal,
        bg: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Bullets, one a character: the field draws them, never the text.
        let shown = Field {
            text: "\u{2022}".repeat(lock.field.text.chars().count()),
            ..lock.field.clone()
        };
        let field = self
            .pdf_field(&shown, shown.text.clone(), 240., "pdf-password", pal)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.pdf_password_press(cx);
                }),
            );
        v_flex()
            .flex_1()
            .min_h(px(0.))
            .size_full()
            .bg(bg)
            .text_color(pal.fg)
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .text_size(px(16.))
                    .font_weight(FontWeight::BOLD)
                    .child(format!("\u{2018}{}\u{2019} is protected", v.file_name())),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .child("Enter the password to open this PDF."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(field)
                    .child(self.pdf_button(
                        "Open".into(),
                        "pdf-unlock",
                        false,
                        PdfCmd::Unlock,
                        pal,
                        cx,
                    )),
            )
            .when(lock.wrong, |d| {
                d.child(
                    div()
                        .relative()
                        .text_size(px(12.))
                        .text_color(rgb_u(REQUIRED))
                        .child(super::host::PDF_WRONG_PASSWORD)
                        .child(probe(&self.probes, "pdf-wrong-password")),
                )
            })
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
            marks.extend(self.pdf_widgets(v, p, k));
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
        let overlays = self.pdf_overlays(v, &l, k, pal, cx);
        let content = div()
            .relative()
            .w(px(l.width))
            .h(px(l.height))
            .cursor(pointer_cursor(v))
            .children(pages)
            .children(overlays)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _w, cx| {
                    let Some(v) = this.active_pdf() else { return };
                    let (x, y) = content_point(v, ev.position);
                    let m = ev.modifiers;
                    this.pdf_page_press(x, y, ev.click_count, m.shift, m.control || m.platform, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                let Some(v) = this.active_pdf() else { return };
                let (x, y) = content_point(v, ev.position);
                if ev.pressed_button == Some(MouseButton::Left) {
                    if v.drag.is_some() || v.link_press.is_some() {
                        this.with_pdf(cx, |v| v.drag_at(x, y));
                    }
                    return;
                }
                // What the pointer is over: its cursor, tooltip and a
                // comment's pop-up (#2255). A redraw only when they change.
                if let Some(v) = this.active_pdf_mut()
                    && v.hover(x, y)
                {
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _w, cx| {
                    this.with_pdf(cx, PdfView::release_at)
                }),
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

impl Docxy {
    /// Page `p`'s form widgets (#2255): fillable ones lightly highlighted,
    /// a required empty one outlined red, and the focused one as an editor
    /// (its typed text, caret and selection; bullets for a password) or a
    /// focus ring. In the page's own coordinates.
    fn pdf_widgets(&self, v: &PdfView, p: usize, k: f32) -> Vec<AnyElement> {
        let mut out = Vec::new();
        for &(f, w) in &v.widgets[p] {
            let field = &v.form.fields[f];
            let widget = &field.widgets[w];
            if widget.hidden || !field.fillable() || widget.read_only {
                continue;
            }
            let r = widget.rect;
            let (x, y, ww, hh) = (
                r[0] * k,
                r[1] * k,
                ((r[2] - r[0]) * k).max(2.),
                ((r[3] - r[1]) * k).max(2.),
            );
            let focused = v.focus.as_ref().filter(|x| x.field == f && x.widget == w);
            let required = field.required && field.is_empty();
            let mut d = div()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(ww))
                .h(px(hh))
                .border_1()
                .border_color(if required {
                    rgb_u(REQUIRED)
                } else {
                    rgba(FIELD, 0.45)
                })
                .bg(rgba(FIELD, 0.10));
            if let Some(focus) = focused {
                d = d.border_2().border_color(rgb_u(SEL));
                if let Some(e) = &focus.edit {
                    let size = if field.font_size > 0. {
                        field.font_size
                    } else {
                        ((r[3] - r[1]) * 0.6).clamp(6., 14.)
                    } * k;
                    let shown = if field.password {
                        "\u{2022}".repeat(e.text.chars().count())
                    } else {
                        e.text.clone()
                    };
                    let caret = e.caret.min(shown.chars().count());
                    let (before, after): (String, String) = if e.sel {
                        (shown.clone(), String::new())
                    } else {
                        (
                            shown.chars().take(caret).collect(),
                            shown.chars().skip(caret).collect(),
                        )
                    };
                    d = d.bg(rgb_u(0xFFFFFF)).child(
                        div()
                            .size_full()
                            .px(px(2. * k))
                            .flex()
                            .when(field.multiline, |d| d.flex_wrap().items_start())
                            .when(!field.multiline, |d| d.items_center())
                            .overflow_hidden()
                            .text_size(px(size))
                            .line_height(px(size * 1.15))
                            .text_color(rgb_u(0x000000))
                            .child(
                                div()
                                    .when(e.sel && !shown.is_empty(), |d| d.bg(rgba(SEL, 0.35)))
                                    .child(before),
                            )
                            .when(!e.sel, |d| {
                                d.child(div().w(px(1.)).h(px(size)).bg(rgb_u(0x000000)))
                            })
                            .child(div().child(after)),
                    );
                }
                // A list box's option under the keys.
                if field.kind == FieldKind::List
                    && let Some(rows) = widget.list
                    && focus.cursor >= rows.top
                {
                    let top = rows.inset + (focus.cursor - rows.top) as f32 * rows.row;
                    if top + rows.row <= r[3] - r[1] {
                        d = d.child(
                            div()
                                .absolute()
                                .left_0()
                                .top(px(top * k))
                                .w_full()
                                .h(px(rows.row * k))
                                .border_1()
                                .border_color(rgb_u(SEL)),
                        );
                    }
                }
            }
            out.push(d.into_any_element());
        }
        out
    }

    /// What is drawn over the pages in the content's coordinates, so a page
    /// edge never cuts it (#2255): a combo box's open list, a comment's
    /// pop-up and the tooltip.
    fn pdf_overlays(
        &self,
        v: &PdfView,
        l: &super::Layout,
        k: f32,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut out = Vec::new();
        if let Some((x, y, w)) = combo_list(v, l, k) {
            let focus = v.focus.as_ref();
            let field = focus.and_then(|f| v.field(f.field));
            // The rows the list shows (#2255 r1): a window of the options
            // that follows the keys' cursor and scrolls by the wheel.
            let window = v.list_window();
            let items: Vec<AnyElement> = field
                .map(|f| f.options.clone())
                .unwrap_or_default()
                .into_iter()
                .enumerate()
                .skip(window.start)
                .take(window.len())
                .map(|(i, (_, text))| {
                    let on = focus.is_some_and(|f| f.cursor == i);
                    div()
                        .relative()
                        .h(px(OPTION_ROW))
                        .px_2()
                        .flex()
                        .items_center()
                        .text_size(px(12.))
                        .text_color(rgb_u(if on { 0xFFFFFF } else { 0x111827 }))
                        .when(on, |d| d.bg(rgb_u(SEL)))
                        .hover(|d| d.bg(rgba(SEL, 0.25)))
                        .child(text)
                        .child(probe(&self.probes, format!("pdf-option:{}", i + 1)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                cx.stop_propagation();
                                this.with_pdf(cx, |v| v.pick(i, false));
                            }),
                        )
                        .into_any_element()
                })
                .collect();
            out.push(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .w(px(w))
                    .bg(rgb_u(0xFFFFFF))
                    .border_1()
                    .border_color(rgb_u(0x6B7280))
                    .shadow_md()
                    .children(items)
                    .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, _w, cx| {
                        let dy = match ev.delta {
                            ScrollDelta::Lines(p) => p.y,
                            ScrollDelta::Pixels(p) => f32::from(p.y) / OPTION_ROW,
                        };
                        let rows = (-dy).round() as i32;
                        if rows != 0 {
                            cx.stop_propagation();
                            this.with_pdf(cx, |v| v.list_scroll(rows));
                        }
                    }))
                    .into_any_element(),
            );
        }
        if let Some(pop) = v.popup
            && let Some(note) = v.notes.get(pop.page).and_then(|n| n.get(pop.note))
            && let Some(pr) = l.pages.get(pop.page)
        {
            let x = pr.x + note.rect[2] * k + 8.;
            let y = pr.y + note.rect[1] * k;
            let head = match (&note.author, &note.date) {
                (Some(a), Some(d)) => format!("{a} · {d}"),
                (Some(a), None) => a.clone(),
                (None, Some(d)) => d.clone(),
                (None, None) => note.subtype.clone(),
            };
            out.push(
                div()
                    .id("pdf-popup")
                    .absolute()
                    .left(px(x.min((l.width - 268.).max(0.))))
                    .top(px(y))
                    .w(px(260.))
                    .p_2()
                    .bg(rgb_u(0xFEF9C3))
                    .border_1()
                    .border_color(rgb_u(0xCA8A04))
                    .shadow_md()
                    .text_size(px(12.))
                    .text_color(rgb_u(0x1F2937))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(head))
                    .child(div().mt_1().child(note.contents.clone()))
                    .child(probe(&self.probes, "pdf-popup"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_, _: &MouseDownEvent, _w, cx| cx.stop_propagation()),
                    )
                    .into_any_element(),
            );
        }
        if let Some(tip) = v.tooltip() {
            let (x, y) = v.pointer;
            out.push(
                div()
                    .absolute()
                    .left(px(x + 12.))
                    .top(px(y + 18.))
                    .max_w(px(360.))
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .bg(pal.panel)
                    .border_1()
                    .border_color(pal.border)
                    .text_color(pal.fg)
                    .text_size(px(12.))
                    .child(tip)
                    .child(probe(&self.probes, "pdf-tooltip"))
                    .into_any_element(),
            );
        }
        out
    }
}

/// Where the focused combo box's open list goes (content px): left, top
/// and width; its rows are [`PdfView::list_window`].
pub(crate) fn combo_list(v: &PdfView, l: &super::Layout, k: f32) -> Option<(f32, f32, f32)> {
    let focus = v.focus.as_ref().filter(|f| f.open)?;
    let field = v
        .field(focus.field)
        .filter(|f| f.kind == FieldKind::Combo)?;
    let widget = field.widgets.get(focus.widget)?;
    let pr = l.pages.get(widget.page?)?;
    let r = widget.rect;
    Some((
        pr.x + r[0] * k,
        pr.y + r[3] * k,
        ((r[2] - r[0]) * k).max(80.),
    ))
}

/// The pointer over the pages: a hand over links, comments and the fields
/// a click fills, an I-beam over text fields and text, an arrow over a
/// read-only field.
fn pointer_cursor(v: &PdfView) -> CursorStyle {
    match v.under {
        Under::Link(..) | Under::Note(..) => CursorStyle::PointingHand,
        Under::Widget(f, _) => match v.field(f) {
            Some(field) if !field.fillable() => CursorStyle::Arrow,
            Some(field)
                if field.kind == FieldKind::Text
                    || (field.kind == FieldKind::Combo && field.edit) =>
            {
                CursorStyle::IBeam
            }
            _ => CursorStyle::PointingHand,
        },
        Under::Nothing => CursorStyle::IBeam,
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
