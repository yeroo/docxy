//! Rendering for [`crate::Backstage`], ported from docxy's `draw_backstage` /
//! `draw_bs_open` / `draw_bs_save_as` / `draw_bs_info` (main.rs).

use crate::{Backstage, BackstageHost, ITEMS, Item, Pane};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line as RLine;
use ratatui::widgets::{
    Block as RBlock, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

/// Render the backstage panel into `area` (the FULL frame area, row 0
/// included — the app has already drawn its ribbon tab strip there). Splits
/// `area` into `[Length(1), Min(1)]` and renders the menu + content into
/// `rows[1]` only; row 0 is left untouched.
pub fn draw(f: &mut Frame, area: Rect, bs: &mut Backstage, host: &dyn BackstageHost) {
    // Layout + preview cache, computed against the full-frame `area` (whose
    // y == 0) so `mouse`'s absolute coordinates line up with these rects.
    let preview_w = (area.width as usize).saturating_sub(50).max(8);
    bs.layout.preview_h = (area.height as usize).saturating_sub(3).max(1);
    bs.layout.name_top = area.height.saturating_sub(3);
    bs.layout.name_x0 = 16;
    bs.layout.save_btn = Rect {
        x: area.width.saturating_sub(10),
        y: bs.layout.name_top,
        width: 10,
        height: 3,
    };
    let list_h = (area.height as usize).saturating_sub(3).max(1);
    bs.layout.list_start = bs
        .sel
        .saturating_sub(list_h / 2)
        .min(bs.entries.len().saturating_sub(list_h));
    bs.refresh_preview(host, preview_w); // fill the preview cache at the pane width

    f.render_widget(Clear, area);
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
    let cols = Layout::horizontal([Constraint::Length(14), Constraint::Min(10)]).split(rows[1]);

    let dim = Style::default().add_modifier(Modifier::DIM);
    let accent = Style::default().fg(Color::Black).bg(host.accent());
    let rev = Style::default().add_modifier(Modifier::REVERSED);

    // left menu
    let menu_focus = bs.pane == Pane::Menu;
    let menu_lines: Vec<RLine> = ITEMS
        .iter()
        .map(|it| {
            let on = *it == bs.item;
            let style = if on && menu_focus {
                accent
            } else if on {
                rev
            } else {
                Style::default()
            };
            RLine::styled(format!(" {:<12}", it.label()), style)
        })
        .collect();
    f.render_widget(
        Paragraph::new(menu_lines)
            .block(RBlock::default().borders(Borders::RIGHT).border_style(dim)),
        cols[0],
    );

    // right content pane
    match bs.item {
        Item::Open => draw_open(f, cols[1], bs, host),
        Item::SaveAs => draw_save_as(f, cols[1], bs, host),
        Item::Info => draw_info(f, cols[1], host),
        other => {
            let msg = match other {
                Item::Save => "Save (Ctrl+S) — write changes to the current file.",
                Item::Export => "Export — write a PDF next to the document.",
                Item::New => "New — start a blank document.",
                Item::Exit => "Exit — quit docxy.",
                _ => "",
            };
            f.render_widget(
                Paragraph::new(format!("\n  {msg}\n\n  Enter to run · Esc to close")),
                cols[1],
            );
        }
    }
}

fn draw_open(f: &mut Frame, area: Rect, bs: &Backstage, host: &dyn BackstageHost) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let accent = Style::default().fg(Color::Black).bg(host.accent());
    let rev = Style::default().add_modifier(Modifier::REVERSED);
    // The preview gets most of the width; the file list is a compact column.
    let panes = Layout::horizontal([Constraint::Length(34), Constraint::Min(20)]).split(area);

    // file list (dir path as the box title)
    let title = format!(" {} ", bs.dir.display());
    let list_focus = bs.pane == Pane::Browser;
    let inner_h = panes[0].height.saturating_sub(2) as usize;
    let inner_w = panes[0].width.saturating_sub(2) as usize;
    let start = bs.layout.list_start;
    let mut lines = Vec::new();
    for (i, e) in bs.entries.iter().enumerate().skip(start).take(inner_h) {
        let on = i == bs.sel;
        let label = if e.is_dir {
            format!(" {}/", e.name)
        } else {
            // Right-align the size (with its unit) and fit the name to what's
            // left, so the unit is never clipped at the pane's edge.
            let size = e.size_str();
            let name_w = inner_w.saturating_sub(size.len() + 2).max(1);
            let name = fit_width(&e.name, name_w);
            format!(" {:<name_w$} {}", name, size)
        };
        let style = if on && list_focus {
            accent
        } else if on {
            rev
        } else if e.locked {
            dim
        } else {
            Style::default()
        };
        lines.push(RLine::styled(label, style));
    }
    f.render_widget(
        Paragraph::new(lines).block(
            RBlock::default()
                .borders(Borders::ALL)
                .border_style(dim)
                .title(title),
        ),
        panes[0],
    );

    // preview — a scrollable, read-only render of the highlighted document
    let prev_focus = bs.pane == Pane::Preview;
    let inner_ph = panes[1].height.saturating_sub(2) as usize;
    let scroll = bs
        .preview_scroll
        .min(bs.preview.len().saturating_sub(inner_ph));
    let prev: Vec<RLine> = bs
        .preview
        .iter()
        .skip(scroll)
        .take(inner_ph)
        .map(|s| RLine::raw(s.clone()))
        .collect();
    let pstyle = if prev_focus {
        Style::default().fg(host.accent())
    } else {
        dim
    };
    f.render_widget(
        Paragraph::new(prev).block(
            RBlock::default()
                .borders(Borders::ALL)
                .border_style(pstyle)
                .title(if prev_focus {
                    " Preview  (↑↓ PgUp/Dn scroll · ← list) "
                } else {
                    " Preview "
                }),
        ),
        panes[1],
    );
    // scrollbar on the preview's right edge
    if bs.preview.len() > inner_ph {
        let mut sb = ScrollbarState::new(bs.preview.len())
            .position(scroll)
            .viewport_content_length(inner_ph);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None),
            panes[1].inner(ratatui::layout::Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut sb,
        );
    }
}

