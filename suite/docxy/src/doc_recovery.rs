//! Document Recovery (#1225): on the relaunch after a crash, Word's choice
//! between each recovered document's AutoRecover copy and its original.
//!
//! Restore already reopens every unsaved tab from its AutoRecover copy
//! ([`crate::restore_session_noting`]). This dialog lists the ones that have
//! an original on disk, all checked: a checked one keeps its recovered
//! changes, an unchecked one is replaced by its file as last saved. A modal
//! on the active tab's stack, so `dialog-set` and `dialog-click` drive it;
//! its OK is the app's, as the user name's is ([`crate::user_name`]).
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::dialog::catalog;
use crate::dialog::{Control, ControlKind, Dialog, DialogOwner, DialogStack, Value};
use crate::open_mode::{Converted, OpenMode};
use crate::page_setup::ok_cancel;
use crate::{DocTab, Docxy, Surface};

/// A tab restore labelled as an AutoRecover copy: its index in the restored
/// session, its file (none for a never-saved one), and when its copy was
/// written.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Recovered {
    pub index: usize,
    pub path: Option<PathBuf>,
    pub saved: Option<SystemTime>,
}

/// One item of the dialog: a recovered tab whose original is on disk.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Candidate {
    pub index: usize,
    pub path: PathBuf,
    pub label: String,
}

/// The status of a tab replaced by its original.
pub(crate) const ORIGINAL_OPENED: &str = "original opened — the AutoRecover copy was discarded";

/// The recovered tabs the dialog offers, in tab order: those with a file
/// that still exists. An untitled one has no original to go back to, and a
/// missing one none to open; both keep the recovered status.
pub(crate) fn candidates(
    recovered: &[Recovered],
    tabs: &[DocTab],
    now: SystemTime,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = recovered
        .iter()
        .filter_map(|r| {
            let path = r.path.clone().filter(|p| p.exists())?;
            let tab = tabs.get(r.index)?;
            let folder = path
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let label = format!(
                "{} — {folder} — AutoRecover copy {}",
                tab.title,
                crate::recover::recovered_age(r.saved, now)
            );
            Some(Candidate {
                index: r.index,
                path,
                label,
            })
        })
        .collect();
    out.sort_by_key(|c| c.index);
    out
}

/// The dialog over `items`, every one checked: OK with nothing changed
/// keeps every recovered copy, as Cancel does.
pub(crate) fn dialog(items: &[Candidate]) -> Dialog {
    let mut d = Dialog::message(
        catalog::DOCUMENT_RECOVERY,
        "Document Recovery",
        "docxy closed unexpectedly, and these documents were reopened from their \
         AutoRecover copies. Checked documents keep their recovered changes; unchecked \
         ones open as last saved, and their recovered changes are discarded."
            .into(),
        &[],
        DialogOwner::DocumentRecovery,
    );
    let mut list = Control::new(
        "documents",
        "Documents",
        ControlKind::CheckList,
        Value::Checks(vec![true; items.len()]),
    );
    list.items = items.iter().map(|c| c.label.clone()).collect();
    list.depths = vec![0; items.len()];
    d.controls = vec![list];
    d.buttons = ok_cancel();
    d.mark_opened();
    // The list has the focus, so Space toggles its first item at once.
    d.focus_step(false);
    d
}

/// What an OK hands back: each item's check, in item order.
pub(crate) type Checks = Vec<bool>;

/// A press of `button` on `dialogs` when the dialog on top is this one:
/// `None` for any other dialog. Cancel closes it; OK closes it and hands
/// back each item's check.
pub(crate) fn click(
    dialogs: &mut DialogStack,
    button: &str,
) -> Option<(Result<(), String>, Option<Checks>)> {
    if dialogs.top()?.owner != DialogOwner::DocumentRecovery {
        return None;
    }
    let mut accepted = None;
    let done = dialogs.click(button, |d| {
        accepted = Some(match d.value("documents") {
            Some(Value::Checks(c)) => c.clone(),
            _ => Vec::new(),
        });
        Ok(())
    });
    Some((done, accepted))
}

