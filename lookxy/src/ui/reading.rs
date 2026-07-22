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

use mailcore::graph::model::MasterCategory;
use mailcore::htmlrender::{self, ImageRef, ImageSource, StyledLine, StyledSpan};
use mailcore::store::MessageRow;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::Protocol;
use ratatui_image::{Image, Resize};
use std::collections::HashMap;

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

    // Fixed header (From/Subject/Received, an optional meeting-invite banner,
    // + blank), then the scrolling body.
    let mut header = header_lines(m, &app.master_categories);
    if m.is_meeting_request {
        header.push(Line::from(
            "📅 Meeting invite — [A]ccept  [D]ecline  [T]entative",
        ));
    }
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
    let focused_url = app
        .focused_link
        .and_then(|i| app.body_links.get(i))
        .map(|l| l.url.clone());
    let styled = render_body(app, body_area.width as usize);
    let (lines, images, links) = body_layout(styled, focused_url.as_deref());
    app.body_links = links;
    app.reading_body_rect = body_area;
    let vh = body_area.height as usize;
    app.reading_content_rows = lines.len();
    app.reading_viewport = vh;
    let scroll = app.reading_scroll.min(lines.len().saturating_sub(vh));
    app.reading_scroll = scroll; // keep the stored scroll at the effective value

    // Text: render the visible window as one Paragraph, no re-wrap (lines
    // already fit width; blank lines hold the image bands' space).
    let visible: Vec<Line<'static>> = lines.iter().skip(scroll).take(vh).cloned().collect();
    f.render_widget(Paragraph::new(visible), body_area);

    // Images: crop each band to the visible window (docxy's draw_images
    // math, main.rs:3017-3065). Real pixels are tried first (when a
    // graphics-capable `Picker` is present and the bytes have arrived);
    // anything else — no picker, bytes not yet fetched, a decode/encode
    // failure, or a Remote/Unsupported source — falls back to the box.
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
        // Resolve bytes + a stable cache key from the source (immutable
        // reads first, so the split-borrow call below never overlaps
        // `&app.picker`/`&mut app.image_protocols`).
        let resolved: Option<(String, &[u8])> = match &ib.img.src {
            ImageSource::Cid(c) => app
                .inline_images
                .get(c)
                .map(|b| (format!("cid:{c}"), b.as_slice())),
            ImageSource::Data { bytes, .. } => Some((data_image_key(bytes), bytes.as_slice())),
            _ => None, // Remote / Unsupported → box
        };
        let painted = match (&app.picker, resolved) {
            (Some(picker), Some((key, bytes))) => {
                paint_inline_image(f, picker, &mut app.image_protocols, &key, bytes, rect)
            }
            _ => false,
        };
        if !painted {
            draw_image_fallback_rect(f, rect, &ib.img);
        }
    }

    // Focused-link URL strip: the full URL along the reader's bottom edge,
    // wrapped across up to 3 rows (Enter opens it — with a warning).
    if let Some(url) = focused_url {
        let rows = wrap_url(&url, body_area.width as usize, 3);
        let h = (rows.len() as u16).min(body_area.height);
        if h > 0 {
            let strip = Rect {
                x: body_area.x,
                y: body_area.y + body_area.height - h,
                width: body_area.width,
                height: h,
            };
            f.render_widget(Clear, strip);
            f.render_widget(
                Paragraph::new(rows).style(Style::new().fg(Color::Black).bg(Color::Cyan)),
                strip,
            );
        }
    }
}

/// Wraps `url` into at most `max_rows` lines of `width` columns, char-breaking
/// (a URL has no spaces to wrap on); an ellipsis marks truncation if it doesn't
/// fit. The first row is prefixed with a small link glyph.
fn wrap_url(url: &str, width: usize, max_rows: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let text = format!("\u{1f517} {url}"); // 🔗 url
    let chars: Vec<char> = text.chars().collect();
    let chunks: Vec<&[char]> = chars.chunks(width).collect();
    let mut rows: Vec<Line<'static>> = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        if i == max_rows - 1 && chunks.len() > max_rows {
            // Last allowed row and there's more — end with an ellipsis.
            let mut s: String = chunk.iter().take(width.saturating_sub(1)).collect();
            s.push('\u{2026}');
            rows.push(Line::from(s));
            break;
        }
        rows.push(Line::from(chunk.iter().collect::<String>()));
        if i + 1 == max_rows {
            break;
        }
    }
    rows
}

