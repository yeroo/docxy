//! The sheet's drop-down and grid menus (#707): Home › Paste's gallery,
//! Fill, Clear and Find & Select, and the menus the grid opens (Auto Fill
//! Options, Paste Options, and a right-drag's drop menu). Each is built by a
//! pure function into the shared [`menu`](crate::menu) model, so the window
//! draws and the harness reads and clicks the same items, and each item runs
//! a [`SheetAct`] through `Act::Sheet`.

use crate::menu::{Entry, MenuItem};
use crate::{Act, SheetAct};
use gridcore::edit::{ClearWhat, FillDir, FillKind, PasteOp, PasteSpec, PasteWhat};

/// A sheet ribbon command that opens a menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SheetMenu {
    /// Home › Paste's arrow: the Paste gallery.
    Paste,
    Fill,
    Clear,
    FindSelect,
}

/// One item of the Paste gallery and of the Paste Options menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PasteItem {
    Paste,
    Formulas,
    FormulasNumberFormats,
    KeepSourceFormatting,
    NoBorders,
    KeepSourceColumnWidths,
    Transpose,
    Values,
    ValuesNumberFormats,
    ValuesSourceFormatting,
    Formatting,
    Link,
}

impl PasteItem {
    /// The gallery's items in its order, in its three sections.
    pub const SECTIONS: [&'static [PasteItem]; 3] = [
        &[
            PasteItem::Paste,
            PasteItem::Formulas,
            PasteItem::FormulasNumberFormats,
            PasteItem::KeepSourceFormatting,
            PasteItem::NoBorders,
            PasteItem::KeepSourceColumnWidths,
            PasteItem::Transpose,
        ],
        &[
            PasteItem::Values,
            PasteItem::ValuesNumberFormats,
            PasteItem::ValuesSourceFormatting,
        ],
        &[PasteItem::Formatting, PasteItem::Link],
    ];

    pub fn label(self) -> &'static str {
        match self {
            PasteItem::Paste => "Paste",
            PasteItem::Formulas => "Formulas",
            PasteItem::FormulasNumberFormats => "Formulas & Number Formatting",
            PasteItem::KeepSourceFormatting => "Keep Source Formatting",
            PasteItem::NoBorders => "No Borders",
            PasteItem::KeepSourceColumnWidths => "Keep Source Column Widths",
            PasteItem::Transpose => "Transpose",
            PasteItem::Values => "Values",
            PasteItem::ValuesNumberFormats => "Values & Number Formatting",
            PasteItem::ValuesSourceFormatting => "Values & Source Formatting",
            PasteItem::Formatting => "Formatting",
            PasteItem::Link => "Paste Link",
        }
    }

    fn id(self) -> &'static str {
        match self {
            PasteItem::Paste => "paste",
            PasteItem::Formulas => "paste-formulas",
            PasteItem::FormulasNumberFormats => "paste-formulas-number-formats",
            PasteItem::KeepSourceFormatting => "paste-keep-source",
            PasteItem::NoBorders => "paste-no-borders",
            PasteItem::KeepSourceColumnWidths => "paste-keep-widths",
            PasteItem::Transpose => "paste-transpose",
            PasteItem::Values => "paste-values",
            PasteItem::ValuesNumberFormats => "paste-values-number-formats",
            PasteItem::ValuesSourceFormatting => "paste-values-source",
            PasteItem::Formatting => "paste-formatting",
            PasteItem::Link => "paste-link",
        }
    }

    /// The Paste Special this item is; `None` for Paste Link, which is not
    /// one ([`gridcore::edit::paste_link_changes`]).
    pub fn spec(self) -> Option<PasteSpec> {
        let what = match self {
            PasteItem::Paste | PasteItem::KeepSourceFormatting | PasteItem::Transpose => {
                PasteWhat::All
            }
            PasteItem::Formulas => PasteWhat::Formulas,
            PasteItem::FormulasNumberFormats => PasteWhat::FormulasAndNumberFormats,
            PasteItem::NoBorders => PasteWhat::AllExceptBorders,
            PasteItem::KeepSourceColumnWidths => PasteWhat::AllAndColumnWidths,
            PasteItem::Values => PasteWhat::Values,
            PasteItem::ValuesNumberFormats => PasteWhat::ValuesAndNumberFormats,
            PasteItem::ValuesSourceFormatting => PasteWhat::ValuesAndSourceFormatting,
            PasteItem::Formatting => PasteWhat::Formats,
            PasteItem::Link => return None,
        };
        Some(PasteSpec {
            what,
            op: PasteOp::None,
            skip_blanks: false,
            transpose: self == PasteItem::Transpose,
        })
    }

    /// Whether it pastes from plain text (no grid clip): only Paste, Values
    /// and Transpose read text.
    pub fn takes_text(self) -> bool {
        matches!(
            self,
            PasteItem::Paste | PasteItem::Values | PasteItem::Transpose
        )
    }
}

