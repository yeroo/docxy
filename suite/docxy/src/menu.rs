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
    /// The document body's context menu (document tabs).
    Document,
    /// A sheet cell's context menu (#690, #691): clipboard, Sort and Filter.
    Cell,
    /// Pick From Drop-down List (Alt+Down, #665): the column block's
    /// distinct text entries over the selected cell.
    PickList,
    /// The Flash Fill Options button's menu (#666, ENT-109).
    FlashFill,

    /// A Project task row's context menu; `None` is the entry row.
    Row(Option<i32>),
    /// A ribbon split button's drop-down: tab, group and the primary's label
    /// (and its id, which the drawn arrow knows it by; not reported).
    Ribbon {
        id: String,
        tab: String,
        group: String,
        label: String,
    },
    /// A menu the sheet grid opens (#707): the Auto Fill Options and Paste
    /// Options buttons, and the menus a right-drag of the fill handle or of
    /// the selection's border opens on release.
    Grid(GridMenu),
    /// The Quick Access Toolbar Undo button's drop-down (#619): the undo
    /// history, newest first.
    QatUndo,
}

/// Which grid menu ([`MenuTarget::Grid`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GridMenu {
    FillOptions,
    PasteOptions,
    FillDrop,
    BorderDrop,
}

impl GridMenu {
    /// The name `menu-open {"grid": name}` takes and reports.
    pub fn name(self) -> &'static str {
        match self {
            GridMenu::FillOptions => "fill-options",
            GridMenu::PasteOptions => "paste-options",
            GridMenu::FillDrop => "fill-drop",
            GridMenu::BorderDrop => "border-drop",
        }
    }

    pub fn from_name(s: &str) -> Option<GridMenu> {
        [
            GridMenu::FillOptions,
            GridMenu::PasteOptions,
            GridMenu::FillDrop,
            GridMenu::BorderDrop,
        ]
        .into_iter()
        .find(|g| g.name() == s)
    }
}

/// The Quick Access Toolbar's Undo split button, whose arrow opens
/// [`MenuTarget::QatUndo`].
pub(crate) const QAT_UNDO_ID: &str = "qat-undo";

/// The most undo steps the Undo drop-down lists.
pub(crate) const UNDO_LIST_CAP: usize = 100;

