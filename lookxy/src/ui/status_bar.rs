//! The bottom, single-row status bar: signed-in account, sync state,
//! folder/message counts, then either the error (red) or the progress of an
//! upload or a folder's backfill and the last attachment save/open notice.
//! Whatever it carries, it is one line cut to fit the bar (#2117).

use crate::app::{App, Mode};

use mailcore::sync::engine::SyncState;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

/// Between the bar's parts.
const SEP: &str = " — ";

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let (line, style) = status_line(app, usize::from(area.width));
    f.render_widget(Paragraph::new(line).style(style), area);
}

/// The bar's text, already one line and at most `width` columns, and its
/// style.
fn status_line(app: &App, width: usize) -> (String, Style) {
    let account = app.account.as_deref().unwrap_or("not signed in");
    let status = match &app.status {
        SyncState::Idle => "Idle".to_string(),
        SyncState::Syncing => "Syncing...".to_string(),
        SyncState::Offline => "Offline".to_string(),
        SyncState::PendingOps(n) => format!("{n} pending"),
        SyncState::SignInRequired => "Sign-in required".to_string(),
    };
    // In Calendar mode the folder/message counts don't apply — show the
    // agenda's own count instead, same sync-state prefix either way, so the
    // account/status portion of the bar reads identically in both modes.
    let (place, counts) = if app.mode == Mode::Calendar {
        (
            "Calendar".to_string(),
            format!("{} event(s)", app.agenda.len()),
        )
    } else {
        let folder = app
            .selected_folder
            .as_ref()
            .and_then(|id| app.folders.iter().find(|f| &f.id == id))
            .map(|f| f.display_name.as_str())
            .unwrap_or("(no folder)");
        // `visible_message_count` (not `app.messages.len()`) so the count
        // reflects the search results while a query is active, not the
        // underlying folder's real size.
        (
            folder.to_string(),
            format!(
                "{} folder(s), {} message(s)",
                app.folders.len(),
                app.visible_message_count()
            ),
        )
    };
    let head = [account, status.as_str(), place.as_str()].join(SEP);
    // An error notice takes precedence over everything after the counts and
    // is drawn in a distinct red style — a failed save must never look like
    // a success, and no progress line may push it out of view. A failed send
    // (#1874) is drawn the same way, after the error notice.
    if let Some(err) = app.error_notice.as_ref().or(app.send_failure.as_ref()) {
        let style = Style::new().fg(Color::White).bg(Color::Red);
        let full = one_line(&[head.as_str(), &counts, err].join(SEP));
        if full.width() <= width {
            return (full, style);
        }
        // Too wide: the error wins the counts' room before it is cut.
        let short = one_line(&[head.as_str(), err].join(SEP));
        return (fit_line(&short, width), style);
    }
    // Otherwise the normal dark-gray bar shows what is going on: an upload,
    // a folder's backfill or that the selected folder is up to date, and
    // the last attachment save/open outcome, if any.
    let mut parts = vec![head, counts];
    if let Some(p) = &app.upload_progress {
        parts.push(format!(
            "Uploading {}: {}/{}",
            p.name,
            human_bytes(p.sent),
            human_bytes(p.total)
        ));
    }
    if let Some(b) = &app.backfill {
        parts.push(updating_line(app, b));
    }
    if app.mode == Mode::Mail
        && app
            .selected_folder
            .as_ref()
            .is_some_and(|id| app.folders_up_to_date.contains(id))
    {
        parts.push("This folder is up to date".to_string());
    }
    if let Some(notice) = &app.attachment_notice {
        parts.push(notice.clone());
    }
    let line = fit_line(&one_line(&parts.join(SEP)), width);
    (line, Style::new().fg(Color::White).bg(Color::DarkGray))
}

/// `Updating <folder>… <synced> of <total>` for the backfill walk under way
/// (#1871): `synced` clamped to `total`, which can be below it; with no
/// total (window mode, or a negative one) no "of". A folder not (yet) in
/// `app.folders` is named `mail`, never by its Graph id.
fn updating_line(app: &App, b: &crate::app::FolderProgressLine) -> String {
    let name = app
        .folders
        .iter()
        .find(|f| f.id == b.folder_id)
        .map(|f| f.display_name.as_str())
        .unwrap_or("mail");
    match b.total.and_then(|t| u64::try_from(t).ok()) {
        Some(t) => format!(
            "Updating {name}… {} of {}",
            group_thousands(b.synced.min(t)),
            group_thousands(t)
        ),
        None => format!("Updating {name}… {}", group_thousands(b.synced)),
    }
}