/// A stable cache key for a `data:` URI image, derived from the bytes'
/// *content* rather than their length — two different images that happen to
/// encode to the same byte count (common for e.g. same-size PNGs) must not
/// collide on the same key, or the second one paints with the first's
/// cached protocol. Not cryptographic; `DefaultHasher` is only asked to tell
/// distinct byte strings apart within one reading-pane session.
fn data_image_key(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    format!("data:{:016x}", h.finish())
}

/// Tries to paint `bytes` (the image source resolved from `key`'s
/// `ImageSource`) as real pixels into `rect`, using `picker`'s detected
/// graphics protocol and `cache` to avoid re-decoding/re-encoding every
/// frame. Returns `false` (caller falls back to the bordered box) on a
/// decode or encode failure; `true` once the pixels are rendered.
///
/// Mirrors docxy's `encode` closure (`main.rs:2961-2974`) and paint call
/// (`main.rs:3056`) — for email there's no mid-image crop-on-scroll: the
/// whole decoded image is re-scaled to fit `rect` via `Resize::Fit(None)`,
/// which is what a partially-scrolled band ends up doing here too (see the
/// module doc comment).
fn paint_inline_image(
    f: &mut Frame,
    picker: &Picker,
    cache: &mut HashMap<String, Protocol>,
    key: &str,
    bytes: &[u8],
    rect: Rect,
) -> bool {
    let cache_key = format!("{key}#{}x{}", rect.width, rect.height);
    if !cache.contains_key(&cache_key) {
        // Inline image bytes are attacker-controlled (a `data:` URI in the
        // HTML, or a sender's cid attachment): decode under bounded limits
        // so a small "decompression bomb" declaring huge pixel dimensions
        // can't make the decoder allocate an unbounded buffer and OOM/hang
        // the TUI. Any limit violation (or any other decode error) just
        // falls back to the bordered box like every other unsupported case.
        let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes));
        reader = match reader.with_guessed_format() {
            Ok(r) => r,
            Err(_) => return false,
        };
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(10_000);
        limits.max_image_height = Some(10_000);
        limits.max_alloc = Some(256 * 1024 * 1024); // 256 MiB decoded ceiling
        reader.limits(limits);
        let Ok(img) = reader.decode() else {
            return false;
        };
        let Ok(proto) = picker.new_protocol(img, rect, Resize::Fit(None)) else {
            return false;
        };
        cache.insert(cache_key.clone(), proto);
    }
    f.render_widget(Image::new(cache.get(&cache_key).unwrap()), rect);
    true
}

/// The message named by `App::selected_msg`, resolved via the app's shared
/// accessor (flat list or, in threaded mode, the built threads).
fn selected_message(app: &App) -> Option<&MessageRow> {
    app.selected_message_row()
}

