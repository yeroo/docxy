//! The window's side of the Help tab (#1855): opening one (one per editor
//! per window, a new one right after the active tab), its keys, and the
//! presses its drawing routes here.

use super::{Follow, HelpView, Loc};
use crate::help_manual::{self, Bundle, Editor};
use crate::*;
use helpdocs::Via;
use std::sync::Arc;

/// The status line after Ctrl+S on a Help tab.
pub(crate) const HELP_NO_SAVE: &str = "Help pages are not saved; there is nothing to save";

/// The part name a Help tab's pictures are cached under (`DocTab::pictures`).
pub(crate) fn picture_part(editor: Editor) -> String {
    format!("help:{}", editor.name())
}

/// The manual a tab of `kind` reads (`None`: no tab, which reads Word's).
pub(crate) fn editor_for(kind: Option<Kind>) -> Editor {
    match kind {
        Some(Kind::Xlsx) => Editor::Excel,
        Some(Kind::Project) => Editor::Project,
        Some(Kind::Docx | Kind::Look | Kind::Help) | None => Editor::Word,
    }
}

/// Where a new Help tab goes: right after the active tab (at the end with
/// no tab).
pub(crate) fn insert_at(active: usize, len: usize) -> usize {
    if len == 0 { 0 } else { (active + 1).min(len) }
}

/// The index of this window's Help tab of `editor`, if one is open.
pub(crate) fn help_tab_of(tabs: &[DocTab], editor: Editor) -> Option<usize> {
    tabs.iter()
        .position(|t| matches!(&t.surface, Surface::Help(v) if v.editor() == editor))
}

/// A Help tab on `view`.
pub(crate) fn new_help_tab(view: HelpView) -> DocTab {
    let mut tab = blank_docx_tab(&view.title());
    tab.kind = Kind::Help;
    tab.status = "".into();
    tab.surface = Surface::Help(Box::new(view));
    tab
}

/// Keep a Help tab's title on its page.
fn retitle(tab: &mut DocTab) {
    if let Surface::Help(v) = &tab.surface {
        tab.title = v.title().into();
    }
}

impl Docxy {
    pub(crate) fn active_is_help(&self) -> bool {
        matches!(
            self.tabs.get(self.active).map(|t| &t.surface),
            Some(Surface::Help(_))
        )
    }

    pub(crate) fn active_help(&self) -> Option<&HelpView> {
        match self.tabs.get(self.active).map(|t| &t.surface) {
            Some(Surface::Help(v)) => Some(v),
            _ => None,
        }
    }

    /// The manual of `editor` this window may open, if one is installed.
    pub(crate) fn help_bundle(&self, editor: Editor) -> Option<Arc<Bundle>> {
        help_manual::load(editor, self.harness)
    }