fn item(id: &str, label: &str, act: SheetAct, enabled: bool) -> MenuItem {
    MenuItem::Item(Entry::new(id, label, "", Act::Sheet(act), enabled))
}

fn sep() -> MenuItem {
    MenuItem::Separator
}

/// The Paste gallery (Home › Paste's arrow): its sections, each item on
/// when `on` says the paste source takes it, then Paste Special…, on when
/// `special` (there is something to paste, and it is not a cut).
pub(crate) fn paste_gallery(on: impl Fn(PasteItem) -> bool, special: bool) -> Vec<MenuItem> {
    let mut items = Vec::new();
    for (k, section) in PasteItem::SECTIONS.iter().enumerate() {
        if k > 0 {
            items.push(sep());
        }
        for &p in *section {
            items.push(item(p.id(), p.label(), SheetAct::PasteAs(p), on(p)));
        }
    }
    items.push(sep());
    items.push(item(
        "paste-special",
        "Paste Special\u{2026}",
        SheetAct::PasteSpecial,
        special,
    ));
    items
}

/// The Paste Options button's menu: the gallery's items without Paste
/// Special, each pasting the last paste again that way.
pub(crate) fn paste_again_menu(current: PasteItem) -> Vec<MenuItem> {
    let mut items = Vec::new();
    for (k, section) in PasteItem::SECTIONS.iter().enumerate() {
        if k > 0 {
            items.push(sep());
        }
        for &p in *section {
            items.push(MenuItem::Item(
                Entry::new(
                    p.id(),
                    p.label(),
                    "",
                    Act::Sheet(SheetAct::PasteAgain(p)),
                    true,
                )
                .checked(p == current),
            ));
        }
    }
    items
}

/// Home › Fill.
pub(crate) fn fill_menu() -> Vec<MenuItem> {
    vec![
        item("fill-down", "Down", SheetAct::Fill(FillDir::Down), true),
        item("fill-right", "Right", SheetAct::Fill(FillDir::Right), true),
        item("fill-up", "Up", SheetAct::Fill(FillDir::Up), true),
        item("fill-left", "Left", SheetAct::Fill(FillDir::Left), true),
        sep(),
        item("fill-series", "Series\u{2026}", SheetAct::FillSeries, true),
        item("fill-justify", "Justify", SheetAct::FillJustify, true),
    ]
}

/// Home › Clear.
pub(crate) fn clear_menu() -> Vec<MenuItem> {
    ClearWhat::ALL
        .into_iter()
        .map(|w| {
            let id = match w {
                ClearWhat::All => "clear-all",
                ClearWhat::Formats => "clear-formats",
                ClearWhat::Contents => "clear-contents",
                ClearWhat::Comments => "clear-comments",
                ClearWhat::Hyperlinks => "clear-hyperlinks",
                ClearWhat::RemoveHyperlinks => "remove-hyperlinks",
            };
            item(id, w.label(), SheetAct::Clear(w), true)
        })
        .collect()
}

/// Home › Find & Select.
pub(crate) fn find_select_menu() -> Vec<MenuItem> {
    vec![
        item("goto", "Go To\u{2026}", SheetAct::GoTo, true),
        item(
            "goto-special",
            "Go To Special\u{2026}",
            SheetAct::GoToSpecial,
            true,
        ),
    ]
}

/// The kinds the Auto Fill Options button offers after a fill: Copy Cells
/// and Fill Series, the formatting pair, and for dates the date units, for
/// numbers the trends.
pub(crate) fn fill_kinds(dates: bool, numbers: bool) -> Vec<FillKind> {
    let mut kinds = vec![
        FillKind::Copy,
        FillKind::Series,
        FillKind::FormatsOnly,
        FillKind::WithoutFormatting,
    ];
    if dates {
        kinds.extend([
            FillKind::Days,
            FillKind::Weekdays,
            FillKind::Months,
            FillKind::Years,
        ]);
    }
    if numbers {
        kinds.extend([FillKind::LinearTrend, FillKind::GrowthTrend]);
    }
    kinds
}

/// The Auto Fill Options menu, `current` ticked: each item runs the last
/// fill again as its kind.
pub(crate) fn fill_options(kinds: &[FillKind], current: Option<FillKind>) -> Vec<MenuItem> {
    fill_kind_items(kinds, current, SheetAct::FillAs)
}

/// The fill handle's right-drag menu: the same kinds, each running the
/// drag that opened it. Its own act, so a choice from it can only ever run
/// that drag, and Auto Fill Options can never run it (#707 r1 M4).
pub(crate) fn fill_drop_menu(kinds: &[FillKind]) -> Vec<MenuItem> {
    fill_kind_items(kinds, None, SheetAct::FillDrop)
}

