//! Menus: one model for what the window draws and what the harness reads
//! (#397). A menu is built by a pure builder when it opens (a right-click, a
//! split button's arrow, or `menu-open`, which calls the same opener), drawn
//! from that model, and an item is clicked through one entry point,
//! `Docxy::menu_activate`, whether the pointer or `menu-click` picks it.
use crate::Act;
use ctlcore::json::Json;

/// What a menu was opened on, as the harness names it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MenuTarget {
    /// The document body's context menu (document and sheet tabs).
    Document,
    /// A Project task row's context menu; `None` is the entry row.
    Row(Option<i32>),
    /// A ribbon split button's drop-down: tab, group and the primary's label.
    Ribbon {
        tab: String,
        group: String,
        label: String,
    },
}

impl MenuTarget {
    pub fn to_json(&self) -> Json {
        match self {
            Self::Document => Json::Str("document".into()),
            Self::Row(uid) => Json::obj(vec![(
                "row",
                uid.map_or(Json::Null, |u| Json::Num(u as f64)),
            )]),
            Self::Ribbon { tab, group, label } => Json::obj(vec![(
                "ribbon",
                Json::Arr(vec![
                    Json::Str(tab.clone()),
                    Json::Str(group.clone()),
                    Json::Str(label.clone()),
                ]),
            )]),
        }
    }
}

/// One clickable menu entry.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub id: String,
    pub label: String,
    /// An icon under `assets/icons`, or empty for none.
    pub icon: &'static str,
    pub key_tip: String,
    pub enabled: bool,
    pub checked: bool,
    /// The command it runs; an item with none is always disabled.
    pub act: Option<Act>,
    /// A submenu the item opens instead of running a command.
    pub submenu: Vec<MenuItem>,
}

impl Entry {
    /// An item that runs `act` when `enabled`.
    pub fn new(id: &str, label: &str, icon: &'static str, act: Act, enabled: bool) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon,
            key_tip: String::new(),
            enabled,
            checked: false,
            act: Some(act),
            submenu: Vec::new(),
        }
    }
    /// An item drawn for order parity with no command behind it: disabled.
    pub fn unavailable(id: &str, label: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: "",
            key_tip: String::new(),
            enabled: false,
            checked: false,
            act: None,
            submenu: Vec::new(),
        }
    }
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
    pub fn key(mut self, key_tip: &str) -> Self {
        self.key_tip = key_tip.into();
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) enum MenuItem {
    Item(Entry),
    Separator,
    /// A section heading (Office's `Built-In`); not clickable. Drawn and
    /// read already; no menu the suite opens has one yet (#486's drop-downs
    /// will).
    #[cfg_attr(not(test), allow(dead_code))]
    Heading(String),
}

/// An open menu: what it belongs to, where it is drawn (window coordinates)
/// and its items in order.
#[derive(Debug, Clone)]
pub(crate) struct Menu {
    pub target: MenuTarget,
    pub at: (f32, f32),
    pub items: Vec<MenuItem>,
}

impl Menu {
    /// The menu as `menu-open` and `menu-read` report it.
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("open", Json::Bool(true)),
            ("target", self.target.to_json()),
            ("items", items_json(&self.items)),
        ])
    }
}

fn items_json(items: &[MenuItem]) -> Json {
    Json::Arr(
        items
            .iter()
            .map(|item| match item {
                MenuItem::Separator => Json::obj(vec![("separator", Json::Bool(true))]),
                MenuItem::Heading(label) => Json::obj(vec![("heading", Json::Str(label.clone()))]),
                MenuItem::Item(e) => Json::obj(vec![
                    ("id", Json::Str(e.id.clone())),
                    ("label", Json::Str(e.label.clone())),
                    ("enabled", Json::Bool(e.enabled)),
                    ("checked", Json::Bool(e.checked)),
                    ("key_tip", Json::Str(e.key_tip.clone())),
                    (
                        "submenu",
                        if e.submenu.is_empty() {
                            Json::Null
                        } else {
                            items_json(&e.submenu)
                        },
                    ),
                ]),
            })
            .collect(),
    )
}