fn header_lines(m: &MessageRow, master: &[MasterCategory]) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(format!("From: {} <{}>", m.from_name, m.from_addr)),
        Line::from(format!("Subject: {}", m.subject)),
        Line::from(format!("Received: {}", m.received_at)),
    ];
    if !m.categories.is_empty() {
        let mut spans: Vec<Span<'static>> = vec![Span::raw("Categories: ")];
        for name in &m.categories {
            spans.push(Span::styled(
                format!("[{name}] "),
                Style::default().fg(crate::ui::categories::color_for(master, name)),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
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
fn body_layout(
    styled: Vec<StyledLine>,
    focused_url: Option<&str>,
) -> (Vec<Line<'static>>, Vec<ImgBox>, Vec<BodyLink>) {
    let mut out_lines: Vec<Line<'static>> = Vec::new();
    let mut images: Vec<ImgBox> = Vec::new();
    let mut raw_links: Vec<BodyLink> = Vec::new();
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
            collect_line_links(line, out_lines.len(), &mut raw_links);
            out_lines.push(to_ratatui_line(line, focused_url));
        }
    }
    (out_lines, images, dedup_continuations(raw_links))
}

/// A navigable link in the rendered body: its screen row (index into the
/// laid-out lines), start column and width (for highlighting/mouse hit-testing),
/// and target URL.
#[derive(Debug, Clone)]
pub struct BodyLink {
    pub line: usize,
    pub col: u16,
    pub width: u16,
    pub url: String,
}

/// Collects one `BodyLink` per contiguous same-link run on `line`, at output
/// `row`. Column tracking mirrors `to_ratatui_line` (leading indent spaces,
/// then span texts in order).
fn collect_line_links(line: &StyledLine, row: usize, out: &mut Vec<BodyLink>) {
    let mut col = (line.indent as usize * htmlrender::INDENT_SPACES) as u16;
    let mut run: Option<BodyLink> = None;
    for span in &line.spans {
        let slen = span.text.chars().count() as u16;
        match &span.link {
            Some(url) => match &mut run {
                Some(r) if r.url == *url => r.width += slen,
                _ => {
                    if let Some(r) = run.take() {
                        out.push(r);
                    }
                    run = Some(BodyLink {
                        line: row,
                        col,
                        width: slen,
                        url: url.clone(),
                    });
                }
            },
            None => {
                if let Some(r) = run.take() {
                    out.push(r);
                }
            }
        }
        col += slen;
    }
    if let Some(r) = run.take() {
        out.push(r);
    }
}

/// Collapses a wrapped anchor — the same URL continuing on the next line — into
/// a single navigable target at its first line, so a long hard-wrapped URL is
/// one Ctrl-arrow stop rather than many.
fn dedup_continuations(raw: Vec<BodyLink>) -> Vec<BodyLink> {
    let mut out: Vec<BodyLink> = Vec::new();
    for link in raw {
        if let Some(last) = out.last() {
            if last.url == link.url && last.line + 1 == link.line {
                continue;
            }
        }
        out.push(link);
    }
    out
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
fn to_ratatui_line(line: &StyledLine, focused_url: Option<&str>) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    let indent = line.indent as usize * htmlrender::INDENT_SPACES;
    if indent > 0 {
        spans.push(Span::raw(" ".repeat(indent)));
    }
    spans.extend(line.spans.iter().map(|s| to_ratatui_span(s, focused_url)));
    Line::from(spans)
}

/// Maps one `StyledSpan` to a ratatui `Span`: bold/italic/underline become the
/// matching `Modifier`; a link renders blue, and the currently-focused link
/// (its URL == `focused_url`) is additionally reversed so it stands out.
fn to_ratatui_span(span: &StyledSpan, focused_url: Option<&str>) -> Span<'static> {
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
        style = style.fg(Color::Blue);
    }
    if focused_url.is_some() && span.link.as_deref() == focused_url {
        style = style.add_modifier(Modifier::REVERSED);
    }
    Span::styled(span.text.clone(), style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mailcore::graph::model::Body;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn linked_spans_render_blue() {
        let span = StyledSpan {
            text: "click".into(),
            link: Some("https://x".into()),
            ..Default::default()
        };
        assert_eq!(to_ratatui_span(&span, None).style.fg, Some(Color::Blue));
    }

    #[test]
    fn drawing_a_two_link_body_populates_body_links() {
        let mut app = App::for_test_with_seeded_store();
        app.store
            .put_body(
                "m1",
                &Body {
                    content_type: "html".into(),
                    content:
                        r#"<p><a href="https://a">one</a> and <a href="https://b">two</a></p>"#
                            .into(),
                },
            )
            .expect("seed body");
        app.open_message("m1");
        assert!(app.body_links.is_empty()); // cleared until drawn
        let mut term = Terminal::new(TestBackend::new(80, 20)).unwrap();
        term.draw(|f| draw(f, &mut app, f.area())).unwrap();
        assert_eq!(app.body_links.len(), 2);
        assert_eq!(app.body_links[0].url, "https://a");
        assert_eq!(app.body_links[1].url, "https://b");
        assert_eq!(app.focused_link, None);
    }

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

    /// With `app.picker == None` (every test `App`'s default — see
    /// `App::picker`'s doc comment), a `cid:` image whose bytes ARE already
    /// cached must still draw the fallback box rather than panicking trying
    /// to paint pixels with no graphics capability. This is the one path
    /// automated tests can exercise — real pixel output needs a real
    /// graphics-capable terminal (verified manually, as docxy/xlsxy do).
    #[test]
    fn cid_image_without_graphics_capability_draws_the_box() {
        let mut app = App::for_test_with_seeded_store();
        assert!(app.picker.is_none());
        app.store
            .put_body(
                "m1",
                &Body {
                    content_type: "html".into(),
                    content: r#"<img src="cid:logo" alt="Logo">"#.into(),
                },
            )
            .expect("seed body");
        app.inline_images.insert("logo".into(), vec![0, 1, 2]); // bytes present but no Picker
        app.open_message("m1");
        app.inline_images.insert("logo".into(), vec![0, 1, 2]); // re-add (open cleared it)

        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("[image: Logo]"));
    }

    /// Two `data:` images of equal *byte length* but different content must
    /// not collide on the cache key — the old `format!("data:{}", bytes.len())`
    /// key would let a second same-length image paint over the first's
    /// cached protocol. Keying on content (a hash of the bytes) instead
    /// keeps them distinct, while identical bytes still share one key.
    #[test]
    fn reader_shows_category_chips() {
        use mailcore::graph::model::{MasterCategory, Message, Recipient};
        let mut app = App::for_test_with_seeded_store();
        app.master_categories = vec![MasterCategory {
            display_name: "Work".into(),
            color: "preset0".into(),
        }];
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "mc".into(),
                    conversation_id: "c1".into(),
                    subject: "Budget".into(),
                    from: Recipient {
                        name: "Al".into(),
                        address: "a@x".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-19T10:00:00Z".into(),
                    sent: "".into(),
                    is_read: true,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "p".into(),
                    is_draft: false,
                    is_meeting_request: false,
                    categories: vec!["Work".into()],
                },
            )
            .unwrap();
        app.reload_messages();
        app.open_message("mc");
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &mut app, f.area())).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Categories:"));
        assert!(text.contains("Work"));
    }

    #[test]
    fn renders_the_meeting_banner_for_an_invite_only() {
        use mailcore::graph::model::{Message, Recipient};
        let mut app = App::for_test_with_seeded_store();
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "invite1".into(),
                    conversation_id: "c9".into(),
                    subject: "Sprint review".into(),
                    from: Recipient {
                        name: "Boss".into(),
                        address: "boss@x".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-18T10:00:00Z".into(),
                    sent: "2026-07-18T09:00:00Z".into(),
                    is_read: false,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "invite".into(),
                    is_draft: false,
                    is_meeting_request: true,
                    categories: Vec::new(),
                },
            )
            .expect("seed invite");
        app.reload_messages();

        // Invite → banner present.
        app.open_message("invite1");
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &mut app, f.area())).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("Meeting invite"));
        assert!(text.contains("[A]ccept"));

        // Ordinary message → no banner.
        app.open_message("m1");
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &mut app, f.area())).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!text.contains("Meeting invite"));
    }

    #[test]
    fn data_image_key_distinguishes_same_length_different_content() {
        let a = data_image_key(b"AAAA");
        let b = data_image_key(b"BBBB");
        assert_ne!(a, b, "same-length different-content bytes must not collide");
        assert_eq!(
            data_image_key(b"AAAA"),
            a,
            "identical bytes must produce the same key"
        );
    }
}
