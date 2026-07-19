//! The right reading pane: headers plus the rendered message body for the
//! selected (opened) message. Body rendering itself (HTML→styled-text, or
//! plain-text wrapping) lives in `mailcore::htmlrender` so it's testable
//! without ratatui; this module's job is headers, the loading/no-body
//! placeholders, mapping `mailcore`'s neutral `StyledLine`/`StyledSpan` onto
//! ratatui's `Line`/`Span`/`Style`, and — this task — a deterministic
//! row-based body layout with vertical scroll and a bordered fallback box
//! reserved for each inline image (`StyledLine::image`).
//!
//! Body loading itself (cache-hit vs. `SyncCommand::FetchBody` vs.
//! `SyncEvent::BodyReady`) is `App::open_message`/`reload_body` (see
//! `app.rs`) and `main::drain_events` — this module only reads the result
//! (`app.body`/`app.body_loading`).

use crate::app::{App, Pane};
use crate::ui::border_style;

use mailcore::htmlrender::{self, ImageRef, StyledLine, StyledSpan};
use mailcore::store::MessageRow;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// Rows reserved in the reader for one inline image band. Task 6 paints real
/// pixels into this space when the fetch succeeds; until then (and whenever
/// it fails/is unsupported/remote), `draw_image_fallback_rect` fills it.
pub const IMAGE_BOX_ROWS: usize = 10;

/// One inline image's placement: the absolute body-row of its band's first
/// row, plus the ref (owned — the layout borrows nothing from `app`).
struct ImgBox {
    row: usize,
    img: ImageRef,
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::Reading;
    let block = Block::default()
        .title("Reading Pane")
        .borders(Borders::ALL)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(m) = selected_message(app) else {
        f.render_widget(
            Paragraph::new("(no message selected — press Enter on a message)"),
            inner,
        );
        return;
    };

    // Fixed header (From/Subject/Received + blank), then the scrolling body.
    let header = header_lines(m);
    let header_h = (header.len() as u16 + 1).min(inner.height);
    let header_area = Rect {
        height: header_h,
        ..inner
    };
    let body_area = Rect {
        y: inner.y + header_h,
        height: inner.height.saturating_sub(header_h),
        ..inner
    };
    f.render_widget(Paragraph::new(header), header_area);

    // Build the owned layout (render_body returns Vec<StyledLine>, owned).
    let styled = render_body(app, body_area.width as usize);
    let (lines, images) = body_layout(styled);
    let vh = body_area.height as usize;
    app.reading_content_rows = lines.len();
    app.reading_viewport = vh;
    let scroll = app.reading_scroll.min(lines.len().saturating_sub(vh));

    // Text: render the visible window as one Paragraph, no re-wrap (lines
    // already fit width; blank lines hold the image bands' space).
    let visible: Vec<Line<'static>> = lines.iter().skip(scroll).take(vh).cloned().collect();
    f.render_widget(Paragraph::new(visible), body_area);

    // Images: crop each band to the visible window (docxy's draw_images
    // math, main.rs:3017-3065). In THIS task every box is the fallback;
    // Task 6 paints pixels first and only falls back here.
    for ib in &images {
        let wtop = scroll.saturating_sub(ib.row);
        let wbot = (scroll + vh).saturating_sub(ib.row).min(IMAGE_BOX_ROWS);
        if wbot <= wtop {
            continue;
        }
        let y = body_area.y + (ib.row + wtop - scroll) as u16;
        let rect = Rect {
            x: body_area.x,
            y,
            width: body_area.width,
            height: (wbot - wtop) as u16,
        };
        draw_image_fallback_rect(f, rect, &ib.img);
    }
}

/// The message named by `App::selected_msg`, if it's still in the currently
/// loaded (visible-folder) message list.
fn selected_message(app: &App) -> Option<&MessageRow> {
    let id = app.selected_msg.as_deref()?;
    app.messages.iter().find(|m| m.id == id)
}

fn header_lines(m: &MessageRow) -> Vec<Line<'static>> {
    vec![
        Line::from(format!("From: {} <{}>", m.from_name, m.from_addr)),
        Line::from(format!("Subject: {}", m.subject)),
        Line::from(format!("Received: {}", m.received_at)),
    ]
}

