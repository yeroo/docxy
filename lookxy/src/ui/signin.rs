//! The sign-in modal: a centered overlay shown whenever `App::signin_modal`
//! is `Some` — the same overlay shape as the move-folder/attachments popups
//! (`ui::centered_rect`), drawn last so it sits on top of everything else.
//!
//! Three states (`app::SignInModal`):
//! - `Required` — no valid token; says why in plain words (#2169,
//!   [`signin_reason_text`]), then prompts "press Enter to sign in with your
//!   browser" (`App::on_key_enter` sends `SyncCommand::SignIn`). Taller
//!   than the others, so the reason and the prompt both fit at 80x24.
//! - `SavedUnavailable` — a saved sign-in whose keyring cannot be read now
//!   (#2167): the keyring's reason, and the keys (Enter signs in again, `r`
//!   asks the keyring again). The sync picks the keyring up by itself only
//!   while it is merely not running yet; after a dismissed unlock prompt,
//!   or with no keyring, only those keys move it.
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
use mailcore::sync::engine::SignInReason;

use ratatui::Frame;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

/// Renders the sign-in modal when `app.signin_modal` is open; a no-op
/// otherwise.
pub fn draw(f: &mut Frame, app: &App) {
    let Some(modal) = &app.signin_modal else {
        return;
    };

    // `Required` says why before its prompt (#2169): at 80x24 the 30%
    // rect leaves 6 text rows, too few for a long reason and the prompt,
    // so it gets half the height.
    let height = match modal {
        SignInModal::Required { .. } => 50,
        _ => 30,
    };
    let area = centered_rect(60, height, f.area());
    f.render_widget(Clear, area);

    let body = match modal {
        SignInModal::Required { reason } => format!(
            "{}\n\nPress Enter to sign in with your browser.",
            signin_reason_text(reason)
        ),
        SignInModal::SavedUnavailable { reason } => format!(
            "Your saved sign-in is in the keyring, which is locked or not \
             available:\n{reason}\n\n\
             Unlock the keyring and press r to try it again, or press Enter \
             to sign in again with your browser.\n\n\
             (If the keyring was only not running yet, lookxy also picks the \
             sign-in up once it is.)"
        ),
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

/// The OAuth codes that ask the person to do something at the sign-in
/// (#2169): confirm with MFA, give consent, or sign in interactively.
const INTERACTION_CODES: [&str; 3] = ["interaction_required", "consent_required", "login_required"];

/// Why the `Required` modal asks for a sign-in, in plain words (#2169).
/// An expiry with an AADSTS number shows it on its own line, for a
/// person to look up. A reason this lookxy does not know reads as not
/// signed in.
pub(crate) fn signin_reason_text(reason: &SignInReason) -> String {
    match reason {
        SignInReason::NotSignedIn => "Not signed in.".to_string(),
        SignInReason::SavedSignInUnreadable(why) => {
            format!("Couldn't read your saved sign-in:\n{why}")
        }
        SignInReason::Expired { code, aadsts } => {
            let text = if INTERACTION_CODES.contains(&code.as_str()) {
                "Microsoft 365 needs you to sign in again, for example to confirm \
                 with MFA or to give consent."
            } else {
                "Your sign-in expired. Sign in again."
            };
            match aadsts {
                Some(n) => format!("{text}\n(AADSTS{n})"),
                None => text.to_string(),
            }
        }
        SignInReason::Rejected => "Microsoft 365 refused your sign-in. Sign in again.".to_string(),
        SignInReason::SignedOut => "You signed out.".to_string(),
        SignInReason::SignInFailed => "The sign-in didn't finish. Try again.".to_string(),
        _ => "Not signed in.".to_string(),
    }
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

    /// The text of `app`'s modal drawn on a `w`x`h` terminal.
    fn drawn(app: &App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    fn expired(code: &str, aadsts: Option<u32>) -> SignInReason {
        SignInReason::Expired {
            code: code.into(),
            aadsts,
        }
    }

    /// #2169: each reason in its own plain words.
    #[test]
    fn signin_reason_text_says_each_reason() {
        let cases = [
            (SignInReason::NotSignedIn, "Not signed in."),
            (
                SignInReason::SavedSignInUnreadable("unexpected end of input at byte 60".into()),
                "Couldn't read your saved sign-in:\nunexpected end of input at byte 60",
            ),
            (
                expired("invalid_grant", None),
                "Your sign-in expired. Sign in again.",
            ),
            (
                expired("invalid_grant", Some(70008)),
                "Your sign-in expired. Sign in again.\n(AADSTS70008)",
            ),
            (expired("", None), "Your sign-in expired. Sign in again."),
            (
                SignInReason::Rejected,
                "Microsoft 365 refused your sign-in. Sign in again.",
            ),
            (SignInReason::SignedOut, "You signed out."),
            (
                SignInReason::SignInFailed,
                "The sign-in didn't finish. Try again.",
            ),
        ];
        for (reason, want) in cases {
            assert_eq!(signin_reason_text(&reason), want, "{reason:?}");
        }
        for code in INTERACTION_CODES {
            let text = signin_reason_text(&expired(code, Some(50076)));
            assert!(
                text.starts_with("Microsoft 365 needs you to sign in again"),
                "{code}: {text}"
            );
            assert!(text.ends_with("\n(AADSTS50076)"), "{code}: {text}");
        }
    }

    #[test]
    fn required_modal_renders_the_reason() {
        let mut app = App::for_test_with_seeded_store();
        app.signin_modal = Some(SignInModal::Required {
            reason: expired("invalid_grant", None),
        });
        let text = drawn(&app, 100, 30);
        assert!(text.contains("sign-in expired"), "{text}");
        assert!(text.contains("Enter"), "{text}");
    }

    /// #2169 (plan critique A): the longest reasons leave the prompt in
    /// view at 80x24, which the 30%-high modal did not.
    #[test]
    fn required_modal_shows_enter_at_80x24() {
        // A D-Bus store error as Secret Service words them: four wrapped
        // lines at this width, which the 30% rect had no room for.
        let long = "secret store failure: org.freedesktop.Secret.Error.NoSuchObject: \
                    The collection /org/freedesktop/secrets/collection/login \
                    holding the item does not exist";
        assert!(long.len() >= 140);
        for reason in [
            expired("interaction_required", Some(50076)),
            SignInReason::SavedSignInUnreadable(long.into()),
        ] {
            let mut app = App::for_test_with_seeded_store();
            app.signin_modal = Some(SignInModal::Required {
                reason: reason.clone(),
            });
            let text = drawn(&app, 80, 24);
            assert!(text.contains("Press Enter"), "{reason:?}: {text}");
        }
    }

    #[test]
    fn required_modal_renders_the_sign_in_prompt() {
        let mut app = App::for_test_with_seeded_store();
        app.signin_modal = Some(SignInModal::Required {
            reason: SignInReason::NotSignedIn,
        });

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
            authorize_url:
                "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize?x=1".into(),
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