fn fill_kind_items(
    kinds: &[FillKind],
    current: Option<FillKind>,
    act: fn(FillKind) -> SheetAct,
) -> Vec<MenuItem> {
    kinds
        .iter()
        .map(|&k| {
            let id = format!("fill-{}", k.label().to_ascii_lowercase().replace(' ', "-"));
            MenuItem::Item(
                Entry::new(&id, k.label(), "", Act::Sheet(act(k)), true)
                    .checked(current == Some(k)),
            )
        })
        .collect()
}

/// What a right-drag of the selection's border offers on release.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DropChoice {
    #[default]
    Move,
    Copy,
    CopyValues,
    CopyFormats,
    Link,
    Cancel,
}

impl DropChoice {
    pub const ALL: [DropChoice; 6] = [
        DropChoice::Move,
        DropChoice::Copy,
        DropChoice::CopyValues,
        DropChoice::CopyFormats,
        DropChoice::Link,
        DropChoice::Cancel,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DropChoice::Move => "Move Here",
            DropChoice::Copy => "Copy Here",
            DropChoice::CopyValues => "Copy Here as Values Only",
            DropChoice::CopyFormats => "Copy Here as Formats Only",
            DropChoice::Link => "Link Here",
            DropChoice::Cancel => "Cancel",
        }
    }

    pub fn from_label(s: &str) -> Option<DropChoice> {
        DropChoice::ALL
            .into_iter()
            .find(|d| d.label().eq_ignore_ascii_case(s.trim()))
    }
}

/// The border right-drag's drop menu.
pub(crate) fn drop_menu() -> Vec<MenuItem> {
    DropChoice::ALL
        .into_iter()
        .enumerate()
        .flat_map(|(i, d)| {
            let id = format!("drop-{}", d.label().to_ascii_lowercase().replace(' ', "-"));
            let mut v = Vec::new();
            if i == DropChoice::ALL.len() - 1 {
                v.push(sep());
            }
            v.push(item(&id, d.label(), SheetAct::Drop(d), true));
            v
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::resolve;

    fn labels(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .filter_map(|i| match i {
                MenuItem::Item(e) => Some(e.label.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_gallery_is_excels_with_paste_special_last() {
        let g = paste_gallery(|_| true, true);
        let l = labels(&g);
        assert_eq!(l.len(), 13);
        assert_eq!(l[0], "Paste");
        assert_eq!(l.last().unwrap(), "Paste Special\u{2026}");
        assert_eq!(labels(&paste_again_menu(PasteItem::Paste)).len(), 12);
        assert!(resolve(&g, &["No Borders"]).is_ok());
    }

    #[test]
    fn the_gallery_turns_off_what_the_source_cannot_take() {
        let g = paste_gallery(PasteItem::takes_text, false);
        let on: Vec<String> = g
            .iter()
            .filter_map(|i| match i {
                MenuItem::Item(e) if e.enabled => Some(e.label.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(on, ["Paste", "Transpose", "Values"]);
        assert!(resolve(&g, &["Paste Special\u{2026}"]).is_err());
    }

    /// #707 r1 M4: the right-drag menu runs its own drag and Auto Fill
    /// Options its own fill: their items never share an act, so a dismissed
    /// drop menu leaves nothing an Options choice could run.
    #[test]
    fn the_drop_menu_and_auto_fill_options_run_different_acts() {
        let kinds = fill_kinds(false, true);
        let acts = |items: Vec<MenuItem>| -> Vec<SheetAct> {
            items
                .into_iter()
                .filter_map(|i| match i {
                    MenuItem::Item(e) => match e.act {
                        Some(Act::Sheet(a)) => Some(a),
                        _ => None,
                    },
                    _ => None,
                })
                .collect()
        };
        assert!(
            acts(fill_drop_menu(&kinds))
                .iter()
                .all(|a| matches!(a, SheetAct::FillDrop(_)))
        );
        assert!(
            acts(fill_options(&kinds, None))
                .iter()
                .all(|a| matches!(a, SheetAct::FillAs(_)))
        );
    }

    #[test]
    fn items_map_to_their_specs() {
        assert_eq!(
            PasteItem::NoBorders.spec().unwrap().what,
            PasteWhat::AllExceptBorders
        );
        assert!(PasteItem::Transpose.spec().unwrap().transpose);
        assert!(PasteItem::Link.spec().is_none());
    }

    #[test]
    fn fill_clear_and_find_menus() {
        assert_eq!(
            labels(&fill_menu()),
            ["Down", "Right", "Up", "Left", "Series\u{2026}", "Justify"]
        );
        assert_eq!(labels(&clear_menu()).len(), 6);
        assert_eq!(labels(&find_select_menu())[1], "Go To Special\u{2026}");
        let k = fill_kinds(false, true);
        assert!(k.contains(&FillKind::GrowthTrend) && !k.contains(&FillKind::Months));
        let opts = fill_options(&k, Some(FillKind::Series));
        assert!(matches!(&opts[1], MenuItem::Item(e) if e.checked));
        assert_eq!(labels(&drop_menu()).len(), 6);
        assert_eq!(DropChoice::from_label("link here"), Some(DropChoice::Link));
    }
}