/// The opened message's body as `StyledLine`s (HTML or plain), mirroring the
/// `loading…`/no-body placeholders: `loading…` while a `FetchBody` is
/// outstanding (`App::body_loading`), the HTML- or plain-text-rendered body
/// once `App::body` has it, or a placeholder if neither (the store lookup
/// itself failed — see `App::reload_body`). Returns the neutral
/// `Vec<StyledLine>` (so image markers survive) rather than ratatui lines —
/// `body_layout` does that mapping.
fn render_body(app: &App, width: usize) -> Vec<StyledLine> {
    match (&app.body, app.body_loading) {
        (_, true) => vec![StyledLine {
            spans: vec![StyledSpan {
                text: "loading…".into(),
                ..Default::default()
            }],
            ..Default::default()
        }],
        (Some(b), false) if b.content_type.eq_ignore_ascii_case("html") => {
            htmlrender::render_html(&b.content, width)
        }
        (Some(b), false) => htmlrender::render_text(&b.content, width),
        (None, false) => vec![StyledLine {
            spans: vec![StyledSpan {
                text: "(no body)".into(),
                ..Default::default()
            }],
            ..Default::default()
        }],
    }
}

/// Consumes an owned `Vec<StyledLine>` (as returned by `render_body`) into an
/// owned, flat `Vec<Line<'static>>` plus the absolute-row placement of each
/// inline image band: a text line becomes one `to_ratatui_line`; an image
/// marker becomes `IMAGE_BOX_ROWS` blank lines (reserving the band's space)
/// plus an `ImgBox` recording the band's first row. Owning everything means
/// this borrows nothing from `app`, so `app.reading_*` can be assigned right
/// after building it — and an absolute-row image box can still be cropped
/// correctly even when its top row is scrolled above the viewport.
fn body_layout(styled: Vec<StyledLine>) -> (Vec<Line<'static>>, Vec<ImgBox>) {
    let mut out_lines: Vec<Line<'static>> = Vec::new();
    let mut images: Vec<ImgBox> = Vec::new();
    for line in &styled {
        if let Some(img) = &line.image {
            images.push(ImgBox {
                row: out_lines.len(),
                img: img.clone(),
            });
            for _ in 0..IMAGE_BOX_ROWS {
                out_lines.push(Line::from(""));
            }
        } else {
            out_lines.push(to_ratatui_line(line));
        }
    }
    (out_lines, images)
}

/// Renders the bordered fallback box for one inline image band, captioned
/// with its `alt` text (or `[image]` if there isn't one). Reused unchanged by
/// Task 6 as the fallback for a cid fetch that hasn't landed yet/failed/is
/// unsupported.
fn draw_image_fallback_rect(f: &mut Frame, rect: Rect, img: &ImageRef) {
    let label = if img.alt.is_empty() {
        "[image]".to_string()
    } else {
        format!("[image: {}]", img.alt)
    };
    f.render_widget(
        Paragraph::new(label).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(Color::DarkGray)),
        ),
        rect,
    );
}

/// Maps one `StyledLine` to a ratatui `Line`: `indent` becomes that many
/// `htmlrender::INDENT_SPACES`-wide groups of leading spaces (the same
/// figure `htmlrender` already subtracted from its wrap width, so this
/// plus the wrapped text still fits within what `render_body` asked for).
fn to_ratatui_line(line: &StyledLine) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    let indent = line.indent as usize * htmlrender::INDENT_SPACES;
    if indent > 0 {
        spans.push(Span::raw(" ".repeat(indent)));
    }
    spans.extend(line.spans.iter().map(to_ratatui_span));
    Line::from(spans)
}

/// Maps one `StyledSpan` to a ratatui `Span`: bold/italic/underline become
/// the matching `Modifier`; a link (footnote reference or the footnote
/// appendix line itself) renders in cyan so it stands out from plain body
/// text.
fn to_ratatui_span(span: &StyledSpan) -> Span<'static> {
    let mut style = Style::default();
    if span.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if span.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if span.underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if span.link.is_some() {
        style = style.fg(Color::Cyan);
    }
    Span::styled(span.text.clone(), style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcore::graph::model::Body;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn renders_an_image_fallback_box_for_an_inline_cid_image() {
        let mut app = App::for_test_with_seeded_store();
        app.store
            .put_body(
                "m1",
                &Body {
                    content_type: "html".into(),
                    content: r#"<p>hello</p><img src="cid:logo123" alt="Logo"><p>bye</p>"#.into(),
                },
            )
            .expect("seed body");
        app.open_message("m1");

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| {
            let area = f.area();
            draw(f, &mut app, area);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("[image: Logo]"));
    }
}
