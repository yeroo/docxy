//! lookxy's ribbon: its command set (`Act`), its tab/button data, its cyan
//! accent, and its mail/calendar Home context — all rendered/navigated by the
//! shared [`ribboncore`] crate. The wrapper `Ribbon` derefs to
//! `ribboncore::Ribbon<Act>`, so every call site uses the core API directly.

use ratatui::style::Color;
use ribboncore::{Group as CoreGroup, Ribbon as CoreRibbon, btn, gap};

// Re-export the shared types so `crate::ui::ribbon::{Focus, Hit, Dir, EXPANDED_H}`
// keep resolving across the app.
pub use ribboncore::{Dir, EXPANDED_H, Focus, Hit};

/// lookxy's accent colour (docxy light blue, xlsxy green, yppxy yellow).
const ACCENT: Color = Color::Cyan;
/// The Home tab's index (contextual mail/calendar buttons).
const HOME_TAB: usize = 1;

/// A ribbon command. Each maps to an existing `App` method (see
/// `App::run_ribbon_act`); `Todo` ones are drawn dimmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    // Home (Mail)
    Compose,
    Reply,
    ReplyAll,
    Forward,
    Delete,
    Flag,
    MarkRead,
    MarkUnread,
    Move,
    Categorize,
    Find,
    // Home (Calendar)
    NewEvent,
    EditEvent,
    DeleteEvent,
    RsvpAccept,
    RsvpDecline,
    RsvpTentative,
    // Send / Receive
    SendReceive,
    // Folder
    ExpandAll,
    CollapseAll,
    // View
    Threaded,
    CategoryFilter,
    // Help
    Help,
    /// Not yet implemented; the `&str` is the feature name for the hint.
    Todo(&'static str),
}

type Group = CoreGroup<Act>;

/// lookxy's ribbon — a thin wrapper over the shared core carrying lookxy's
/// command type, so it can add the mail/calendar Home swap while every core
/// method (render/hit/nav/…) is reached by deref.
pub struct Ribbon(CoreRibbon<Act>);

impl Ribbon {
    pub fn new() -> Ribbon {
        let tabs = vec!["File", "Home", "Send/Receive", "Folder", "View", "Help"];
        let tab_groups = vec![
            Vec::new(), // File → backstage, no body
            home_groups(false),
            send_receive_groups(),
            folder_groups(),
            view_groups(),
            help_groups(),
        ];
        Ribbon(CoreRibbon::new(tabs, tab_groups, HOME_TAB, ACCENT))
    }

    /// Swap the Home tab between the mail and calendar button sets.
    pub fn set_home_context(&mut self, calendar: bool) {
        self.0.set_tab_groups(HOME_TAB, home_groups(calendar));
    }
}

