//! The Help tab drawn (#1855): the contents or search hits on the left,
//! under Back/Forward and the search box, and the page on the right, one
//! child of the scrolled body per laid-out row ([`lay::lay`]), so a row's
//! index is what an `#anchor` scrolls to. Colours come from the theme
//! (`Pal`), so the page follows Light and Dark.

use super::host::picture_part;
use super::lay::{self, Item, Row, Run};
use super::{HelpView, ScrollTo};
use crate::*;
use helpdocs::md::Align;

/// The left pane's width, px.
const LEFT_W: f32 = 250.;

/// The code blocks' and inline code's family.
const CODE_FONT: &str = if cfg!(windows) {
    "Consolas"
} else if cfg!(target_os = "macos") {
    "Menlo"
} else {
    "DejaVu Sans Mono"
};

impl Docxy {
    /// The Help tab's body.
    pub(crate) fn help_body(
        &self,
        tab: &DocTab,
        v: &HelpView,
        pal: Pal,
        bg: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // A scroll a navigation asked for: gpui places an item in this
        // frame's prepaint, from this frame's layout of the page.
        match v.scroll_to.take() {
            Some(ScrollTo::Top) => v.scroll.set_offset(point(px(0.), px(0.))),
            Some(ScrollTo::Block(i)) => v.scroll.scroll_to_top_of_item(i),
            None => {}
        }
        h_flex()
            .flex_1()
            .min_h(px(0.))
            .size_full()
            .bg(bg)
            .text_color(pal.fg)
            .child(self.help_left(v, pal, cx))
            .child(self.help_page(tab, v, pal, cx))
            .into_any_element()
    }

