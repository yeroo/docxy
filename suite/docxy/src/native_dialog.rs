//! Native (rfd) dialogs off the App borrow on Windows (#2194).
//!
//! On Windows, rfd's `IFileDialog::Show` and `MessageBox` run a modal loop
//! that pumps the main thread's messages, and gpui_windows runs foreground
//! tasks from those messages. Shown inside a listener, with the App
//! `RefCell` still borrowed, the first task dispatched while the dialog is
//! up runs nested in that borrow. A task that updates an entity through
//! `AsyncApp` then panics with "RefCell already borrowed" and aborts in
//! `window_procedure`, which cannot unwind. #2192 moved Open's picker off
//! the borrow ([`crate::open_mode::PickRoute::Deferred`]); [`Docxy::native_dialog`]
//! is the one way every other rfd dialog is shown, and Open's picker uses it
//! too on Windows and macOS.
//!
//! A site hands over the dialog, how to show it, and `then`: everything the
//! listener did after the answer. On the Inline route `then` runs before
//! `native_dialog` returns, exactly as the synchronous call did. On the
//! Deferred route it runs later, from the spawned task, so nothing the
//! caller does after `native_dialog` may depend on the answer.
use crate::Docxy;
use crate::close::TabId;
use gpui::{Context, Window};

/// Where a native dialog runs on this OS (#2194).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DialogRoute {
    /// Windows: shown from a foreground task spawned with `spawn_in`, which
    /// holds no App borrow (as gpui's own `prompt_for_paths` does), and owned
    /// by the suite window (`set_parent`), which is disabled while it is up.
    /// The answer comes back through `update_in`.
    Deferred,
    /// Linux and macOS: shown synchronously inside the listener, as before.
    /// Linux's portal call blocks without running gpui's tasks; macOS is the
    /// #2192 follow-up (not verified safe, not reported to crash).
    Inline,
}

/// The route for this OS (#2194).
pub(crate) const fn dialog_route() -> DialogRoute {
    if cfg!(target_os = "windows") {
        DialogRoute::Deferred
    } else {
        DialogRoute::Inline
    }
}

/// An rfd dialog an owner window can be set on.
pub(crate) trait OwnedDialog: Sized {
    /// The dialog, owned by `window`: modal to it, which is disabled while
    /// the dialog is up.
    fn owned_by(self, window: &Window) -> Self;
}

impl OwnedDialog for rfd::FileDialog {
    fn owned_by(self, window: &Window) -> Self {
        self.set_parent(window)
    }
}

impl OwnedDialog for rfd::MessageDialog {
    fn owned_by(self, window: &Window) -> Self {
        self.set_parent(window)
    }
}

impl Docxy {
    /// Whether the active tab is still the one `id` was taken of
    /// (`active_tab_id`), for an
    /// answer that acts on it. The window is disabled while the dialog is
    /// up, so it always is: this is defence, not a flow.
    pub(crate) fn active_tab_is(&self, id: &Option<TabId>) -> bool {
        self.active_tab_id() == *id
    }

    /// Show `dialog` with `show`, then hand its answer to `then` (#2194).
    ///
    /// On the Inline route both run now, under `native_modal`. On the
    /// Deferred route (Windows) the dialog is shown from a spawned task, and
    /// `then` runs once it is answered, followed by a `cx.notify()`, since no
    /// listener's return repaints after it. If the window is gone before the
    /// dialog shows, it is never shown; if it goes while the dialog is up,
    /// the answer is dropped.
    pub(crate) fn native_dialog<D: OwnedDialog + 'static, T: 'static>(
        &mut self,
        dialog: D,
        show: impl FnOnce(D) -> T + 'static,
        then: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match dialog_route() {
            DialogRoute::Deferred => {
                // ⚠️ Never call `show` before the spawn: here the App is
                // borrowed, and the dialog's modal loop runs gpui's tasks.
                let dialog = dialog.owned_by(window);
                cx.spawn_in(window, async move |this, cx| {
                    // The owner went between the spawn and this task.
                    if this.update_in(cx, |_, _, _| ()).is_err() {
                        return;
                    }
                    let answer = crate::macos_menu::native_modal(|| show(dialog));
                    let _ = this.update_in(cx, |this, window, cx| {
                        then(this, answer, window, cx);
                        cx.notify();
                    });
                })
                .detach();
            }
            DialogRoute::Inline => {
                let answer = crate::macos_menu::native_modal(|| show(dialog));
                then(self, answer, window, cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_mode::{PickRoute, open_pick_route};

    #[test]
    fn dialog_route_per_os() {
        let route = dialog_route();
        if cfg!(target_os = "windows") {
            assert_eq!(route, DialogRoute::Deferred);
        } else {
            assert_eq!(route, DialogRoute::Inline);
        }
    }

    /// Open's picker shows through `native_dialog` everywhere but Linux's
    /// Background arm, so the two routes must agree there.
    #[test]
    fn open_picker_and_other_dialogs_agree_off_linux() {
        let want = match open_pick_route() {
            PickRoute::Background => return,
            PickRoute::Deferred => DialogRoute::Deferred,
            PickRoute::Inline => DialogRoute::Inline,
        };
        assert_eq!(dialog_route(), want);
    }

    /// The `native_modal(` calls in `text`, by line.
    fn native_modal_lines(text: &str) -> Vec<usize> {
        text.match_indices("native_modal(")
            .filter(|(at, _)| !text[..*at].ends_with("fn "))
            .map(|(at, _)| text[..at].matches('\n').count() + 1)
            .collect()
    }

    /// The one `native_modal(` outside this module: Open's Linux picker on
    /// the background executor (#2192), which never runs inside a listener.
    const OPEN_BACKGROUND: &str =
        "spawn(async move { macos_menu::native_modal(|| dialog.pick_file()) })";

    /// Every native dialog goes through [`Docxy::native_dialog`] (#2194):
    /// no `native_modal(` call is left anywhere else in `src/`, but in
    /// `macos_menu.rs` (its definition and tests) and Open's Background arm.
    #[test]
    fn no_native_dialog_outside_the_helper() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = vec![src];
        let mut stray = Vec::new();
        let mut background = 0;
        while let Some(path) = files.pop() {
            if path.is_dir() {
                files.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs")
                || path.ends_with("macos_menu.rs")
                || path.ends_with("native_dialog.rs")
            {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let allowed: Vec<usize> = text
                .match_indices(OPEN_BACKGROUND)
                .map(|(at, _)| text[..at].matches('\n').count() + 1)
                .collect();
            for line in native_modal_lines(&text) {
                if allowed.contains(&line) {
                    background += 1;
                } else {
                    stray.push(format!("{}:{line}", path.display()));
                }
            }
        }
        assert!(
            stray.is_empty(),
            "native_modal( outside Docxy::native_dialog: {stray:?}"
        );
        assert_eq!(
            background, 1,
            "Open's Background arm moved: is the scan looking?"
        );
    }

    #[test]
    fn the_native_modal_scan_finds_calls_not_its_definition() {
        let text = "pub(crate) fn native_modal(run: F) {}\nlet a = native_modal(|| x);\n  macos_menu::native_modal(|| y)";
        assert_eq!(native_modal_lines(text), [2, 3]);
    }
}