fn draw_save_as(f: &mut Frame, area: Rect, bs: &Backstage, host: &dyn BackstageHost) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let accent = Style::default().fg(Color::Black).bg(host.accent());
    let rev = Style::default().add_modifier(Modifier::REVERSED);
    let focused = Style::default().fg(host.accent());
    // The focused piece gets an accent border; the other is dimmed.
    let (list_border, name_border) = if bs.name_focus {
        (dim, focused)
    } else {
        (focused, dim)
    };
    // Folder list on top, the typed file name in a box below it.
    let rows = Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).split(area);

    // folder list (only subfolders matter for choosing a destination)
    let title = format!(" {} ", bs.dir.display());
    let inner_h = rows[0].height.saturating_sub(2) as usize;
    let inner_w = rows[0].width.saturating_sub(2) as usize;
    let start = bs.layout.list_start;
    let mut lines = Vec::new();
    for (i, e) in bs.entries.iter().enumerate().skip(start).take(inner_h) {
        let on = i == bs.sel;
        let label = if e.is_dir {
            format!(" {}/", e.name)
        } else {
            let size = e.size_str();
            let name_w = inner_w.saturating_sub(size.len() + 2).max(1);
            format!(" {:<name_w$} {}", fit_width(&e.name, name_w), size)
        };
        // Highlight the selection strongly only when the browser is focused.
        let style = if on && !bs.name_focus {
            accent
        } else if on {
            rev
        } else if e.is_dir {
            Style::default()
        } else {
            // existing files are dimmed — they're overwrite targets, not folders
            dim
        };
        lines.push(RLine::styled(label, style));
    }
    f.render_widget(
        Paragraph::new(lines).block(
            RBlock::default()
                .borders(Borders::ALL)
                .border_style(list_border)
                .title(title),
        ),
        rows[0],
    );

    // The name band is the file-name input plus a Save button on the right.
    let btn = bs.layout.save_btn;
    let name_box = Rect {
        width: rows[1].width.saturating_sub(btn.width),
        ..rows[1]
    };
    // file-name input — the text is plain; the caret is the real terminal
    // cursor (same as the main editor), placed via set_cursor_position only
    // while the field is focused.
    f.render_widget(
        Paragraph::new(RLine::raw(format!(" {}", bs.name_input))).block(
            RBlock::default()
                .borders(Borders::ALL)
                .border_style(name_border)
                .title(" File name  (Tab · Enter · Esc) "),
        ),
        name_box,
    );
    // Save button — clickable duplicate of Enter.
    f.render_widget(
        Paragraph::new("Save")
            .alignment(Alignment::Center)
            .block(RBlock::default().borders(Borders::ALL).border_style(accent))
            .style(accent),
        btn,
    );
    if bs.name_focus {
        // left border (1) + leading space (1) + caret column, clamped to the box
        let inner_w = name_box.width.saturating_sub(2);
        let cx = (2 + bs.name_cursor as u16).min(inner_w);
        f.set_cursor_position(Position {
            x: name_box.x + cx,
            y: name_box.y + 1,
        });
    }
}

fn draw_info(f: &mut Frame, area: Rect, host: &dyn BackstageHost) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    f.render_widget(
        Paragraph::new(host.info_lines()).block(
            RBlock::default()
                .borders(Borders::ALL)
                .border_style(dim)
                .title(" Info "),
        ),
        area,
    );
}

/// Truncate `s` to at most `w` display columns, replacing an overflow with a
/// trailing ellipsis. Ported verbatim from docxy's `main.rs`.
fn fit_width(s: &str, w: usize) -> String {
    if w == 0 {
        return String::new();
    }
    if s.chars().count() <= w {
        return s.to_string();
    }
    let mut out: String = s.chars().take(w - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use crate::{Backstage, BackstageHost, Item};
    use ratatui::{Terminal, backend::TestBackend, style::Color, text::Line};
    use std::path::Path;

    struct H;
    impl BackstageHost for H {
        fn extensions(&self) -> &'static [&'static str] {
            &["docx"]
        }
        fn default_save_name(&self) -> String {
            "untitled.docx".into()
        }
        fn preview_lines(&self, _p: &Path, _w: usize) -> Vec<String> {
            vec!["hello".into()]
        }
        fn info_lines(&self) -> Vec<Line<'static>> {
            vec![Line::raw("info")]
        }
        fn accent(&self) -> Color {
            Color::Green
        }
    }

    #[test]
    fn draws_open_pane_without_panic() {
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        bs.item = Item::Open;
        term.draw(|f| {
            let a = f.area();
            super::draw(f, a, &mut bs, &H);
        })
        .unwrap();
    }
}
