//! The sign-in modal: a centered overlay shown whenever `App::signin_modal`
//! is `Some` — the same overlay shape as the move-folder/attachments popups
//! (`ui::centered_rect`), drawn last so it sits on top of everything else.
//!
//! Two states (`app::SignInModal`):
//! - `Required` — no valid token; prompts "press Enter to sign in with your
//!   browser" (`App::on_key_enter` sends `SyncCommand::SignIn`).
//! - `Started` — the engine's `begin_auth` succeeded and the browser has
//!   already been opened (`App::on_sync_event`'s `SignInStarted` handler);
//!   nothing left to press, just "finish it over there". Also shows the
//!   `authorize_url` itself as a fallback: `open_url_with_os_handler` is a
//!   fire-and-forget shell-out with no way to observe whether a browser
//!   actually launched, so a user whose OS-open silently no-ops still has a
//!   URL they can copy into a browser by hand.
//!
//! The modal blocks every other key while it's open (same as the other
//! popups — see `ui::handle_key`), since there's nothing useful the rest of
//! the UI can do without a token anyway. It clears itself on the next
//! successful sync (`App::on_sync_event`).

use crate::app::{App, SignInModal};
use crate::ui::centered_rect;

use ratatui::Frame;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

/// Renders the sign-in modal when `app.signin_modal` is open; a no-op
/// otherwise.
pub fn draw(f: &mut Frame, app: &App) {
    let Some(modal) = &app.signin_modal else {
        return;
    };

    let area = centered_rect(60, 30, f.area());
    f.render_widget(Clear, area);

    let body = match modal {
        SignInModal::Required => {
            "Not signed in.\n\nPress Enter to sign in with your browser.".to_string()
        }
        SignInModal::Started { authorize_url } => format!(
            "Signing in via your browser…\n\ncomplete it in the window that opened.\n\n\
             If your browser didn't open, visit:\n{authorize_url}"
        ),
    };

    let block = Block::default()
        .title("Sign in")
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Yellow));
    f.render_widget(
        Paragraph::new(body).wrap(Wrap { trim: false }).block(block),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn draws_nothing_when_the_modal_is_closed() {
        let app = App::for_test_with_seeded_store();
        assert!(app.signin_modal.is_none());

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(!text.to_lowercase().contains("sign in"));
    }

    #[test]
    fn required_modal_renders_the_sign_in_prompt() {
        let mut app = App::for_test_with_seeded_store();
        app.signin_modal = Some(SignInModal::Required);

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.to_lowercase().contains("sign in"));
        assert!(text.to_lowercase().contains("enter"));
    }

    #[test]
    fn started_modal_renders_the_browser_message_and_fallback_url() {
        let mut app = App::for_test_with_seeded_store();
        app.signin_modal = Some(SignInModal::Started {
            authorize_url: "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?x=1"
                .into(),
        });

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.to_lowercase().contains("browser"));
        // The fallback URL is shown verbatim so a user can copy it by hand
        // if the OS-open silently failed to launch anything.
        assert!(text.contains("login.microsoftonline.com"));
    }
}