/// Where a discarded workbook's last AutoRecover copy is kept as a draft,
/// as a Don't Save close keeps it (#613).
pub(crate) struct Drafts<'a> {
    pub root: &'a Path,
    pub minutes: u32,
    pub keep: bool,
    pub now: SystemTime,
}

/// What [`apply`] did: the tabs it replaced, and whether it kept a draft.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Applied {
    pub replaced: Vec<usize>,
    pub drafts_kept: bool,
}

/// The mode a recovered tab's original opens in: the one the tab was opened
/// in, so a repaired workbook is repaired again rather than refused.
fn mode_of(tab: &DocTab) -> OpenMode {
    let a = &tab.access;
    if a.repaired {
        OpenMode::Repair
    } else if a.converted == Some(Converted::RecoveredText) {
        OpenMode::RecoverText
    } else if a.read_only {
        OpenMode::ReadOnly
    } else {
        OpenMode::Normal
    }
}

/// Replace each unchecked item's tab by its original. `chosen` is the
/// items' tabs (index and path) as the dialog opened, `keep` their checks.
/// An item whose tab no longer has that path, or is no longer unsaved, is
/// skipped. A tab whose original does not open, or opens as something
/// other than that file (a template opens untitled), keeps its recovered
/// copy and says why; so does a workbook whose draft could not be kept.
pub(crate) fn apply(
    tabs: &mut [DocTab],
    chosen: &[(usize, PathBuf)],
    keep: &[bool],
    trusted: &crate::trusted::TrustStore,
    drafts: &Drafts,
) -> Applied {
    let mut out = Applied::default();
    for ((index, path), &kept) in chosen.iter().zip(keep) {
        if kept {
            continue;
        }
        let Some(tab) = tabs.get_mut(*index) else {
            continue;
        };
        if tab.path.as_deref() != Some(path.as_path()) || !tab.dirty {
            continue;
        }
        let fresh = match crate::tab_from_path_mode(path, mode_of(tab), trusted) {
            Err(e) => Err(e),
            Ok(t) if t.load_failed || matches!(t.surface, Surface::Placeholder) => {
                Err(t.status.to_string())
            }
            Ok(t) if t.path.as_deref() != Some(path.as_path()) => {
                Err("it opens as a new document".into())
            }
            Ok(t) => Ok(t),
        };
        let mut fresh = match fresh {
            Ok(t) => t,
            Err(why) => {
                tab.status = format!(
                    "recovered — the original could not be opened ({why}); this is the \
                     AutoRecover copy: Save to keep it"
                )
                .into();
                continue;
            }
        };
        // Kept before the tab goes: the persist after this deletes the
        // sidecar once no session names it (#1224).
        let sidecar = tab.last_hot.borrow().clone();
        if crate::close::should_keep_draft(
            tab.kind,
            drafts.minutes,
            drafts.keep,
            true,
            sidecar.as_deref(),
        ) {
            let s = sidecar
                .as_deref()
                .expect("should_keep_draft needs a sidecar");
            match crate::recover::keep_draft(drafts.root, &tab.title, s, drafts.now) {
                Ok(_) => out.drafts_kept = true,
                // Discarding the copy now would lose its only one.
                Err(e) => {
                    tab.status = format!(
                        "recovered — the original was not opened: {e}; this is the \
                         AutoRecover copy: Save to keep it"
                    )
                    .into();
                    continue;
                }
            }
        }
        fresh.status = ORIGINAL_OPENED.into();
        *tab = fresh;
        out.replaced.push(*index);
    }
    out
}

impl Docxy {
    /// After a crash: open the dialog on the active tab's stack over the
    /// recovered tabs that have an original, if any do.
    pub(crate) fn open_document_recovery(&mut self, recovered: &[Recovered], now: SystemTime) {
        let items = candidates(recovered, &self.tabs, now);
        if items.is_empty() {
            return;
        }
        let d = dialog(&items);
        self.active_dialogs_mut().push(d);
        self.recovery = items.into_iter().map(|c| (c.index, c.path)).collect();
    }