    /// Change the active Help tab's view, then keep its title and redraw.
    /// `false` when the active tab is not a Help tab.
    pub(crate) fn with_help(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut HelpView),
    ) -> bool {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return false;
        };
        let Surface::Help(v) = &mut tab.surface else {
            return false;
        };
        f(v);
        retitle(tab);
        cx.notify();
        true
    }

    /// Open `bundle`'s manual at `at` in this window's Help tab of its
    /// editor (a history step there), or in a new one after the active tab,
    /// and bring it to the front. A dialog open on another tab stays there.
    pub(crate) fn open_help_at(
        &mut self,
        bundle: Arc<Bundle>,
        at: Loc,
        via: Option<Via>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.keytips = KeyTip::Off;
        self.backstage = false;
        let i = match help_tab_of(&self.tabs, bundle.editor) {
            Some(i) => {
                if let Surface::Help(v) = &mut self.tabs[i].surface {
                    v.navigate(at, via);
                }
                retitle(&mut self.tabs[i]);
                i
            }
            None => {
                let i = insert_at(self.active, self.tabs.len());
                let tab = new_help_tab(HelpView::new(bundle, at, via));
                self.tabs.insert(i, tab);
                i
            }
        };
        self.select_tab(i, window, cx);
        cx.notify();
    }

    /// A key on the active Help tab: history, the search box, scrolling.
    pub(crate) fn help_key(
        &mut self,
        ev: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::field_edit::Op;
        let m = ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        let ctrl = m.control || m.platform;
        // Ctrl+S (and Save from anywhere) has nothing to write.
        if crate::field_edit::is_save_chord(ctrl, m.alt, key) {
            return self.save_active(window, cx);
        }
        // Back and Forward: Alt+Left/Right, and ⌘[ / ⌘] on macOS.
        let history = match (key, m.alt && !ctrl && !m.shift, m.platform && !m.alt) {
            ("left", true, _) | ("[", _, true) => Some(false),
            ("right", true, _) | ("]", _, true) => Some(true),
            _ => None,
        };
        if let Some(forward) = history {
            self.with_help(cx, |v| {
                if forward {
                    v.go_forward();
                } else {
                    v.go_back();
                }
            });
            return;
        }
        let focused = self.active_help().is_some_and(|v| v.search_focused);
        // Ctrl+F (⌘F) goes to the search box, its text selected.
        if ctrl && !m.alt && key == "f" {
            self.with_help(cx, |v| {
                v.search_focused = true;
                crate::field_edit::apply(&mut v.query, &mut v.caret, &mut v.sel, Op::SelectAll);
            });
            return;
        }
        if focused {
            return self.help_search_key(ev, ctrl, m.alt, window, cx);
        }
        // The body: Page Up/Down, the arrows, Home and End scroll it.
        let Some(v) = self.active_help() else {
            return;
        };
        let handle = v.scroll.clone();
        let Some(to) = scroll_target(
            key,
            -f32::from(handle.offset().y),
            f32::from(handle.bounds().size.height),
            f32::from(handle.max_offset().y),
        ) else {
            return;
        };
        handle.set_offset(point(px(0.), px(-to)));
        cx.notify();
    }

    fn help_search_key(
        &mut self,
        ev: &KeyDownEvent,
        ctrl: bool,
        alt: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::field_edit::Op;
        let key = ev.keystroke.key.as_str();
        // Ctrl+Alt is AltGr on Windows: its keys are text, not chords.
        if ctrl && !alt {
            match key {
                "a" => {
                    self.with_help(cx, |v| v.edit_query(Op::SelectAll));
                }
                "c" | "x" => {
                    let Some(v) = self.active_help() else {
                        return;
                    };
                    let text = v.query.clone();
                    if v.sel && !text.is_empty() {
                        if key == "x" {
                            self.with_help(cx, |v| v.edit_query(Op::Delete));
                        }
                        self.clipboard_write(text.clone(), cx);
                        self.office_clip.push(&text);
                    }
                }
                "v" => {
                    if let crate::ClipRead::Text(text) = self.clipboard_read(cx) {
                        let line = crate::field_edit::one_line(&text);
                        if !line.is_empty() {
                            self.with_help(cx, |v| v.edit_query(Op::Insert(&line)));
                        }
                    }
                }
                _ => {}
            }
            cx.notify();
            return;
        }
        let op = match key {
            "escape" => {
                // A query clears first; an empty box gives the keys back.
                self.with_help(cx, |v| {
                    if v.query.is_empty() {
                        v.search_focused = false;
                    } else {
                        v.clear_query();
                    }
                });
                return;
            }
            "enter" => {
                self.with_help(cx, |v| {
                    if v.open_hit(0) {
                        v.search_focused = false;
                    }
                });
                return;
            }
            "tab" => {
                self.with_help(cx, |v| v.search_focused = false);
                return;
            }
            "backspace" => Op::Backspace,
            "delete" => Op::Delete,
            "left" => Op::Left,
            "right" => Op::Right,
            "home" => Op::Home,
            "end" => Op::End,
            _ => match ev.keystroke.key_char.as_deref() {
                Some(c) if !c.is_empty() && !c.chars().any(char::is_control) => {
                    let c = c.to_string();
                    self.with_help(cx, |v| v.edit_query(Op::Insert(&c)));
                    return;
                }
                _ => return,
            },
        };
        self.with_help(cx, |v| v.edit_query(op));
    }

    /// Back (`false`) or Forward (`true`) pressed.
    pub(crate) fn help_history(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.with_help(cx, |v| {
            if forward {
                v.go_forward();
            } else {
                v.go_back();
            }
        });
    }

    /// A press on the search box: it takes the keyboard.
    pub(crate) fn help_search_press(&mut self, cx: &mut Context<Self>) {
        self.with_help(cx, |v| {
            v.search_focused = true;
            v.sel = false;
            v.caret = v.query.chars().count();
        });
    }

    /// A press on the page body: the search box gives the keys back.
    pub(crate) fn help_body_press(&mut self, cx: &mut Context<Self>) {
        if self.active_help().is_some_and(|v| v.search_focused) {
            self.with_help(cx, |v| v.search_focused = false);
        }
    }

    /// A contents entry pressed.
    pub(crate) fn help_contents_press(&mut self, page: &str, cx: &mut Context<Self>) {
        let at = Loc::page(page);
        self.with_help(cx, |v| v.navigate(at, None));
    }

    /// Search hit `n` pressed.
    pub(crate) fn help_hit_press(&mut self, n: usize, cx: &mut Context<Self>) {
        self.with_help(cx, |v| {
            v.open_hit(n);
        });
    }

    /// The page's link `n` pressed: navigate, scroll, or open the browser.
    pub(crate) fn help_link_press(&mut self, n: usize, cx: &mut Context<Self>) {
        let mut followed = None;
        self.with_help(cx, |v| followed = v.follow_link(n));
        if let Some(Follow::External(url)) = followed {
            self.open_help_url(url, cx);
            self.set_status(format!(
                "Opened {} in your browser",
                url_host(&self.last_opened_url)
            ));
        }
    }
}

