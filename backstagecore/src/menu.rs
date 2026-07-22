//! The shared left-menu column drawn by every backstage (file + mail). One
//! styled implementation so the menu look is defined once.
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};

/// Render a vertical menu column into `area`: each label on its own row,
/// left-padded to `label_width`, with the selected row highlighted (black-on-
/// `accent` when `focused`, reversed otherwise) and a dim right border.
pub fn draw_menu_column(
    f: &mut Frame,
    area: Rect,
    labels: &[&str],
    sel: usize,
    focused: bool,
    accent: Color,
    label_width: usize,
) {
    let dim = Style::default().add_modifier(Modifier::DIM);
    let accent_style = Style::default().fg(Color::Black).bg(accent);
    let rev = Style::default().add_modifier(Modifier::REVERSED);
    let lines: Vec<Line> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let on = i == sel;
            let style = if on && focused {
                accent_style
            } else if on {
                rev
            } else {
                Style::default()
            };
            Line::styled(format!(" {:<label_width$}", label), style)
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::RIGHT).border_style(dim)),
        area,
    );
}