impl MenuTarget {
    pub fn to_json(&self) -> Json {
        match self {
            Self::Document => Json::Str("document".into()),
            Self::Cell => Json::Str("cell".into()),
            Self::PickList => Json::Str("pick-list".into()),
            Self::FlashFill => Json::Str("flash-fill".into()),

            Self::Row(uid) => Json::obj(vec![(
                "row",
                uid.map_or(Json::Null, |u| Json::Num(u as f64)),
            )]),
            Self::Ribbon {
                tab, group, label, ..
            } => Json::obj(vec![(
                "ribbon",
                Json::Arr(vec![
                    Json::Str(tab.clone()),
                    Json::Str(group.clone()),
                    Json::Str(label.clone()),
                ]),
            )]),
            Self::Grid(g) => Json::obj(vec![("grid", Json::Str(g.name().into()))]),
            Self::QatUndo => Json::obj(vec![("qat", Json::Str(QAT_UNDO_ID.into()))]),
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
    /// Insert > Table's hover grid of `cols` × `rows` cells (#646): its
    /// header reads "Insert Table", or "<c>x<r> Table" for the cell under the
    /// pointer, and a click inserts that table. The `table-grid` verb hovers
    /// and clicks it through the same handlers.
    TableGrid {
        cols: usize,
        rows: usize,
    },
}

/// An open menu: what it belongs to, where it is drawn (window coordinates)
/// and its items in order.
#[derive(Debug, Clone)]
pub(crate) struct Menu {
    pub target: MenuTarget,
    pub at: (f32, f32),
    pub items: Vec<MenuItem>,
    /// The item Up and Down have highlighted (an index into `items`), for
    /// Enter to run; `None` until the first arrow.
    pub hi: Option<usize>,
}

impl Menu {
    /// A menu on `target` at `at`, nothing highlighted.
    pub fn new(target: MenuTarget, at: (f32, f32), items: Vec<MenuItem>) -> Self {
        Self {
            target,
            at,
            items,
            hi: None,
        }
    }

    /// The menu as `menu-open` and `menu-read` report it.
    pub fn to_json(&self) -> Json {
        Json::obj(vec![
            ("open", Json::Bool(true)),
            ("target", self.target.to_json()),
            ("items", items_json(&self.items)),
            (
                "highlight",
                self.hi.map_or(Json::Null, |i| Json::Num(i as f64)),
            ),
        ])
    }

    /// Down (`down`) or Up: the highlight moves to the next enabled item,
    /// over separators, headings and disabled items, wrapping at the ends as
    /// Office's menus do. With nothing highlighted, Down takes the first and
    /// Up the last. No enabled item: nothing is highlighted.
    pub fn step(&mut self, down: bool) {
        let n = self.items.len();
        let usable = |i: usize| matches!(&self.items[i], MenuItem::Item(e) if e.enabled);
        let order: Vec<usize> = if down {
            (0..n).collect()
        } else {
            (0..n).rev().collect()
        };
        let from = self
            .hi
            .and_then(|h| order.iter().position(|&i| i == h))
            .map_or(0, |p| p + 1);
        self.hi = (0..n).map(|k| order[(from + k) % n]).find(|&i| usable(i));
    }
}

fn items_json(items: &[MenuItem]) -> Json {
    Json::Arr(
        items
            .iter()
            .map(|item| match item {
                MenuItem::Separator => Json::obj(vec![("separator", Json::Bool(true))]),
                MenuItem::Heading(label) => Json::obj(vec![("heading", Json::Str(label.clone()))]),
                MenuItem::TableGrid { cols, rows } => Json::obj(vec![(
                    "table_grid",
                    Json::obj(vec![
                        ("columns", Json::Num(*cols as f64)),
                        ("rows", Json::Num(*rows as f64)),
                    ]),
                )]),
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
        (MenuTarget::Grid(_), Some(_)) => Err("a grid menu does not run on a Project tab".into()),
        (MenuTarget::Cell | MenuTarget::PickList | MenuTarget::FlashFill, Some(_)) => {
            Err("this menu runs on a sheet tab".into())
        }
        (MenuTarget::QatUndo, Some(_)) => Err("the undo list does not run on a Project tab".into()),
        (
            MenuTarget::Document
            | MenuTarget::Cell
            | MenuTarget::PickList
            | MenuTarget::FlashFill
            | MenuTarget::Ribbon { .. }
            | MenuTarget::Grid(_)
            | MenuTarget::QatUndo,
            _,
        ) => Ok(()),
    }
}

/// Whether a press on split button `id`'s arrow opens its menu. Office
/// toggles the drop-down: a press on the arrow of the menu that is open shuts
/// it. The backdrop's press handler may run first and close the menu before
/// the arrow's sees it, so the arrow is told both what is open now and what
/// this same press already closed.
pub(crate) fn split_arrow_opens(
    id: &str,
    open: Option<&MenuTarget>,
    closed_by_this_press: Option<&MenuTarget>,
) -> bool {
    let mine = |t: Option<&MenuTarget>| match t {
        Some(MenuTarget::Ribbon { id: i, .. }) => i == id,
        Some(MenuTarget::QatUndo) => id == QAT_UNDO_ID,
        _ => false,
    };
    !(mine(open) || mine(closed_by_this_press))
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

/// The item a `menu-click {"index": k}` names: the `k`th clickable item
/// (0-based, separators and headings not counted) among the top-level
/// items. Labels can repeat (two `Typing "a"` steps in the undo list, #619);
/// an index cannot.
pub(crate) fn resolve_index(items: &[MenuItem], k: usize) -> Result<Vec<usize>, String> {
    let entries: Vec<(usize, &Entry)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| match item {
            MenuItem::Item(e) => Some((i, e)),
            _ => None,
        })
        .collect();
    let Some((i, e)) = entries.get(k) else {
        return Err(format!(
            "no menu item at index {k}; the menu has {} items",
            entries.len()
        ));
    };
    if !e.submenu.is_empty() {
        return Err(format!(
            "menu item '{}' opens a submenu; name an item in it with a path",
            e.label
        ));
    }
    if !e.enabled {
        return Err(format!("menu item '{}' is disabled", e.label));
    }
    Ok(vec![*i])
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

/// A sheet cell's context menu: the clipboard, then Excel's Sort and Filter
/// submenus over the selected cell, and New Comment. Every item is a
/// [`crate::SheetAct`], so it runs through `run_sheet_act` as the sheet
/// ribbon's do, never the document's handlers.
pub(crate) fn cell_menu() -> Vec<MenuItem> {
    use crate::SheetAct as S;
    use crate::sheet_sort::OnTop;
    use MenuItem::{Item, Separator};
    use gridcore::filter::ByCell;
    let sheet = |id: &str, label: &str, act: S| Entry::new(id, label, "", Act::Sheet(act), true);
    let sub = |id: &str, label: &str, items: Vec<MenuItem>| Entry {
        submenu: items,
        ..Entry::unavailable(id, label)
    };
    vec![
        Item(Entry::new("cm-cut", "Cut", "cut", Act::Sheet(S::Cut), true)),
        Item(Entry::new(
            "cm-copy",
            "Copy",
            "copy",
            Act::Sheet(S::Copy),
            true,
        )),
        Item(Entry::new(
            "cm-paste",
            "Paste",
            "paste",
            Act::Sheet(S::Paste),
            true,
        )),
        Separator,
        Item(Entry {
            enabled: true,
            ..sub(
                "cm-filter",
                "Filter",
                vec![
                    Item(sheet("cm-reapply", "Reapply", S::ReapplyFilter)),
                    Separator,
                    Item(sheet(
                        "cm-filter-value",
                        "Filter by Selected Cell's Value",
                        S::FilterBy(ByCell::Value),
                    )),
                    Item(sheet(
                        "cm-filter-color",
                        "Filter by Selected Cell's Color",
                        S::FilterBy(ByCell::CellColor),
                    )),
                    Item(sheet(
                        "cm-filter-font",
                        "Filter by Selected Cell's Font Color",
                        S::FilterBy(ByCell::FontColor),
                    )),
                    Item(sheet(
                        "cm-filter-icon",
                        "Filter by Selected Cell's Icon",
                        S::FilterBy(ByCell::Icon),
                    )),
                ],
            )
        }),
        Item(Entry {
            enabled: true,
            ..sub(
                "cm-sort",
                "Sort",
                vec![
                    Item(sheet("cm-sort-a-z", "Sort A to Z", S::SortAsc)),
                    Item(sheet("cm-sort-z-a", "Sort Z to A", S::SortDesc)),
                    Separator,
                    Item(sheet(
                        "cm-top-color",
                        "Put Selected Cell Color On Top",
                        S::PutOnTop(OnTop::CellColor),
                    )),
                    Item(sheet(
                        "cm-top-font",
                        "Put Selected Font Color On Top",
                        S::PutOnTop(OnTop::FontColor),
                    )),
                    Item(sheet(
                        "cm-top-icon",
                        "Put Selected Cell Icon On Top",
                        S::PutOnTop(OnTop::Icon),
                    )),
                    Separator,
                    Item(sheet("cm-custom-sort", "Custom Sort...", S::CustomSort)),
                ],
            )
        }),
        Separator,
        Item(Entry::new(
            "cm-comment",
            "New Comment",
            "comment-add",
            Act::Sheet(S::NewComment),
            true,
        )),
        Separator,
        Item(sheet(
            "cm-pick-list",
            "Pick From Drop-down List...",
            S::PickList,
        )),
    ]
}

/// A sheet ribbon drop-down's menu (Sort & Filter), from the items of its
/// `sheet_ribbon` table entry. An item shows the text its entry gives it
/// (`Custom Sort...`) and runs the act the ribbon button would.
pub(crate) fn sheet_dropdown(items: &[crate::sheet_ribbon::SheetCmd]) -> Vec<MenuItem> {
    items
        .iter()
        .map(|c| {
            MenuItem::Item(Entry::new(
                c.id,
                c.text(false),
                "",
                Act::Sheet(c.act),
                c.enabled(),
            ))
        })
        .collect()
}

/// Pick From Drop-down List's menu (#665): one item per entry, in order
/// (gridcore bounds the list at `MENU_LIMIT`).
pub(crate) fn pick_menu(values: &[String]) -> Vec<MenuItem> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let at = u32::try_from(i).unwrap_or(u32::MAX);
            MenuItem::Item(Entry::new(
                &format!("pick-{i}"),
                v,
                "",
                Act::Sheet(crate::SheetAct::PickItem(at)),
                true,
            ))
        })
        .collect()
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

/// The Undo drop-down (#619): the undo steps' names, newest first (at most
/// [`UNDO_LIST_CAP`]); the `k`th undoes `k + 1` steps, back to and
/// including it. With nothing to undo, one disabled `Can't Undo`.
pub(crate) fn undo_menu(names: &[String]) -> Vec<MenuItem> {
    if names.is_empty() {
        return vec![MenuItem::Item(Entry::unavailable(
            "undo-none",
            "Can't Undo",
        ))];
    }
    names
        .iter()
        .take(UNDO_LIST_CAP)
        .enumerate()
        .map(|(k, name)| {
            MenuItem::Item(Entry::new(
                &format!("undo-{}", k + 1),
                name,
                "",
                Act::UndoTo(k + 1),
                true,
            ))
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
                MenuItem::TableGrid { .. } => "[grid]".into(),
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
    fn every_cell_menu_command_is_a_sheet_act() {
        fn walk(items: &[MenuItem], seen: &mut usize) {
            for item in items {
                if let MenuItem::Item(e) = item {
                    if e.submenu.is_empty() {
                        *seen += 1;
                        assert!(
                            matches!(e.act, Some(Act::Sheet(_))),
                            "{} runs {:?}",
                            e.label,
                            e.act
                        );
                    }
                    walk(&e.submenu, seen);
                }
            }
        }
        let mut seen = 0;
        walk(&cell_menu(), &mut seen);
        assert_eq!(seen, 16);
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
        let menu = Menu::new(MenuTarget::Row(Some(7)), (0., 0.), sample());
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
                id: "pr-baseline".into(),
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
    fn up_and_down_step_over_what_cannot_run() {
        // sample(): Cut, separator, Insert (submenu), Font... (disabled),
        // Twice, Twice, a heading.
        let mut m = Menu::new(MenuTarget::Document, (0., 0.), sample());
        assert_eq!(m.to_json().get("highlight"), Some(&Json::Null));
        m.step(true);
        assert_eq!(m.hi, Some(0), "Down from nothing: the first");
        m.step(true);
        assert_eq!(m.hi, Some(2), "over the separator");
        m.step(true);
        assert_eq!(m.hi, Some(4), "over the disabled item");
        m.step(true);
        m.step(true);
        assert_eq!(m.hi, Some(0), "over the heading, wrapping");
        m.step(false);
        assert_eq!(m.hi, Some(5), "Up wraps back");
        assert_eq!(m.to_json().get("highlight"), Some(&Json::Num(5.0)));
        let mut up = Menu::new(MenuTarget::Document, (0., 0.), sample());
        up.step(false);
        assert_eq!(up.hi, Some(5), "Up from nothing: the last that runs");
        let mut none = Menu::new(
            MenuTarget::Document,
            (0., 0.),
            vec![
                MenuItem::Separator,
                MenuItem::Item(Entry::unavailable("x", "X")),
            ],
        );
        none.step(true);
        assert_eq!(none.hi, None);
    }

    #[test]
    fn menu_read_after_close_is_closed() {
        let mut open = Some(Menu::new(MenuTarget::Document, (0., 0.), document_menu()));
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
            id: "pr-baseline".into(),
            tab: "Project".into(),
            group: "Schedule".into(),
            label: "Set Baseline".into(),
        };
        assert_eq!(target_stands(&ribbon, Some(Some(1))), Ok(()));
        assert_eq!(target_stands(&ribbon, None), Ok(()));
        let grid = MenuTarget::Grid(GridMenu::FillOptions);
        assert_eq!(target_stands(&grid, None), Ok(()));
        assert!(target_stands(&grid, Some(None)).is_err());
        assert_eq!(
            grid.to_json().get("grid").and_then(Json::as_str),
            Some("fill-options")
        );
        assert_eq!(
            GridMenu::from_name("border-drop"),
            Some(GridMenu::BorderDrop)
        );
    }

    #[test]
    fn a_split_arrow_toggles_its_own_menu_shut_in_either_handler_order() {
        let split = |id: &str| MenuTarget::Ribbon {
            id: id.into(),
            tab: "Project".into(),
            group: "Schedule".into(),
            label: "Set Baseline".into(),
        };
        let (mine, other) = (split("pr-baseline"), split("pr-other"));
        // Nothing open: the arrow opens its menu.
        assert!(split_arrow_opens("pr-baseline", None, None));
        // Its own menu open, the arrow's handler first: it shuts.
        assert!(!split_arrow_opens("pr-baseline", Some(&mine), None));
        // The backdrop's handler first closed it in this same press: it stays shut.
        assert!(!split_arrow_opens("pr-baseline", None, Some(&mine)));
        // Another menu open or just closed: this arrow opens its own.
        assert!(split_arrow_opens("pr-baseline", Some(&other), None));
        assert!(split_arrow_opens("pr-baseline", None, Some(&other)));
        assert!(split_arrow_opens(
            "pr-baseline",
            None,
            Some(&MenuTarget::Row(Some(1)))
        ));
    }

    #[test]
    fn the_qat_undo_arrow_toggles_its_own_menu_619() {
        let undo = MenuTarget::QatUndo;
        assert!(split_arrow_opens(QAT_UNDO_ID, None, None));
        assert!(!split_arrow_opens(QAT_UNDO_ID, Some(&undo), None));
        assert!(!split_arrow_opens(QAT_UNDO_ID, None, Some(&undo)));
        assert!(split_arrow_opens(
            QAT_UNDO_ID,
            Some(&MenuTarget::Document),
            None
        ));
        // A ribbon arrow is not the Undo arrow.
        assert!(split_arrow_opens("pr-baseline", Some(&undo), None));
        assert_eq!(undo.to_json().to_string(), r#"{"qat":"qat-undo"}"#);
        assert!(
            target_stands(&undo, Some(None)).is_err(),
            "never on a Project"
        );
        assert!(target_stands(&undo, None).is_ok());
    }

    #[test]
    fn the_undo_menu_lists_names_newest_first_and_undoes_back_to_the_pick_619() {
        let names: Vec<String> = ["Bold", "Typing \"two\"", "Enter", "Typing \"one\""]
            .map(String::from)
            .into();
        let items = undo_menu(&names);
        assert_eq!(labels(&items), names);
        let acts: Vec<usize> = items
            .iter()
            .map(|item| match item {
                MenuItem::Item(Entry {
                    act: Some(Act::UndoTo(n)),
                    enabled: true,
                    ..
                }) => *n,
                _ => panic!("every entry undoes"),
            })
            .collect();
        assert_eq!(acts, [1, 2, 3, 4]);
        let none = undo_menu(&[]);
        assert_eq!(labels(&none), ["Can't Undo"]);
        assert!(resolve_index(&none, 0).is_err(), "disabled");
        let many: Vec<String> = (0..150).map(|i| format!("Step {i}")).collect();
        assert_eq!(undo_menu(&many).len(), UNDO_LIST_CAP);
    }

    #[test]
    fn a_menu_item_resolves_by_index_when_labels_repeat_619() {
        let names: Vec<String> = ["Typing \"a\"", "Enter", "Typing \"a\""]
            .map(String::from)
            .into();
        let items = undo_menu(&names);
        assert!(
            resolve(&items, &["Typing \"a\""])
                .unwrap_err()
                .contains("ambiguous")
        );
        assert_eq!(resolve_index(&items, 2), Ok(vec![2]));
        assert_eq!(
            entry_at(&items, &[2])
                .unwrap()
                .act
                .map(|a| matches!(a, Act::UndoTo(3))),
            Some(true)
        );
        assert!(
            resolve_index(&items, 3)
                .unwrap_err()
                .contains("no menu item at index 3")
        );
        // Separators are not counted.
        let doc = document_menu();
        let path = resolve_index(&doc, 3).unwrap();
        assert_eq!(entry_at(&doc, &path).unwrap().label, "Bold");
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