/// Where a key scrolls the body to, as a distance from the top: a line for
/// the arrows, a page less a line for Page Up/Down and Space, the ends for
/// Home and End. `None` for any other key.
pub(crate) fn scroll_target(key: &str, y: f32, view_h: f32, max: f32) -> Option<f32> {
    const LINE: f32 = 40.;
    let page = (view_h - LINE).max(LINE);
    let to = match key {
        "down" => y + LINE,
        "up" => y - LINE,
        "pagedown" | "space" => y + page,
        "pageup" => y - page,
        "home" => 0.,
        "end" => max,
        _ => return None,
    };
    Some(to.clamp(0., max.max(0.)))
}

/// The host of the URL last opened, for the status line.
fn url_host(url: &Option<String>) -> String {
    let url = url.as_deref().unwrap_or("");
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn a_tab_reads_its_editors_manual() {
        assert_eq!(editor_for(Some(Kind::Docx)), Editor::Word);
        assert_eq!(editor_for(Some(Kind::Look)), Editor::Word);
        assert_eq!(editor_for(None), Editor::Word);
        assert_eq!(editor_for(Some(Kind::Xlsx)), Editor::Excel);
        assert_eq!(editor_for(Some(Kind::Project)), Editor::Project);
        assert_eq!(picture_part(Editor::Excel), "help:excel");
    }

    #[test]
    fn a_new_help_tab_goes_right_after_the_active_one() {
        assert_eq!(insert_at(0, 0), 0);
        assert_eq!(insert_at(0, 1), 1);
        assert_eq!(insert_at(1, 4), 2);
        assert_eq!(insert_at(3, 4), 4);
        assert_eq!(insert_at(9, 4), 4, "a stale active index lands at the end");
    }

    #[test]
    fn one_help_tab_per_editor() {
        let word = Arc::new(help_manual::tests::fixture_bundle(Editor::Word));
        let excel = Arc::new(help_manual::tests::fixture_bundle(Editor::Excel));
        let tabs = vec![
            blank_docx_tab("a"),
            new_help_tab(HelpView::new(word.clone(), Loc::page("home"), None)),
            new_help_tab(HelpView::new(excel, Loc::page("home"), None)),
        ];
        assert_eq!(help_tab_of(&tabs, Editor::Word), Some(1));
        assert_eq!(help_tab_of(&tabs, Editor::Excel), Some(2));
        assert_eq!(help_tab_of(&tabs, Editor::Project), None);
        let t = &tabs[1];
        assert!(t.kind == Kind::Help && !t.dirty);
        assert_eq!(t.title.as_ref(), "Help: Home tab");
        assert!(hot_bytes(t).is_none(), "a Help tab saves as nothing");
    }

    #[test]
    fn a_help_tab_keeps_its_title_on_its_page() {
        let word = Arc::new(help_manual::tests::fixture_bundle(Editor::Word));
        let mut tab = new_help_tab(HelpView::new(word, Loc::page("home"), None));
        if let Surface::Help(v) = &mut tab.surface {
            v.navigate(Loc::page("tables"), None);
        }
        retitle(&mut tab);
        assert_eq!(tab.title.as_ref(), "Help: Tables");
    }

    #[test]
    fn keys_scroll_the_body_within_its_ends() {
        assert_eq!(scroll_target("down", 0., 500., 1000.), Some(40.));
        assert_eq!(scroll_target("up", 0., 500., 1000.), Some(0.));
        assert_eq!(scroll_target("pagedown", 100., 500., 1000.), Some(560.));
        assert_eq!(scroll_target("space", 900., 500., 1000.), Some(1000.));
        assert_eq!(scroll_target("pageup", 100., 500., 1000.), Some(0.));
        assert_eq!(scroll_target("end", 3., 500., 1000.), Some(1000.));
        assert_eq!(scroll_target("home", 300., 500., 1000.), Some(0.));
        assert_eq!(scroll_target("end", 0., 500., -1.), Some(0.));
        assert_eq!(scroll_target("a", 0., 500., 1000.), None);
    }

    #[test]
    fn the_status_names_the_host_an_external_link_opened() {
        assert_eq!(
            url_host(&Some("https://github.com/yeroo/docxy".into())),
            "github.com"
        );
        assert_eq!(url_host(&None), "");
    }
}