    fn help_left(&self, v: &HelpView, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let button = |label: &'static str, probe_name: &'static str, on: bool, forward: bool| {
            div()
                .relative()
                .px_2()
                .py_1()
                .rounded_sm()
                .border_1()
                .border_color(pal.border)
                .text_size(px(12.))
                .when(on, |d| d.cursor_pointer().hover(|d| d.bg(pal.hover)))
                .when(!on, |d| d.text_color(pal.dim))
                .child(label)
                .child(probe(&self.probes, probe_name))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                        cx.stop_propagation();
                        this.help_history(forward, cx);
                    }),
                )
        };
        let toolbar = h_flex()
            .gap_1()
            .p_2()
            .child(button(
                "\u{25C0} Back",
                "help-back",
                !v.back.is_empty(),
                false,
            ))
            .child(button(
                "Forward \u{25B6}",
                "help-forward",
                !v.forward.is_empty(),
                true,
            ));
        let list: AnyElement = if v.query.trim().is_empty() {
            self.help_contents(v, pal, cx)
        } else {
            self.help_hits(v, pal, cx)
        };
        v_flex()
            .w(px(LEFT_W))
            .flex_none()
            .h_full()
            .min_h(px(0.))
            .bg(pal.panel)
            .border_r_1()
            .border_color(pal.border)
            .child(toolbar)
            .child(self.help_search_box(v, pal, cx))
            .child(
                div()
                    .id("help-left-list")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .py_1()
                    .child(list),
            )
            .into_any_element()
    }

    fn help_search_box(&self, v: &HelpView, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let focused = v.search_focused;
        let text: AnyElement = if v.query.is_empty() && !focused {
            div()
                .text_color(pal.dim)
                .child("Search the manual")
                .into_any_element()
        } else {
            let caret = v.caret.min(v.query.chars().count());
            let (before, after): (String, String) = (
                v.query.chars().take(caret).collect(),
                v.query.chars().skip(caret).collect(),
            );
            h_flex()
                .when(v.sel, |d| d.bg(pal.sel))
                .child(before)
                .when(focused && !v.sel, |d| {
                    d.child(div().w(px(1.)).h(px(14.)).bg(rgb(accent::brand())))
                })
                .child(after)
                .into_any_element()
        };
        div()
            .mx_2()
            .mb_2()
            .relative()
            .h(px(26.))
            .px_2()
            .flex()
            .items_center()
            .rounded_sm()
            .border_1()
            .border_color(if focused {
                rgb(accent::brand()).into()
            } else {
                pal.border
            })
            .bg(cx.theme().background)
            .text_size(px(13.))
            .cursor_text()
            .child(text)
            .child(probe(&self.probes, "help-search"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    this.help_search_press(cx);
                }),
            )
            .into_any_element()
    }

    fn help_contents(&self, v: &HelpView, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        let m = &v.bundle.manual;
        let mut col = v_flex();
        for e in &m.contents {
            let title = m.page(&e.id).map_or(e.id.clone(), |p| p.title.clone());
            let current = e.id == v.at.page;
            let id = e.id.clone();
            col = col.child(
                div()
                    .relative()
                    .pl(px(10. + 14. * e.depth as f32))
                    .pr_2()
                    .py(px(3.))
                    .text_size(px(13.))
                    .cursor_pointer()
                    .hover(|d| d.bg(pal.hover))
                    .when(current, |d| {
                        d.bg(pal.sel)
                            .text_color(rgb(accent::brand()))
                            .font_weight(FontWeight::BOLD)
                    })
                    .child(title)
                    .child(probe(&self.probes, format!("help-contents:{}", e.id)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation();
                            this.help_contents_press(&id, cx);
                        }),
                    ),
            );
        }
        col.into_any_element()
    }

    fn help_hits(&self, v: &HelpView, pal: Pal, cx: &mut Context<Self>) -> AnyElement {
        if v.hits.is_empty() {
            return div()
                .px_3()
                .py_2()
                .text_size(px(13.))
                .text_color(pal.dim)
                .child("No pages match")
                .into_any_element();
        }
        let mut col = v_flex();
        for (i, h) in v.hits.iter().enumerate() {
            col = col.child(
                v_flex()
                    .relative()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(|d| d.bg(pal.hover))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::BOLD)
                            .child(h.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(pal.dim)
                            .child(h.snippet.clone()),
                    )
                    .child(probe(&self.probes, format!("help-hit:{i}")))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                            cx.stop_propagation();
                            this.help_hit_press(i, cx);
                        }),
                    ),
            );
        }
        col.into_any_element()
    }

    fn help_page(
        &self,
        tab: &DocTab,
        v: &HelpView,
        pal: Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let page = v.page();
        let rows = lay::lay(&page.blocks, &v.bundle);
        let mut seen = std::collections::HashSet::new();
        let mut body = div()
            .id("help-body")
            .size_full()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&v.scroll)
            .px(px(28.))
            .pt(px(16.));
        for row in &rows {
            body = body.child(self.help_row(tab, v, row, pal, &mut seen, cx));
        }
        // Room below the last row, so every heading can reach the top.
        let room = f32::from(v.scroll.bounds().size.height).max(200.);
        body = body.child(div().h(px(room)));
        div()
            .relative()
            .flex_1()
            .h_full()
            .min_h(px(0.))
            .child(probe(&self.probes, "help-page"))
            .child(body)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(12.))
                    .child(
                        gpui_component::scroll::Scrollbar::vertical(&v.scroll)
                            .scrollbar_show(gpui_component::scroll::ScrollbarShow::Always),
                    ),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _w, cx| this.help_body_press(cx)),
            )
            .into_any_element()
    }

    fn help_row(
        &self,
        tab: &DocTab,
        v: &HelpView,
        row: &Row,
        pal: Pal,
        seen: &mut std::collections::HashSet<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match row {
            Row::Heading { px: size, runs, .. } => div()
                .pt(px(size * 0.6))
                .pb(px(6.))
                .font_weight(FontWeight::BOLD)
                .child(self.help_runs(runs, *size, pal, seen, cx))
                .into_any_element(),
            Row::Para(runs) => div()
                .pb(px(10.))
                .child(self.help_runs(runs, lay::BODY_PX, pal, seen, cx))
                .into_any_element(),
            Row::List { items, .. } => {
                let mut col = v_flex().pb(px(8.));
                for item in items {
                    col = col.child(self.help_item(tab, v, item, pal, seen, cx));
                }
                col.into_any_element()
            }
            Row::Code { lines, .. } => {
                let mut col = v_flex()
                    .mb(px(10.))
                    .p(px(8.))
                    .rounded_sm()
                    .bg(pal.panel)
                    .border_1()
                    .border_color(pal.border)
                    .font_family(CODE_FONT)
                    .text_size(px(13.));
                for l in lines {
                    // An empty line keeps its height.
                    col = col.child(div().min_h(px(16.)).child(l.clone()));
                }
                col.into_any_element()
            }
            Row::Table {
                cols,
                align,
                header,
                rows,
            } => {
                let cells = |this: &Self,
                             cells: &[Vec<Run>],
                             head: bool,
                             seen: &mut std::collections::HashSet<usize>,
                             cx: &mut Context<Self>| {
                    let mut r = h_flex().border_b_1().border_color(pal.border);
                    if head {
                        r = r.bg(pal.panel).font_weight(FontWeight::BOLD);
                    }
                    for (c, runs) in cells.iter().enumerate().take(*cols) {
                        let cell = h_flex().flex_1().min_w(px(0.)).px_2().py_1();
                        let cell = match align.get(c) {
                            Some(Align::Center) => cell.justify_center(),
                            Some(Align::Right) => cell.justify_end(),
                            _ => cell.justify_start(),
                        };
                        r = r.child(cell.child(this.help_runs(runs, lay::BODY_PX, pal, seen, cx)));
                    }
                    r
                };
                let mut t = v_flex()
                    .mb(px(12.))
                    .border_1()
                    .border_color(pal.border)
                    .child(cells(self, header, true, seen, cx));
                for r in rows {
                    t = t.child(cells(self, r, false, seen, cx));
                }
                t.into_any_element()
            }
            Row::Image { src, alt, found } => {
                let image = found
                    .then(|| v.bundle.image(src))
                    .flatten()
                    .and_then(|bytes| {
                        tab.pictures
                            .borrow_mut()
                            .file_image(&picture_part(v.editor()), src, bytes)
                    });
                match image {
                    Some(image) => div()
                        .pb(px(10.))
                        .child(img(image).max_w(px(720.)).object_fit(ObjectFit::ScaleDown))
                        .into_any_element(),
                    None => div()
                        .mb(px(10.))
                        .p(px(12.))
                        .border_1()
                        .border_color(pal.border)
                        .text_color(pal.dim)
                        .text_size(px(13.))
                        .child(if alt.is_empty() {
                            format!("[{src}]")
                        } else {
                            alt.clone()
                        })
                        .into_any_element(),
                }
            }
        }
    }

    fn help_item(
        &self,
        tab: &DocTab,
        v: &HelpView,
        item: &Item,
        pal: Pal,
        seen: &mut std::collections::HashSet<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut body = v_flex().flex_1().min_w(px(0.)).child(self.help_runs(
            &item.runs,
            lay::BODY_PX,
            pal,
            seen,
            cx,
        ));
        for child in &item.children {
            body = body.child(
                div()
                    .pt(px(4.))
                    .child(self.help_row(tab, v, child, pal, seen, cx)),
            );
        }
        h_flex()
            .items_start()
            .pb(px(3.))
            .child(
                div()
                    .w(px(26.))
                    .flex_none()
                    .text_size(px(lay::BODY_PX))
                    .text_color(pal.dim)
                    .child(item.marker.clone()),
            )
            .child(body)
            .into_any_element()
    }

    /// Inline runs, wrapped at spaces. A link's first piece carries its
    /// `help-link:<n>` probe; every piece of it follows it on a press.
    fn help_runs(
        &self,
        runs: &[Run],
        size: f32,
        pal: Pal,
        seen: &mut std::collections::HashSet<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut line = h_flex().flex_wrap().items_center().text_size(px(size));
        for run in runs {
            let link = run.link();
            let pieces: Vec<AnyElement> = match run {
                Run::Text {
                    text,
                    bold,
                    italic,
                    code,
                    ..
                } => text
                    .split_inclusive(' ')
                    .map(|piece| {
                        div()
                            .child(piece.to_string())
                            .when(*bold, |d| d.font_weight(FontWeight::BOLD))
                            .when(*italic, |d| d.italic())
                            .when(*code, |d| {
                                d.font_family(CODE_FONT)
                                    .bg(pal.panel)
                                    .px(px(2.))
                                    .rounded_sm()
                            })
                            .into_any_element()
                    })
                    .collect(),
                Run::Keys { caps, .. } => {
                    let mut chord = h_flex().gap(px(2.)).px(px(1.));
                    for (i, cap) in caps.iter().enumerate() {
                        if i > 0 {
                            chord = chord.child(div().text_color(pal.dim).child("+"));
                        }
                        chord = chord.child(
                            div()
                                .px(px(5.))
                                .rounded(px(3.))
                                .border_1()
                                .border_b_2()
                                .border_color(pal.border)
                                .bg(pal.panel)
                                .text_size(px((size - 2.).max(10.)))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(cap.clone()),
                        );
                    }
                    vec![chord.into_any_element()]
                }
            };
            for piece in pieces {
                let Some(n) = link else {
                    line = line.child(piece);
                    continue;
                };
                let first = seen.insert(n);
                line = line.child(
                    div()
                        .relative()
                        .text_color(rgb(LINK))
                        .underline()
                        .cursor_pointer()
                        .child(piece)
                        .when(first, |d| {
                            d.child(probe(&self.probes, format!("help-link:{n}")))
                        })
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _w, cx| {
                                cx.stop_propagation();
                                this.help_link_press(n, cx);
                            }),
                        ),
                );
            }
        }
        line.into_any_element()
    }
}
