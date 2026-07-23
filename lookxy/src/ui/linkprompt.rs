//! The "open link?" warning dialog. Opening a link from an email is a
//! trust boundary, so the reader never opens one silently: focusing a link and
//! pressing Enter (or clicking it) raises this centered dialog showing the full
//! URL — toggleable to a parsed breakdown — and only opens it, in the default
//! browser, on an explicit Enter, and only for `http`/`https`.

use crate::app::App;
use crate::ui::centered_rect;

use ratatui::Frame;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// The dialog state: the target URL and whether the parsed breakdown is shown.
pub struct LinkPrompt {
    pub url: String,
    pub parsed: bool,
}

/// Whether `url` is safe to hand to the OS's browser opener: non-empty, not
/// absurdly long, no control characters, and a web scheme. Ported from docxy.
pub fn safe_url(url: &str) -> bool {
    if url.is_empty() || url.len() > 2048 {
        return false;
    }
    if url.chars().any(|c| (c as u32) < 0x20 || c == '\u{7f}') {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Splits a URL into its present components, in display order:
/// `protocol`/`host`/`path`/`query`. Hand-rolled (no url crate); blank parts
/// are omitted.
pub fn parse_url_parts(url: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let rest = if let Some((scheme, after)) = url.split_once("://") {
        out.push(("protocol", scheme.to_string()));
        after
    } else {
        url
    };
    let host_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let host = &rest[..host_end];
    if !host.is_empty() {
        out.push(("host", host.to_string()));
    }
    let after_host = &rest[host_end..];
    let (path, query) = match after_host.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (after_host, None),
    };
    if !path.is_empty() {
        out.push(("path", path.to_string()));
    }
    if let Some(q) = query.filter(|q| !q.is_empty()) {
        out.push(("query", q.to_string()));
    }
    out
}

/// Renders the dialog when `app.link_prompt` is set; a no-op otherwise.
pub fn draw(f: &mut Frame, app: &App) {
    let Some(p) = &app.link_prompt else {
        return;
    };
    let area = centered_rect(70, 50, f.area());
    f.render_widget(Clear, area);
    let block = Block::default().title("Open link?").borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if p.parsed {
        for (label, value) in parse_url_parts(&p.url) {
            lines.push(Line::from(format!("  {label}: {value}")));
        }
    } else {
        // Char-wrap the raw URL to the inner width.
        let w = (inner.width as usize).max(1);
        let chars: Vec<char> = p.url.chars().collect();
        for chunk in chars.chunks(w) {
            lines.push(Line::from(chunk.iter().collect::<String>()));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::styled(
        "Enter open \u{b7} p toggle view \u{b7} Esc cancel",
        Style::new().add_modifier(Modifier::DIM),
    ));
    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_url_allows_only_web_schemes() {
        assert!(safe_url("https://acme.com"));
        assert!(safe_url("http://x"));
        assert!(!safe_url("javascript:alert(1)"));
        assert!(!safe_url("mailto:a@b"));
        assert!(!safe_url(""));
    }

    #[test]
    fn parse_url_parts_splits_scheme_host_path_query() {
        let parts = parse_url_parts("https://acme.com/a/b?x=1&y=2");
        assert_eq!(parts[0], ("protocol", "https".to_string()));
        assert_eq!(parts[1], ("host", "acme.com".to_string()));
        assert_eq!(parts[2], ("path", "/a/b".to_string()));
        assert_eq!(parts[3], ("query", "x=1&y=2".to_string()));

        let bare = parse_url_parts("https://acme.com");
        assert_eq!(
            bare,
            vec![("protocol", "https".into()), ("host", "acme.com".into())]
        );
    }
}