    /// [`click`] on the active stack; an OK replaces the unchecked tabs by
    /// their originals and persists the session, which then no longer names
    /// their AutoRecover copies.
    pub(crate) fn document_recovery_click(
        &mut self,
        button: &str,
        cx: &mut gpui::App,
    ) -> Option<Result<(), String>> {
        let (done, accepted) = click(self.active_dialogs_mut(), button)?;
        if done.is_err() {
            return Some(done);
        }
        let chosen = std::mem::take(&mut self.recovery);
        let Some(keep) = accepted else {
            return Some(done);
        };
        let root = crate::config_root();
        let trusted = crate::trusted::TrustStore::load(&root);
        let drafts = Drafts {
            root: &root,
            minutes: self.autorecover_minutes,
            keep: self.keep_drafts,
            now: SystemTime::now(),
        };
        let applied = apply(&mut self.tabs, &chosen, &keep, &trusted, &drafts);
        if applied.drafts_kept {
            self.refresh_drafts(cx);
        }
        if !applied.replaced.is_empty() {
            // The grid under the active tab's UI state is another one now.
            if applied.replaced.contains(&self.active) {
                self.drop_grid_state();
            }
            self.persist(cx);
        }
        Some(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialog::ButtonRole;
    use ctlcore::json::Json;

    fn items() -> Vec<Candidate> {
        ["a.docx", "b.xlsx"]
            .iter()
            .enumerate()
            .map(|(index, name)| Candidate {
                index,
                path: PathBuf::from(name),
                label: format!("{name} — here — AutoRecover copy from 3 min ago"),
            })
            .collect()
    }

    fn opened() -> DialogStack {
        let mut stack = DialogStack::default();
        stack.push(dialog(&items()));
        stack
    }

    #[test]
    fn the_dialog_lists_every_candidate_checked() {
        let d = dialog(&items());
        assert_eq!(d.id, catalog::DOCUMENT_RECOVERY);
        assert_eq!(d.title, "Document Recovery");
        assert!(d.text.as_deref().unwrap().contains("unchecked"));
        let list = &d.controls[0];
        assert_eq!(list.name, "documents");
        assert_eq!(list.kind, ControlKind::CheckList);
        assert_eq!(
            list.items,
            items().iter().map(|c| c.label.clone()).collect::<Vec<_>>()
        );
        assert_eq!(list.value, Value::Checks(vec![true, true]));
        assert_eq!(d.focused().map(|c| c.name), Some("documents"));
        let ok = d.buttons.iter().find(|b| b.default).unwrap();
        assert_eq!((ok.label.as_str(), ok.role), ("OK", ButtonRole::Accept));
        assert!(d.buttons.iter().any(|b| b.role == ButtonRole::Cancel));
    }

    #[test]
    fn ok_hands_back_each_item_s_check() {
        let mut stack = opened();
        let label = items()[1].label.clone();
        stack
            .set(
                "documents",
                &Json::obj(vec![
                    ("item", Json::Str(label)),
                    ("checked", Json::Bool(false)),
                ]),
            )
            .unwrap();
        let (done, accepted) = click(&mut stack, "OK").expect("ours");
        done.unwrap();
        assert_eq!(accepted, Some(vec![true, false]));
        assert!(!stack.is_open());
    }

    #[test]
    fn cancel_closes_and_keeps_every_copy() {
        let mut stack = opened();
        let (done, accepted) = click(&mut stack, "Cancel").expect("ours");
        done.unwrap();
        assert_eq!(accepted, None);
        assert!(!stack.is_open());
    }

    #[test]
    fn another_dialog_is_not_ours() {
        let mut stack = DialogStack::default();
        stack.push(Dialog::message(
            catalog::TEST_T,
            "T",
            String::new(),
            &[("OK", ButtonRole::Accept)],
            DialogOwner::Test,
        ));
        assert!(click(&mut stack, "OK").is_none());
        assert!(stack.is_open());
    }

    #[test]
    fn enter_and_escape_press_through_the_app() {
        assert!(crate::dialog_host::presses_through_app(
            &DialogOwner::DocumentRecovery
        ));
    }
}
