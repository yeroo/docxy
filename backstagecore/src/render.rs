//! Rendering for [`crate::Backstage`], ported from docxy's `draw_backstage` /
//! `draw_bs_open` / `draw_bs_save_as` / `draw_bs_info` (main.rs).

use crate::{Backstage, BackstageHost, Item, Pane};
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
    // Save As's bottom band: the name box, plus the type box when there is one.
    let band = if bs.save_types.is_empty() { 3 } else { 6 };
    bs.layout.name_top = area.height.saturating_sub(band);
    bs.layout.type_top = area.height.saturating_sub(3);
    bs.layout.name_x0 = 16;
    bs.layout.save_btn = Rect {
        x: area.width.saturating_sub(10),
        y: bs.layout.name_top,
        width: 10,
        height: 3,
    };
    let list_h = (area.height as usize).saturating_sub(3).max(1);
    bs.layout.list_start = if bs.item == Item::Export {
        // Export's type rows start 6 rows down; keep the selection in view.
        let rows = (area.height as usize).saturating_sub(7).max(1);
        bs.export_sel
            .saturating_sub(1)
            .saturating_sub(rows - 1)
            .min(bs.save_types.len().saturating_sub(rows))
    } else {
        bs.sel
            .saturating_sub(list_h / 2)
            .min(bs.entries.len().saturating_sub(list_h))
    };
    bs.refresh_preview(host, preview_w); // fill the preview cache at the pane width

    // Clear only the menu + content region (rows[1..]); row 0 holds the app's
    // ribbon tab strip and must never be wiped, whether the app draws it before
    // or after this call.
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
    f.render_widget(Clear, rows[1]);
    let cols = Layout::horizontal([Constraint::Length(14), Constraint::Min(10)]).split(rows[1]);

    // left menu
    let menu_focus = bs.pane == Pane::Menu;
    let labels: Vec<&str> = bs.items().iter().map(|it| it.label()).collect();
    let sel_index = bs.items().iter().position(|it| *it == bs.item).unwrap_or(0);
    crate::draw_menu_column(
        f,
        cols[0],
        &labels,
        sel_index,
        menu_focus,
        host.accent(),
        12,
    );

    // right content pane
    match bs.item {
        Item::Open => draw_open(f, cols[1], bs, host),
        Item::SaveAs => draw_save_as(f, cols[1], bs, host),
        Item::Info => draw_info(f, cols[1], bs, host),
        Item::Options => draw_options(f, cols[1], bs, host),
        Item::Export if !bs.save_types.is_empty() => draw_export(f, cols[1], bs, host),
        other => {
            // App-neutral: the same crate serves docxy (PDF), xlsxy (CSV) and
            // yppxy (Gantt), so avoid naming a format or the app.
            let msg = match other {
                Item::Save => "Save (Ctrl+S) — write changes to the current file.",
                Item::Export => "Export — write an export next to the current file.",
                Item::New => "New — start a blank file.",
                Item::Exit => "Exit — quit the app.",
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
    // Folder list on top, the typed file name in a box below it, and the
    // Save as type box below that when the host has types.
    let rows = if bs.save_types.is_empty() {
        Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).split(area)
    } else {
        Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(area)
    };

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
    if let (Some(t), Some(&type_box)) = (bs.save_types.get(bs.type_sel), rows.get(2)) {
        let border = if bs.type_focus { focused } else { dim };
        f.render_widget(
            Paragraph::new(RLine::raw(format!(" {} (*.{})", t.label, t.ext))).block(
                RBlock::default()
                    .borders(Borders::ALL)
                    .border_style(border)
                    .title(" Save as type  (↑↓ when focused) "),
            ),
            type_box,
        );
    }
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

/// Export with a type list: the host's quick export, then Change File Type.
/// Rows sit at fixed offsets (see `mouse`): the quick export at 3, the types
/// from 6, scrolled by `layout.list_start`.
fn draw_export(f: &mut Frame, area: Rect, bs: &Backstage, host: &dyn BackstageHost) {
    let focus = bs.pane == Pane::Export;
    let accent = Style::default().fg(Color::Black).bg(host.accent());
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let row = |text: String, on: bool| {
        RLine::styled(
            text,
            if focus && on {
                accent
            } else {
                Style::default()
            },
        )
    };
    let mut lines = vec![
        RLine::styled(" Export", bold),
        RLine::raw(""),
        row(format!(" {}", bs.export_quick), bs.export_sel == 0),
        RLine::raw(""),
        RLine::styled(" Change File Type", bold),
    ];
    let rows = (area.height as usize).saturating_sub(6).max(1);
    for (i, t) in bs
        .save_types
        .iter()
        .enumerate()
        .skip(bs.layout.list_start)
        .take(rows)
    {
        lines.push(row(
            format!("   {} (*.{})", t.label, t.ext),
            bs.export_sel == i + 1,
        ));
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// The Options page: a heading and its checkboxes. Rows sit at fixed offsets
/// (heading, blank, then one per option) so `mouse` can map a click.
fn draw_options(f: &mut Frame, area: Rect, bs: &Backstage, host: &dyn BackstageHost) {
    let focus = bs.pane == Pane::Options;
    let accent = Style::default().fg(Color::Black).bg(host.accent());
    let mut lines = vec![
        RLine::styled(
            format!(" {}", bs.options_title),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        RLine::raw(""),
    ];
    for (i, (label, on)) in bs.options.iter().enumerate() {
        let text = format!(" [{}] {label}", if *on { "x" } else { " " });
        let style = if focus && i == bs.option_sel {
            accent
        } else {
            Style::default()
        };
        lines.push(RLine::styled(text, style));
    }
    lines.push(RLine::raw(""));
    lines.push(RLine::styled(
        if focus {
            " ↑↓ choose · Space toggle · ← menu · Esc close"
        } else {
            " Enter to change these options · Esc to close"
        },
        Style::default().add_modifier(Modifier::DIM),
    ));
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_info(f: &mut Frame, area: Rect, bs: &mut Backstage, host: &dyn BackstageHost) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let mut lines = host.info_lines();
    let fields = host.info_fields();
    let custom = host.info_custom_row();
    let mut scroll = 0usize;
    if !fields.is_empty() || custom {
        let focus = bs.pane == Pane::Info;
        let accent = Style::default().fg(Color::Black).bg(host.accent());
        lines.push(RLine::raw(""));
        let first_row = lines.len();
        let rows = fields
            .iter()
            .map(|(label, value)| format!("  {label:<18}{value}"))
            .chain(custom.then(|| "  Custom property…".to_string()));
        for (i, text) in rows.enumerate() {
            let style = if focus && i == bs.info_sel {
                accent
            } else {
                Style::default()
            };
            lines.push(RLine::styled(text, style));
        }
        lines.push(RLine::raw(""));
        if let Some(msg) = &bs.info_message {
            lines.push(RLine::styled(
                format!("  {msg}"),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        }
        lines.push(RLine::styled(
            if focus {
                "  ↑↓ choose · Enter edit · ← menu · Esc close"
            } else {
                "  Enter to edit the properties · Esc to close"
            },
            dim,
        ));
        // Scroll only when the selected row would leave the box, so a row
        // stays under the pointer between a selecting and an editing click.
        let inner = area.height.saturating_sub(2) as usize;
        let sel_line = first_row + bs.info_sel;
        if focus && inner > 0 {
            scroll = bs
                .info_scroll
                .min(sel_line)
                .max((sel_line + 1).saturating_sub(inner));
        }
        bs.info_scroll = scroll;
        bs.layout.info_top = i32::from(area.y) + 1 + first_row as i32 - scroll as i32;
        bs.layout.info_view = (area.y + 1, area.y + 1 + inner as u16);
    }
    f.render_widget(
        Paragraph::new(lines)
            .scroll((scroll.min(u16::MAX as usize) as u16, 0))
            .block(
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

    const TYPES: &[crate::SaveType] = &[
        crate::SaveType {
            label: "Excel Workbook",
            ext: "xlsx",
        },
        crate::SaveType {
            label: "Text (Tab delimited)",
            ext: "txt",
        },
    ];

    fn screen(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn draws_save_as_with_a_type_box_and_export_with_change_file_type() {
        let mut term = Terminal::new(TestBackend::new(90, 24)).unwrap();
        let mut bs = Backstage::open(std::env::temp_dir(), &["xlsx"])
            .with_save_types(TYPES, "Export CSV next to the workbook");
        bs.begin_save_as("book.xlsx".into(), None);
        term.draw(|f| {
            let a = f.area();
            super::draw(f, a, &mut bs, &H);
        })
        .unwrap();
        let text = screen(&term);
        assert!(text.contains("Save as type"), "{text}");
        assert!(text.contains("Excel Workbook (*.xlsx)"), "{text}");
        assert_eq!(bs.layout.name_top, 18);
        assert_eq!(bs.layout.type_top, 21);
        bs.item = Item::Export;
        bs.pane = crate::Pane::Export;
        term.draw(|f| {
            let a = f.area();
            super::draw(f, a, &mut bs, &H);
        })
        .unwrap();
        let text = screen(&term);
        assert!(text.contains("Export CSV next to the workbook"), "{text}");
        assert!(text.contains("Change File Type"), "{text}");
        assert!(text.contains("Text (Tab delimited) (*.txt)"), "{text}");
    }

    #[test]
    fn a_host_without_types_keeps_its_save_as_band() {
        let mut term = Terminal::new(TestBackend::new(90, 24)).unwrap();
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        bs.begin_save_as("a.docx".into(), None);
        term.draw(|f| {
            let a = f.area();
            super::draw(f, a, &mut bs, &H);
        })
        .unwrap();
        assert_eq!(bs.layout.name_top, 21);
        assert!(!screen(&term).contains("Save as type"));
    }

    #[test]
    fn draws_the_options_page() {
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut bs = Backstage::open(std::env::temp_dir(), &["xlsx"]).with_options(
            "Automatic Data Conversion",
            vec![("Keep zeros".into(), true)],
        );
        bs.item = Item::Options;
        term.draw(|f| {
            let a = f.area();
            super::draw(f, a, &mut bs, &H);
        })
        .unwrap();
        let text = format!("{:?}", term.backend().buffer());
        assert!(text.contains("Options"), "menu item missing");
        assert!(
            text.contains("Automatic Data Conversion"),
            "heading missing"
        );
        assert!(text.contains("[x] Keep zeros"), "checkbox missing");
    }

    struct Editable;
    impl BackstageHost for Editable {
        fn extensions(&self) -> &'static [&'static str] {
            &["xlsx"]
        }
        fn default_save_name(&self) -> String {
            "book.xlsx".into()
        }
        fn preview_lines(&self, _p: &Path, _w: usize) -> Vec<String> {
            Vec::new()
        }
        fn info_lines(&self) -> Vec<Line<'static>> {
            vec![Line::raw("  File  book.xlsx"), Line::raw("  Author  Me")]
        }
        fn accent(&self) -> Color {
            Color::Green
        }
        fn info_fields(&self) -> Vec<(String, String)> {
            vec![
                ("Title".into(), "Budget".into()),
                ("Tags".into(), String::new()),
            ]
        }
        fn info_custom_row(&self) -> bool {
            true
        }
    }

    #[test]
    fn draws_the_info_page_with_its_editable_rows() {
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut bs = Backstage::open(std::env::temp_dir(), &["xlsx"]);
        bs.focus_info(1);
        term.draw(|f| {
            let a = f.area();
            super::draw(f, a, &mut bs, &Editable);
        })
        .unwrap();
        let text = screen(&term);
        let lines: Vec<&str> = text.lines().collect();
        // Box border at y 1, two info lines, a blank line, then the rows.
        assert_eq!(bs.layout.info_top, 5);
        assert!(lines[5].contains("Title             Budget"), "{text}");
        assert!(lines[6].contains("Tags"), "{text}");
        assert!(lines[7].contains("Custom property…"), "{text}");
        assert!(text.contains("Enter edit"), "{text}");
        // The focused row is highlighted in the host's accent.
        let buf = term.backend().buffer();
        assert_eq!(buf[(18, 6)].bg, Color::Green);
        assert_ne!(buf[(18, 5)].bg, Color::Green);
    }

    /// More rows than the box holds: the page scrolls so the selected row
    /// stays inside the border, and a click on the border does nothing.
    #[test]
    fn a_tall_info_page_keeps_the_selected_row_in_the_box() {
        struct Tall;
        impl BackstageHost for Tall {
            fn extensions(&self) -> &'static [&'static str] {
                &["xlsx"]
            }
            fn default_save_name(&self) -> String {
                "book.xlsx".into()
            }
            fn preview_lines(&self, _p: &Path, _w: usize) -> Vec<String> {
                Vec::new()
            }
            fn info_lines(&self) -> Vec<Line<'static>> {
                (0..15).map(|i| Line::raw(format!("  line {i}"))).collect()
            }
            fn accent(&self) -> Color {
                Color::Green
            }
            fn info_fields(&self) -> Vec<(String, String)> {
                (0..8)
                    .map(|i| (format!("Field{i}"), String::new()))
                    .collect()
            }
            fn info_custom_row(&self) -> bool {
                true
            }
        }
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut bs = Backstage::open(std::env::temp_dir(), &["xlsx"]);
        bs.focus_info(8);
        term.draw(|f| {
            let a = f.area();
            super::draw(f, a, &mut bs, &Tall);
        })
        .unwrap();
        let text = screen(&term);
        let lines: Vec<&str> = text.lines().collect();
        // The box spans rows 1..=23; its last inner row is 22.
        assert_eq!(bs.layout.info_view, (2, 23));
        assert!(lines[22].contains("Custom property…"), "{text}");
        assert!(lines[23].contains("└"), "{text}");
        assert_eq!(term.backend().buffer()[(18, 22)].bg, Color::Green);
        // A click on the bottom border does nothing; one on the row edits it.
        assert!(matches!(
            bs.mouse(20, 23, &Tall),
            crate::BackstageEvent::None
        ));
        assert_eq!(bs.info_sel, 8);
        assert!(matches!(
            bs.mouse(20, 22, &Tall),
            crate::BackstageEvent::EditInfo(8)
        ));
        // The page holds still while the selection stays in the box: a row
        // clicked once is still under the pointer for the second click.
        let y5 = (bs.layout.info_top + 5) as u16;
        assert!(matches!(
            bs.mouse(20, y5, &Tall),
            crate::BackstageEvent::None
        ));
        assert_eq!(bs.info_sel, 5);
        let before = bs.layout.info_top;
        term.draw(|f| {
            let a = f.area();
            super::draw(f, a, &mut bs, &Tall);
        })
        .unwrap();
        assert_eq!(bs.layout.info_top, before);
        assert!(matches!(
            bs.mouse(20, y5, &Tall),
            crate::BackstageEvent::EditInfo(5)
        ));
        // From the top, Down past the box's last row moves the page by one
        // line per step; Down inside the box doesn't move it.
        let down = |bs: &mut Backstage, term: &mut Terminal<TestBackend>| {
            bs.key(
                ratatui::crossterm::event::KeyEvent::from(ratatui::crossterm::event::KeyCode::Down),
                &Tall,
            );
            term.draw(|f| {
                let a = f.area();
                super::draw(f, a, bs, &Tall);
            })
            .unwrap();
            bs.info_scroll
        };
        bs.focus_info(0);
        bs.info_scroll = 0;
        term.draw(|f| {
            let a = f.area();
            super::draw(f, a, &mut bs, &Tall);
        })
        .unwrap();
        assert_eq!(bs.info_scroll, 0);
        // Rows start on screen row 18; inner rows end at 22: rows 0..=4 fit.
        let scrolls: Vec<usize> = (1..=8).map(|_| down(&mut bs, &mut term)).collect();
        assert_eq!(scrolls, [0, 0, 0, 0, 1, 2, 3, 4]);
        // Back up inside the box: still.
        bs.key(
            ratatui::crossterm::event::KeyEvent::from(ratatui::crossterm::event::KeyCode::Up),
            &Tall,
        );
        term.draw(|f| {
            let a = f.area();
            super::draw(f, a, &mut bs, &Tall);
        })
        .unwrap();
        assert_eq!(bs.info_scroll, 4);
        // Unfocused (back on the menu), the page shows its top.
        bs.pane = crate::Pane::Menu;
        term.draw(|f| {
            let a = f.area();
            super::draw(f, a, &mut bs, &Tall);
        })
        .unwrap();
        let text = screen(&term);
        assert!(text.lines().nth(2).unwrap().contains("line 0"), "{text}");
    }

    #[test]
    fn draws_a_read_only_info_page_as_before() {
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut bs = Backstage::open(std::env::temp_dir(), &["docx"]);
        bs.item = Item::Info;
        term.draw(|f| {
            let a = f.area();
            super::draw(f, a, &mut bs, &H);
        })
        .unwrap();
        let text = screen(&term);
        assert!(text.contains("info"), "{text}");
        assert!(!text.contains("Custom property"), "{text}");
        assert!(!text.contains("Enter to edit"), "{text}");
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