/// `menu-read`: the open menu, or `{open: false}`.
pub(crate) fn read_json(menu: Option<&Menu>) -> Json {
    menu.map_or_else(
        || Json::obj(vec![("open", Json::Bool(false))]),
        Menu::to_json,
    )
}

/// Whether an open menu still fits what it was opened on, before an item
/// runs: a row menu's task must still be the one selected on the active
/// Project, and the document menu is never run on a Project. `project` is
/// the active tab's selected task (`Some(None)` on the entry row), or `None`
/// off a Project. Whatever moves on from a menu closes it; this is the
/// check that an item never runs against a state it was not built for.
pub(crate) fn target_stands(
    target: &MenuTarget,
    project: Option<Option<i32>>,
) -> Result<(), String> {
    match (target, project) {
        (MenuTarget::Row(uid), Some(selected)) if selected == *uid => Ok(()),
        (MenuTarget::Row(_), _) => Err("the row menu's task is no longer the selected one".into()),
        (MenuTarget::Document, Some(_)) => {
            Err("the document menu does not run on a Project tab".into())
        }
        (MenuTarget::Document | MenuTarget::Ribbon { .. }, _) => Ok(()),
    }
}

/// The item a `menu-click` names: `{label}` among the top-level items, or
/// `{path}` of labels through submenus. Returns the index path of an item
/// that can be clicked, or why not.
pub(crate) fn resolve(items: &[MenuItem], path: &[&str]) -> Result<Vec<usize>, String> {
    let Some((last, walk)) = path.split_last() else {
        return Err("a menu path names at least one item".into());
    };
    let mut level = items;
    let mut out = Vec::new();
    for label in walk {
        let i = find(level, label)?;
        let MenuItem::Item(e) = &level[i] else {
            unreachable!("find returns items only")
        };
        if e.submenu.is_empty() {
            return Err(format!("menu item '{label}' has no submenu"));
        }
        if !e.enabled {
            return Err(format!("menu item '{label}' is disabled"));
        }
        out.push(i);
        level = &e.submenu;
    }
    let i = find(level, last)?;
    let MenuItem::Item(e) = &level[i] else {
        unreachable!("find returns items only")
    };
    if !e.submenu.is_empty() {
        return Err(format!(
            "menu item '{last}' opens a submenu; name an item in it with a path"
        ));
    }
    if !e.enabled {
        return Err(format!("menu item '{last}' is disabled"));
    }
    out.push(i);
    Ok(out)
}

/// The entry at an index path, if the path still names one.
pub(crate) fn entry_at<'a>(items: &'a [MenuItem], path: &[usize]) -> Option<&'a Entry> {
    let (last, walk) = path.split_last()?;
    let mut level = items;
    for &i in walk {
        match level.get(i)? {
            MenuItem::Item(e) => level = &e.submenu,
            _ => return None,
        }
    }
    match level.get(*last)? {
        MenuItem::Item(e) => Some(e),
        _ => None,
    }
}