impl Default for Ribbon {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Deref for Ribbon {
    type Target = CoreRibbon<Act>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for Ribbon {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// The Home tab's groups — mail actions, or (when `calendar`) event actions.
fn home_groups(calendar: bool) -> Vec<Group> {
    use Act::*;
    if calendar {
        return vec![
            Group {
                title: "New",
                width: 3,
                rows: [vec![btn("New", 3, NewEvent, "New event (c)")], vec![]],
            },
            Group {
                title: "Event",
                width: 8,
                rows: [
                    vec![
                        btn("Edit", 4, EditEvent, "Edit event (e)"),
                        gap(" "),
                        btn("Del", 3, DeleteEvent, "Delete event (x)"),
                    ],
                    vec![],
                ],
            },
            Group {
                title: "RSVP",
                width: 7,
                rows: [
                    vec![
                        btn("Acc", 3, RsvpAccept, "Accept (a)"),
                        gap(" "),
                        btn("Dec", 3, RsvpDecline, "Decline (d)"),
                    ],
                    vec![btn("Tent", 4, RsvpTentative, "Tentative (t)")],
                ],
            },
        ];
    }
    vec![
        Group {
            title: "New",
            width: 3,
            rows: [vec![btn("New", 3, Compose, "New message (c)")], vec![]],
        },
        Group {
            title: "Respond",
            width: 9,
            rows: [
                vec![
                    btn("Reply", 5, Reply, "Reply (r)"),
                    gap(" "),
                    btn("All", 3, ReplyAll, "Reply all (R)"),
                ],
                vec![btn("Fwd", 3, Forward, "Forward (F)")],
            ],
        },
        Group {
            title: "Manage",
            width: 13,
            rows: [
                vec![
                    btn("Del", 3, Delete, "Delete (d)"),
                    gap(" "),
                    btn("Flag", 4, Flag, "Flag (f)"),
                    gap(" "),
                    btn("Read", 4, MarkRead, "Mark read (m)"),
                ],
                vec![
                    btn("Unread", 6, MarkUnread, "Mark unread (u)"),
                    gap(" "),
                    btn("Move", 4, Move, "Move to folder (v)"),
                ],
            ],
        },
        Group {
            title: "Tools",
            width: 5,
            rows: [
                vec![btn("Label", 5, Categorize, "Categorize (l)")],
                vec![btn("Find", 4, Find, "Search (/)")],
            ],
        },
    ]
}

fn send_receive_groups() -> Vec<Group> {
    vec![Group {
        title: "Sync",
        width: 14,
        rows: [
            vec![btn(
                "Send & Receive",
                14,
                Act::SendReceive,
                "Sync mail and calendar now",
            )],
            vec![],
        ],
    }]
}

fn folder_groups() -> Vec<Group> {
    vec![Group {
        title: "Tree",
        width: 10,
        rows: [
            vec![btn("Expand All", 10, Act::ExpandAll, "Expand every folder")],
            vec![btn(
                "Collapse",
                8,
                Act::CollapseAll,
                "Collapse every folder",
            )],
        ],
    }]
}

fn view_groups() -> Vec<Group> {
    vec![Group {
        title: "Layout",
        width: 8,
        rows: [
            vec![btn(
                "Threaded",
                8,
                Act::Threaded,
                "Toggle threaded/flat view (t)",
            )],
            vec![btn(
                "Filter",
                6,
                Act::CategoryFilter,
                "Filter by category (L)",
            )],
        ],
    }]
}

fn help_groups() -> Vec<Group> {
    vec![Group {
        title: "Help",
        width: 9,
        rows: [
            vec![btn("Shortcuts", 9, Act::Help, "Keyboard shortcuts (F1)")],
            vec![btn("About", 5, Act::Todo("About"), "About lookxy")],
        ],
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ribboncore::Seg;

    fn content_w(row: &[Seg<Act>]) -> usize {
        row.iter()
            .map(|s| match s {
                Seg::Gap(g) => g.chars().count(),
                Seg::Btn(b) => b.width,
            })
            .sum()
    }

    #[test]
    fn every_group_is_wide_enough_for_its_content() {
        for calendar in [false, true] {
            let tabs = [
                home_groups(calendar),
                send_receive_groups(),
                folder_groups(),
                view_groups(),
                help_groups(),
            ];
            for groups in tabs {
                for g in &groups {
                    for row in &g.rows {
                        assert!(
                            g.width >= content_w(row),
                            "group {:?} width {} < content {}",
                            g.title,
                            g.width,
                            content_w(row)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn home_has_six_tabs_and_mail_actions() {
        let r = Ribbon::new();
        assert_eq!(r.tab_label(0), Some("File"));
        assert_eq!(r.tab_label(5), Some("Help"));
        assert!(r.has_act(Act::Compose));
        assert!(r.button_count() > 0);
        assert!(!r.tab_has_body(0)); // File opens the backstage
    }

    #[test]
    fn idle_tab_names_use_the_cyan_accent() {
        let r = Ribbon::new();
        let line = r.render_tabs(Focus::None);
        let file = line.spans.iter().find(|s| s.content == "File").unwrap();
        assert_eq!(file.style.fg, Some(ACCENT));
    }

    #[test]
    fn set_home_context_swaps_to_calendar_actions() {
        let mut r = Ribbon::new();
        r.set_active(HOME_TAB);
        assert!(r.has_act(Act::Compose));
        r.set_home_context(true);
        assert!(r.has_act(Act::NewEvent));
        assert!(!r.has_act(Act::Compose));
    }
}