/// `s` as one line: every control or whitespace character (`\n`, `\r`,
/// `\t`, …) becomes a space, runs of them collapse to one, and the ends are
/// trimmed — so an error text from Graph, the token endpoint or a parser
/// can never break the bar (#1959).
fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() || c.is_control() {
            space = true;
        } else {
            if space && !out.is_empty() {
                out.push(' ');
            }
            space = false;
            out.push(c);
        }
    }
    out
}

/// `s` cut to at most `width` display columns: whole when it fits, else its
/// first `width - 1` columns and `…`.
fn fit_line(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = super::truncate_width(s, width - 1);
    out.push('…');
    out
}

/// `n` with its thousands grouped by commas: `48210` → `48,210`.
fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A byte count as `N B`, or `N.N KB`/`MB`/`GB` (base 1024, one decimal).
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 3] = ["KB", "MB", "GB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64 / 1024.0;
    let mut unit = 0;
    while v >= 1024.0 && unit + 1 < UNITS.len() {
        v /= 1024.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use mailcore::sync::engine::SyncEvent;
    use ratatui::{Terminal, backend::TestBackend};

    fn render(app: &App) -> String {
        let mut term = Terminal::new(TestBackend::new(120, 5)).unwrap();
        term.draw(|f| draw(f, app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect()
    }

    #[test]
    fn shows_not_signed_in_placeholder_with_no_account() {
        let app = App::for_test_with_seeded_store();
        assert!(app.account.is_none());
        assert!(render(&app).contains("not signed in"));
    }

    #[test]
    fn shows_folder_and_message_counts() {
        let app = App::for_test_with_seeded_store();
        let text = render(&app);
        assert!(text.contains("1 folder(s)"));
        assert!(text.contains("1 message(s)"));
    }

    #[test]
    fn error_notice_is_shown_and_takes_precedence_over_the_success_notice() {
        let mut app = App::for_test_with_seeded_store();
        app.attachment_notice = Some("Saved: ok.txt".into());
        app.error_notice = Some("save failed".into());
        let text = render(&app);
        assert!(text.contains("save failed"));
        // The success notice is suppressed while an error is showing.
        assert!(!text.contains("Saved: ok.txt"));
    }

    #[test]
    fn a_failed_send_stays_shown_after_the_engine_settles() {
        use mailcore::sync::engine::{SyncEvent, SyncState};
        let mut app = App::for_test_with_seeded_store();
        // The order the engine emits for a 403 on `/send` (#1874): the
        // failure, the folder reloads, then the settle to `Idle`, which
        // clears `error_notice`.
        for evt in [
            SyncEvent::Sending {
                id: "local:1".into(),
            },
            SyncEvent::SendFailed {
                id: "local:1".into(),
                error: "Access is denied.".into(),
            },
            SyncEvent::MessagesUpdated {
                folder_id: "DRAFTS1".into(),
            },
            SyncEvent::MessagesUpdated {
                folder_id: "SENT1".into(),
            },
            SyncEvent::State(SyncState::Idle),
        ] {
            app.on_sync_event(evt);
        }
        assert_eq!(
            app.send_failure.as_deref(),
            Some("Send failed: Access is denied.")
        );
        assert!(render(&app).contains("Send failed: Access is denied."));
    }

    #[test]
    fn calendar_mode_shows_sync_state_and_event_count_instead_of_the_folder_line() {
        use mailcore::store::NewEvent;

        let mut app = App::for_test_with_seeded_store();
        app.store
            .upsert_event(&NewEvent {
                id: "e1".into(),
                subject: "Standup".into(),
                start_utc: "2020-01-05T12:00:00Z".into(),
                end_utc: "2020-01-05T13:00:00Z".into(),
                is_all_day: false,
                location: "".into(),
                organizer_name: "".into(),
                organizer_addr: "".into(),
                response_status: "none".into(),
                series_master_id: None,
                body_preview: "".into(),
                web_link: "".into(),
                last_modified: "2020-01-01T00:00:00Z".into(),
                body_html: "".into(),
                reminder_minutes: 0,
                is_reminder_on: false,
                ..Default::default()
            })
            .unwrap();
        // Directly populate `agenda` — this module doesn't own the reload
        // logic (`App::reload_agenda`/`toggle_mode` do), so it only needs to
        // render whatever's already there.
        app.agenda = app
            .store
            .events_in_window("2000-01-01T00:00:00Z", "2100-01-01T00:00:00Z")
            .unwrap();
        app.mode = crate::app::Mode::Calendar;

        let text = render(&app);
        assert!(text.contains("Idle"), "still shows the sync state");
        assert!(text.contains("1 event(s)"));
        // The mail-mode folder/message counts must not leak into this line.
        assert!(!text.contains("message(s)"));
    }

    /// The bar drawn into the top row of a `w`×3 terminal: each row's text.
    fn rows(app: &App, w: u16) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(w, 3)).unwrap();
        term.draw(|f| draw(f, app, Rect::new(0, 0, w, 1))).unwrap();
        let buf = term.backend().buffer().clone();
        (0..3)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect())
            .collect()
    }

    const MB: u64 = 1024 * 1024;

    fn progress(draft_id: &str, sent: u64) -> SyncEvent {
        SyncEvent::AttachmentProgress {
            draft_id: draft_id.into(),
            path: "/tmp/big.pdf".into(),
            name: "big.pdf".into(),
            sent,
            total: 10 * MB,
        }
    }

    fn folder_progress(
        folder_id: &str,
        synced: u64,
        total: Option<i64>,
        complete: bool,
    ) -> SyncEvent {
        SyncEvent::FolderProgress {
            folder_id: folder_id.into(),
            synced,
            total,
            complete,
        }
    }

    /// #2062 plan-lookxy-progress: a send's upload shows on the bar, the
    /// latest value even when it went down, until the send ends.
    #[test]
    fn plan_lookxy_progress_shows_upload_and_clears_on_sent() {
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(SyncEvent::Sending { id: "D1".into() });
        app.on_sync_event(progress("D1", 5 * MB));
        assert!(render(&app).contains("Uploading big.pdf: 5.0 MB/10.0 MB"));
        // A fallback to a new upload session starts over: the lower value.
        app.on_sync_event(progress("D1", 3 * MB));
        let text = render(&app);
        assert!(text.contains("Uploading big.pdf: 3.0 MB/10.0 MB"));
        assert!(!text.contains("5.0 MB"));
        app.on_sync_event(SyncEvent::Sent { id: "D1".into() });
        assert!(!render(&app).contains("Uploading"));

        // A drain a transport error stopped sends no `Sent`: going offline
        // ends the line too.
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(progress("D1", 5 * MB));
        app.on_sync_event(SyncEvent::State(SyncState::Offline));
        assert!(app.upload_progress.is_none());
        assert!(!render(&app).contains("Uploading"));
    }

    #[test]
    fn plan_lookxy_progress_send_failed_clears_only_its_own_draft() {
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(progress("D1", 5 * MB));
        // Another draft's end leaves this draft's upload line.
        app.on_sync_event(SyncEvent::Sent { id: "D2".into() });
        app.on_sync_event(SyncEvent::SendFailed {
            id: "D2".into(),
            error: "nope".into(),
        });
        assert!(app.upload_progress.is_some());
        app.send_failure = None;
        assert!(render(&app).contains("Uploading big.pdf: 5.0 MB/10.0 MB"));
        // Its own failure ends it, and the failure shows.
        app.on_sync_event(SyncEvent::SendFailed {
            id: "D1".into(),
            error: "Access is denied.".into(),
        });
        assert!(app.upload_progress.is_none());
        let text = render(&app);
        assert!(text.contains("Send failed: Access is denied."));
        assert!(!text.contains("Uploading"));
    }

    /// #1976 plan-ui: a folder's backfill shows as `Updating <folder>…`.
    #[test]
    fn plan_ui_folder_progress_updating_line() {
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(folder_progress("inbox", 12_340, Some(48_210), false));
        assert!(render(&app).contains("Updating Inbox… 12,340 of 48,210"));
        // `synced` can pass `total`: clamped.
        app.on_sync_event(folder_progress("inbox", 50_000, Some(48_210), false));
        assert!(render(&app).contains("Updating Inbox… 48,210 of 48,210"));
        // Window mode has no total, and a negative one is unknown.
        for total in [None, Some(-1)] {
            app.on_sync_event(folder_progress("inbox", 12_340, total, false));
            let text = render(&app);
            assert!(text.contains("Updating Inbox… 12,340"), "{total:?}: {text}");
            assert!(!text.contains(" of "), "{total:?}: {text}");
        }
        // A folder lookxy has not loaded yet is never named by its id.
        app.on_sync_event(folder_progress("AAMkGraphId", 7, None, false));
        let text = render(&app);
        assert!(text.contains("Updating mail… 7"));
        assert!(!text.contains("AAMkGraphId"));
    }

    #[test]
    fn plan_ui_folder_progress_complete_says_up_to_date() {
        let mut app = App::for_test_with_seeded_store();
        assert_eq!(app.selected_folder.as_deref(), Some("inbox"));
        app.on_sync_event(folder_progress("inbox", 10, None, false));
        assert!(!render(&app).contains("up to date"));
        app.on_sync_event(folder_progress("inbox", 12, None, true));
        let text = render(&app);
        assert!(text.contains("This folder is up to date"));
        assert!(!text.contains("Updating"));
        // A restarted walk is not up to date again.
        app.on_sync_event(folder_progress("inbox", 1, None, false));
        let text = render(&app);
        assert!(text.contains("Updating Inbox… 1"));
        assert!(!text.contains("up to date"));
        app.on_sync_event(folder_progress("inbox", 12, None, true));
        // Another folder's walk shows ahead of the selected one's state.
        app.on_sync_event(folder_progress("other", 5, None, false));
        let text = render(&app);
        assert!(text.contains("Updating mail… 5 — This folder is up to date"));
        // Only for the selected folder, and only in Mail mode.
        app.selected_folder = Some("other".into());
        assert!(!render(&app).contains("up to date"));
        app.selected_folder = Some("inbox".into());
        app.mode = crate::app::Mode::Calendar;
        assert!(!render(&app).contains("up to date"));
    }

    /// The engine walks only incomplete folders, so one synced in an
    /// earlier run gets no `complete` event: the store says it.
    #[test]
    fn plan_ui_folder_progress_up_to_date_from_the_store_at_load() {
        let mut app = App::for_test_with_seeded_store();
        assert!(!render(&app).contains("up to date"));
        // A delta link from before #1871 (no scope): complete in window mode.
        app.store.set_delta_link("inbox", "LINK").unwrap();
        app.reload_folders();
        assert!(render(&app).contains("This folder is up to date"));
        // Not in whole-mailbox mode (`main` seeds again once it sets it).
        app.whole_mailbox = true;
        app.seed_folders_up_to_date();
        assert!(!render(&app).contains("up to date"));
        app.store
            .complete_backfill("inbox", "FULL", mailcore::store::SCOPE_WINDOW, 1, None)
            .unwrap();
        app.reload_folders();
        assert!(
            !render(&app).contains("up to date"),
            "a window walk in whole mode"
        );
        app.store
            .complete_backfill("inbox", "FULL", mailcore::store::SCOPE_ALL, 1, None)
            .unwrap();
        app.reload_folders();
        assert!(render(&app).contains("This folder is up to date"));
        // A walk under way is kept, and its folder is not up to date, because
        // the store calls it incomplete (here: a `window` walk in whole mode).
        app.store
            .complete_backfill("inbox", "FULL", mailcore::store::SCOPE_WINDOW, 1, None)
            .unwrap();
        app.on_sync_event(folder_progress("inbox", 1, None, false));
        app.reload_folders();
        assert!(app.backfill.is_some());
        assert!(!render(&app).contains("up to date"));
    }

    /// r1-m2: a walk that stops with no `complete` event leaves no
    /// `Updating …` line behind.
    #[test]
    fn plan_ui_folder_progress_a_stopped_walk_leaves_no_line() {
        // Offline or signed out: no walk goes on.
        for state in [SyncState::Offline, SyncState::SignInRequired] {
            let mut app = App::for_test_with_seeded_store();
            app.on_sync_event(folder_progress("inbox", 10, None, false));
            app.on_sync_event(SyncEvent::State(state.clone()));
            assert!(app.backfill.is_none(), "{state:?}");
            assert!(!render(&app).contains("Updating"), "{state:?}");
        }
        // A pending state does not stop it.
        let mut app = App::for_test_with_seeded_store();
        app.on_sync_event(folder_progress("inbox", 10, None, false));
        app.on_sync_event(SyncEvent::State(SyncState::PendingOps(1)));
        assert!(app.backfill.is_some());
        // The store says the walked folder is complete (its `complete`
        // event never came): a reload drops the line and says up to date.
        app.store
            .complete_backfill("inbox", "FULL", mailcore::store::SCOPE_ALL, 1, None)
            .unwrap();
        app.reload_folders();
        assert!(app.backfill.is_none());
        let text = render(&app);
        assert!(!text.contains("Updating"));
        assert!(text.contains("This folder is up to date"));
        // A folder deleted mid-walk: a reload drops its line.
        app.on_sync_event(folder_progress("gone", 10, None, false));
        assert!(render(&app).contains("Updating mail… 10"));
        app.reload_folders();
        assert!(app.backfill.is_none());
        assert!(!render(&app).contains("Updating"));
    }

    /// #1959 plan-host-status-line: a multi-line error is one line.
    #[test]
    fn plan_host_status_line_multi_line_error_is_one_line() {
        let mut app = App::for_test_with_seeded_store();
        app.error_notice = Some("Parse error:\nline 2\r\n\tline 3".into());
        let rows = rows(&app, 120);
        assert!(
            rows[0].contains("Parse error: line 2 line 3"),
            "{}",
            rows[0]
        );
    }

    #[test]
    fn plan_host_status_line_long_error_is_cut_with_ellipsis() {
        let mut app = App::for_test_with_seeded_store();
        app.error_notice = Some(format!("Graph said: {}", "x".repeat(500)));
        let (line, _) = status_line(&app, 80);
        assert_eq!(line.width(), 80);
        assert!(line.ends_with('…'));
        assert!(line.contains("Graph said: x"));
        let rows = rows(&app, 80);
        assert!(rows[0].ends_with('…'), "{}", rows[0]);
        assert!(rows[1].trim().is_empty());
    }

    #[test]
    fn plan_host_status_line_error_drops_the_counts_first() {
        let mut app = App::for_test_with_seeded_store();
        app.error_notice = Some("Folder sync failed!!".into());
        // Fits whole: the counts stay.
        let (line, _) = status_line(&app, 120);
        assert!(line.contains("1 folder(s), 1 message(s) — Folder sync failed!!"));
        // 60 columns: the error wins the counts' room.
        let (line, _) = status_line(&app, 60);
        assert!(line.ends_with("Inbox — Folder sync failed!!"), "{line}");
        assert!(!line.contains("folder(s)"));
        assert!(!line.contains('…'));
    }

    #[test]
    fn one_line_turns_controls_and_whitespace_into_single_spaces() {
        assert_eq!(one_line("  a\n\n b\t\rc\u{7}d  "), "a b c d");
        assert_eq!(one_line("plain"), "plain");
        assert_eq!(one_line("\n\t"), "");
    }

    #[test]
    fn fit_line_cuts_by_display_columns() {
        assert_eq!(fit_line("abc", 3), "abc");
        assert_eq!(fit_line("abcd", 3), "ab…");
        assert_eq!(fit_line("abcd", 0), "");
        // Wide glyphs are 2 columns: 5 columns left for text holds two.
        let cut = fit_line("日本語テキスト", 6);
        assert_eq!(cut, "日本…");
        assert!(cut.width() <= 6);
    }

    #[test]
    fn group_thousands_and_human_bytes() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(48_210), "48,210");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(2048), "2.0 KB");
        assert_eq!(human_bytes(5 * MB), "5.0 MB");
        assert_eq!(human_bytes(3 * 1024 * MB), "3.0 GB");
    }
}
