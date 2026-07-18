//! The search prompt (`/`) and its results: a one-row query line drawn over
//! the message-list pane's column, followed by the results
//! (`Store::search`, via `App::submit_search`) rendered with the exact same
//! row widget as the normal folder view (`ui::message_list::draw_list`) —
//! so a search result looks and behaves identically to any other message
//! row. `App::visible_messages` is the seam that makes this a "virtual
//! message list": once a query is submitted, it returns the search results
//! instead of the selected folder's messages, and this module is the only
//! one that renders it.

use crate::app::App;
use crate::ui::message_list;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;

/// Renders the search overlay in `area` (the message-list pane's column):
/// a one-row prompt line (the query so far, plus a result count once
/// submitted) on top, the results below via the shared list widget. A
/// no-op if the search prompt isn't open — `ui::draw` only calls this in
/// place of `message_list::draw` while `app.search` is `Some`.
pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let Some(search) = &app.search else {
        return;
    };

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let prompt = match &search.results {
        Some(results) => {
            let n = results.len();
            let noun = if n == 1 { "result" } else { "results" };
            format!("Search: {}_  ({n} {noun})", search.query)
        }
        None => format!("Search: {}_", search.query),
    };
    f.render_widget(
        Paragraph::new(prompt).style(Style::new().fg(Color::Black).bg(Color::Yellow)),
        rows[0],
    );

    message_list::draw_list(
        f,
        rows[1],
        "Search results",
        true,
        app.visible_messages(),
        app.msg_index,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_prompt_filters_results() {
        // `for_test_with_seeded_store` seeds one message ("Hello"); add a
        // second, budget-related one so the search has something to
        // actually filter out (a single-message store can't distinguish
        // "search works" from "search returns everything").
        use mailcore::graph::model::{Message, Recipient};
        let mut app = App::for_test_with_seeded_store(); // two messages, one about "budget"
        app.store
            .upsert_message(
                "inbox",
                &Message {
                    id: "m2".into(),
                    conversation_id: "c2".into(),
                    subject: "Budget review".into(),
                    from: Recipient {
                        name: "Bob".into(),
                        address: "bob@example.com".into(),
                    },
                    to: vec![],
                    cc: vec![],
                    received: "2026-07-15T09:00:00Z".into(),
                    sent: "2026-07-15T08:00:00Z".into(),
                    is_read: true,
                    is_flagged: false,
                    has_attachments: false,
                    importance: "normal".into(),
                    preview: "numbers for next quarter".into(),
                    is_draft: false,
                },
            )
            .expect("seed second message");

        app.on_key_char('/');
        app.type_query("budget");
        app.submit_search();
        assert_eq!(app.visible_message_count(), 1);
    }

    #[test]
    fn esc_clears_search_and_restores_the_folder_view() {
        let mut app = App::for_test_with_seeded_store();
        app.on_key_char('/');
        app.type_query("nothing matches this");
        app.submit_search();
        assert_eq!(app.visible_message_count(), 0);

        app.cancel_search();

        assert!(app.search.is_none());
        assert_eq!(app.visible_message_count(), app.messages.len());
    }

    #[test]
    fn empty_query_search_yields_no_results_without_panicking() {
        let mut app = App::for_test_with_seeded_store();
        app.on_key_char('/');
        app.submit_search();
        assert_eq!(app.visible_message_count(), 0);
    }

    #[test]
    fn draw_renders_the_query_and_result_rows() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut app = App::for_test_with_seeded_store();
        app.on_key_char('/');
        app.type_query("Hello");
        app.submit_search();
        assert_eq!(app.visible_message_count(), 1);

        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        term.draw(|f| draw(f, &app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Search: Hello"));
        assert!(text.contains("1 result"));
        assert!(text.contains("Hello")); // the matched message's subject row
    }

    #[test]
    fn draw_is_a_no_op_when_search_is_closed() {
        use ratatui::{Terminal, backend::TestBackend};

        let app = App::for_test_with_seeded_store();
        assert!(app.search.is_none());

        let mut term = Terminal::new(TestBackend::new(60, 20)).unwrap();
        term.draw(|f| draw(f, &app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(!text.contains("Search:"));
    }
}