/// One level's item by its label as drawn: unique, and an item, not a
/// heading.
fn find(level: &[MenuItem], label: &str) -> Result<usize, String> {
    let hits: Vec<usize> = level
        .iter()
        .enumerate()
        .filter(|(_, item)| matches!(item, MenuItem::Item(e) if e.label == label))
        .map(|(i, _)| i)
        .collect();
    match hits.as_slice() {
        [i] => Ok(*i),
        [] if level
            .iter()
            .any(|item| matches!(item, MenuItem::Heading(h) if h == label)) =>
        {
            Err(format!("'{label}' is a menu heading, not an item"))
        }
        [] => Err(format!(
            "no menu item '{label}'; items: {}",
            level
                .iter()
                .filter_map(|item| match item {
                    MenuItem::Item(e) => Some(e.label.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(", ")
        )),
        _ => Err(format!("menu item '{label}' is ambiguous")),
    }
}

/// The document body's context menu: clipboard and quick formatting.
pub(crate) fn document_menu() -> Vec<MenuItem> {
    use MenuItem::{Item, Separator};
    vec![
        Item(Entry::new("cm-cut", "Cut", "cut", Act::Cut, true)),
        Item(Entry::new("cm-copy", "Copy", "copy", Act::Copy, true)),
        Item(Entry::new("cm-paste", "Paste", "paste", Act::Paste, true)),
        Separator,
        Item(Entry::new("cm-bold", "Bold", "bold", Act::Bold, true)),
        Item(Entry::new(
            "cm-italic",
            "Italic",
            "italic",
            Act::Italic,
            true,
        )),
        Item(Entry::new(
            "cm-underline",
            "Underline",
            "underline",
            Act::Underline,
            true,
        )),
        Separator,
        Item(Entry::new(
            "cm-comment",
            "New Comment",
            "comment-add",
            Act::NewComment,
            true,
        )),
    ]
}

/// A split button's drop-down, from its menu commands on the ribbon.
pub(crate) fn split_menu(
    menu: &[ribbonspec::Cmd<Act>],
    enabled: impl Fn(Act) -> bool,
    checked: impl Fn(Act) -> bool,
) -> Vec<MenuItem> {
    menu.iter()
        .map(|c| {
            MenuItem::Item(
                Entry::new(c.id, c.label, c.icon.0, c.act, enabled(c.act))
                    .checked(checked(c.act))
                    .key(c.key_tip),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .map(|item| match item {
                MenuItem::Item(e) => e.label.clone(),
                MenuItem::Separator => "-".into(),
                MenuItem::Heading(h) => format!("[{h}]"),
            })
            .collect()
    }

    fn sample() -> Vec<MenuItem> {
        let mut insert = Entry::unavailable("insert", "Insert");
        insert.enabled = true;
        insert.submenu = vec![
            MenuItem::Heading("Built-In".into()),
            MenuItem::Item(Entry::new("it", "Insert Task", "", Act::Bold, true)),
            MenuItem::Item(Entry::new("im", "Insert Milestone", "", Act::Italic, false)),
        ];
        vec![
            MenuItem::Item(Entry::new("cut", "Cut", "", Act::Cut, true)),
            MenuItem::Separator,
            MenuItem::Item(insert),
            MenuItem::Item(Entry::unavailable("font", "Font...")),
            MenuItem::Item(Entry::new("d1", "Twice", "", Act::Copy, true)),
            MenuItem::Item(Entry::new("d2", "Twice", "", Act::Paste, true)),
            MenuItem::Heading("Section".into()),
        ]
    }

    #[test]
    fn a_label_resolves_to_its_item() {
        assert_eq!(resolve(&sample(), &["Cut"]), Ok(vec![0]));
    }

    #[test]
    fn a_path_walks_submenus() {
        let items = sample();
        let path = resolve(&items, &["Insert", "Insert Task"]).unwrap();
        assert_eq!(path, vec![2, 1]);
        assert_eq!(entry_at(&items, &path).unwrap().id, "it");
        // The last step's own refusals apply inside a submenu too.
        assert!(
            resolve(&items, &["Insert", "Insert Milestone"])
                .unwrap_err()
                .contains("disabled")
        );
        assert!(
            resolve(&items, &["Insert", "Built-In"])
                .unwrap_err()
                .contains("heading")
        );
        assert!(
            resolve(&items, &["Cut", "Anything"])
                .unwrap_err()
                .contains("has no submenu")
        );
    }

    #[test]
    fn what_cannot_be_clicked_is_refused_by_name() {
        let items = sample();
        assert!(
            resolve(&items, &["Font..."])
                .unwrap_err()
                .contains("disabled")
        );
        assert!(
            resolve(&items, &["Twice"])
                .unwrap_err()
                .contains("ambiguous")
        );
        assert!(
            resolve(&items, &["Section"])
                .unwrap_err()
                .contains("heading")
        );
        assert!(
            resolve(&items, &["Insert"])
                .unwrap_err()
                .contains("opens a submenu")
        );
        let unknown = resolve(&items, &["Paste"]).unwrap_err();
        assert!(unknown.contains("no menu item 'Paste'"), "{unknown}");
        assert!(unknown.contains("Cut, Insert, Font..."), "{unknown}");
        // A separator has no label: nothing names it.
        assert!(resolve(&items, &[""]).is_err());
        assert!(resolve(&items, &["-"]).is_err());
        assert!(resolve(&items, &[]).is_err());
    }

    #[test]
    fn menu_json_lists_items_separators_and_submenus_in_order() {
        let menu = Menu {
            target: MenuTarget::Row(Some(7)),
            at: (0., 0.),
            items: sample(),
        };
        let json = menu.to_json();
        assert_eq!(json.get("open"), Some(&Json::Bool(true)));
        assert_eq!(
            json.get("target").unwrap().get("row").unwrap().as_i64(),
            Some(7)
        );
        let items = json.get("items").unwrap().as_array().unwrap();
        assert_eq!(items[0].get_str("label"), Some("Cut"));
        assert_eq!(items[0].get("enabled"), Some(&Json::Bool(true)));
        assert_eq!(items[0].get("submenu"), Some(&Json::Null));
        assert_eq!(items[1].get("separator"), Some(&Json::Bool(true)));
        let sub = items[2].get("submenu").unwrap().as_array().unwrap();
        assert_eq!(sub[0].get_str("heading"), Some("Built-In"));
        assert_eq!(sub[2].get("enabled"), Some(&Json::Bool(false)));
        assert_eq!(items[3].get("enabled"), Some(&Json::Bool(false)));
        assert_eq!(
            MenuTarget::Ribbon {
                tab: "Project".into(),
                group: "Schedule".into(),
                label: "Set Baseline".into()
            }
            .to_json()
            .get("ribbon")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
            3
        );
        assert_eq!(MenuTarget::Document.to_json(), Json::Str("document".into()));
        assert_eq!(
            MenuTarget::Row(None).to_json().get("row"),
            Some(&Json::Null)
        );
    }

    #[test]
    fn menu_read_after_close_is_closed() {
        let mut open = Some(Menu {
            target: MenuTarget::Document,
            at: (0., 0.),
            items: document_menu(),
        });
        assert_eq!(
            read_json(open.as_ref()).get("open"),
            Some(&Json::Bool(true))
        );
        // `Docxy::close_menu` is this take.
        assert!(open.take().is_some(), "an open menu closes");
        let closed = read_json(open.as_ref());
        assert_eq!(closed, Json::obj(vec![("open", Json::Bool(false))]));
    }

    #[test]
    fn an_item_runs_only_against_the_target_its_menu_was_built_for() {
        let row = MenuTarget::Row(Some(3));
        assert_eq!(target_stands(&row, Some(Some(3))), Ok(()));
        for moved in [Some(Some(5)), Some(None), None] {
            assert!(
                target_stands(&row, moved)
                    .unwrap_err()
                    .contains("no longer the selected one")
            );
        }
        let entry = MenuTarget::Row(None);
        assert_eq!(target_stands(&entry, Some(None)), Ok(()));
        assert!(target_stands(&entry, Some(Some(1))).is_err());
        assert_eq!(target_stands(&MenuTarget::Document, None), Ok(()));
        assert!(target_stands(&MenuTarget::Document, Some(None)).is_err());
        let ribbon = MenuTarget::Ribbon {
            tab: "Project".into(),
            group: "Schedule".into(),
            label: "Set Baseline".into(),
        };
        assert_eq!(target_stands(&ribbon, Some(Some(1))), Ok(()));
        assert_eq!(target_stands(&ribbon, None), Ok(()));
    }

    #[test]
    fn the_document_menu_reads_as_drawn_before_the_model() {
        assert_eq!(
            labels(&document_menu()),
            [
                "Cut",
                "Copy",
                "Paste",
                "-",
                "Bold",
                "Italic",
                "Underline",
                "-",
                "New Comment"
            ]
        );
    }
}
