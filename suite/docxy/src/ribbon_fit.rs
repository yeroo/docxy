//! How a ribbon tab's groups fit the window (#1020).
//!
//! Office scales a ribbon in steps as the window narrows, and never hides a
//! command: first groups drop their button labels (icon-only), one at a time,
//! lowest priority first; then groups collapse, lowest priority first, each to
//! one large button whose flyout shows the whole group; and when even the
//! collapsed buttons do not fit, the leftovers go into one trailing chevron
//! whose flyout lists them. Both suite ribbons (the ribbonspec model of the
//! document and Project ribbons, and the spreadsheet's `sheet_ribbon` table)
//! fit through [`fit`], from widths estimated here.
//!
//! The estimates are the renderers' own arithmetic: each control's padding,
//! gaps and fixed sizes as `Docxy::render_group` / `Docxy::sheet_group` draw
//! them, and its text at the width the window's text system shapes it (`tw`;
//! the unit tests pass a character-count approximation). `ribbon-layout`
//! reports each drawn group's width beside its estimate, so drift shows (#357).

use crate::sheet_ribbon::{self, Body, Item, Shape, SheetCmd};
use ribbonspec::{self as rs, Control};

/// The ribbon body's horizontal padding (`px_1` each side) and a little slack
/// for rounding: what a tab's groups may not use.
pub(crate) const GUTTER: f32 = 12.0;
/// The trailing overflow chevron's width.
pub(crate) const CHEVRON_W: f32 = 28.0;
/// The narrowest collapsed button.
const COLLAPSED_MIN_W: f32 = 48.0;
/// The collapsed button's horizontal padding (`px_1p5` each side).
const COLLAPSED_PAD: f32 = 12.0;
/// The collapsed button's title size, as a group title is drawn.
pub(crate) const COLLAPSED_TITLE_PX: f32 = 10.0;

/// A text measure: `tw(text, size_px)` is the drawn width of `text`.
pub(crate) type TextWidth<'a> = &'a dyn Fn(&str, f32) -> f32;

/// Whether a sheet command's state is on now (`Docxy::sheet_act_toggled`):
/// Freeze Panes then reads Unfreeze Panes, and is measured so.
pub(crate) type Toggled<'a> = &'a dyn Fn(crate::SheetAct) -> bool;

/// How a group is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// Every control with its label.
    Full,
    /// Row buttons drop their labels (tooltips keep the names).
    IconOnly,
    /// One large button that opens the whole group in a flyout.
    Collapsed,
}

impl State {
    pub(crate) fn name(self) -> &'static str {
        match self {
            State::Full => "full",
            State::IconOnly => "icon-only",
            State::Collapsed => "collapsed",
        }
    }
}

/// A group's priority and its width in each state, in px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Spec {
    pub priority: u8,
    pub full: f32,
    /// Equal to `full` for a group with no label to drop.
    pub icon: f32,
    pub collapsed: f32,
}

impl Spec {
    pub(crate) fn width(&self, state: State) -> f32 {
        match state {
            State::Full => self.full,
            State::IconOnly => self.icon,
            State::Collapsed => self.collapsed,
        }
    }
}

/// Where every group of a tab went.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Fit {
    /// One per group, in drawn order.
    pub states: Vec<State>,
    /// The collapsed groups listed in the trailing chevron instead of drawn,
    /// in drawn order; the chevron is drawn only when this is not empty.
    pub overflow: Vec<usize>,
}

impl Fit {
    /// Whether group `i` is drawn as a collapsed button or listed in the
    /// chevron: either way its commands are reached through its flyout.
    pub(crate) fn in_flyout(&self, i: usize) -> bool {
        self.states.get(i) == Some(&State::Collapsed)
    }

    pub(crate) fn in_overflow(&self, i: usize) -> bool {
        self.overflow.contains(&i)
    }

    /// The width the drawn groups (and the chevron) take.
    #[cfg(test)]
    pub(crate) fn laid_out(&self, specs: &[Spec]) -> f32 {
        let groups: f32 = specs
            .iter()
            .enumerate()
            .filter(|(i, _)| !self.in_overflow(*i))
            .map(|(i, s)| s.width(self.states[i]))
            .sum();
        groups
            + if self.overflow.is_empty() {
                0.0
            } else {
                CHEVRON_W
            }
    }
}

/// The tab's groups fitted into a ribbon `width` px wide.
///
/// Two passes, as the issue orders them: groups go icon-only one at a time,
/// lowest priority first, until the tab fits; only then do groups collapse,
/// in the same order. Among equal priorities the rightmost goes first. If the
/// collapsed buttons still overflow, the lowest-priority ones move to the
/// chevron, but one group always stays drawn.
pub(crate) fn fit(specs: &[Spec], width: f32) -> Fit {
    let avail = width - GUTTER;
    let mut states = vec![State::Full; specs.len()];
    let mut order: Vec<usize> = (0..specs.len()).collect();
    order.sort_by_key(|&i| (specs[i].priority, std::cmp::Reverse(i)));
    let total =
        |states: &[State]| -> f32 { specs.iter().zip(states).map(|(s, st)| s.width(*st)).sum() };
    for &i in &order {
        if total(&states) <= avail {
            break;
        }
        if specs[i].icon < specs[i].full {
            states[i] = State::IconOnly;
        }
    }
    for &i in &order {
        if total(&states) <= avail {
            break;
        }
        states[i] = State::Collapsed;
    }
    let mut overflow = Vec::new();
    if total(&states) > avail {
        let mut drawn = total(&states);
        for &i in &order {
            if drawn + CHEVRON_W <= avail || overflow.len() + 1 >= specs.len() {
                break;
            }
            overflow.push(i);
            drawn -= specs[i].collapsed;
        }
        overflow.sort_unstable();
    }
    Fit { states, overflow }
}

/// A large button's label lines, as `label_lines` wraps them.
fn lines_w(text: &str, size: f32, tw: TextWidth) -> f32 {
    crate::label_lines(text)
        .iter()
        .map(|l| tw(l, size))
        .fold(0.0, f32::max)
}

/// `menu_button_label`'s width: the label's lines at `size` px, the last
/// with " ▾".
pub(crate) fn menu_label_w(text: &str, size: f32, tw: TextWidth) -> f32 {
    let lines = crate::label_lines(text);
    let last = lines.len().saturating_sub(1);
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if i == last {
                tw(&format!("{l} \u{25BE}"), size)
            } else {
                tw(l, size)
            }
        })
        .fold(0.0, f32::max)
}

/// The collapsed button's width for a group titled `title`: its title as a
/// menu button's label (the last line with the drop-down mark) between its
/// padding, at least [`COLLAPSED_MIN_W`], plus the right divider. The button
/// is drawn exactly this wide.
pub(crate) fn collapsed_w(title: &str, tw: TextWidth) -> f32 {
    let text = menu_label_w(title, COLLAPSED_TITLE_PX, tw);
    (text + COLLAPSED_PAD).ceil().max(COLLAPSED_MIN_W) + 1.0
}

/// Greedy rows of items `widths` wide, `gap` apart, each row at most `max_w`
/// (an item wider than that gets a row to itself): how a flyout wraps a
/// group too wide for the window, every item at its full size (#1020).
pub(crate) fn wrap_rows(widths: &[f32], gap: f32, max_w: f32) -> Vec<std::ops::Range<usize>> {
    let mut rows = Vec::new();
    let (mut start, mut w) = (0, 0.0);
    for (i, &iw) in widths.iter().enumerate() {
        if i > start && w + gap + iw > max_w {
            rows.push(start..i);
            (start, w) = (i, iw);
        } else {
            w += if i > start { gap + iw } else { iw };
        }
    }
    if start < widths.len() {
        rows.push(start..widths.len());
    }
    rows
}

/// A gallery's tile pitch: tile width, the gap between tiles, and the
/// well's padding and border on both sides together.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GalleryGeom {
    pub tile_w: f32,
    pub tile_h: f32,
    pub gap: f32,
    pub frame: f32,
}

/// The Styles gallery's (`style_gallery::TILE`) or the Table Styles
/// gallery's (`table_style_gallery`: 4x12 px cells, 3 px padding and a 2 px
/// border per tile, 2 px apart, in a well with 2 px padding and a 1 px border).
pub(crate) fn gallery_geom<A>(gal: &rs::Gallery<A>) -> GalleryGeom {
    if gal.id == "tablestyles" {
        GalleryGeom {
            tile_w: 58.0,
            tile_h: 4.0 * 8.0 + 10.0,
            gap: 2.0,
            frame: 6.0,
        }
    } else {
        let t = crate::style_gallery::TILE;
        GalleryGeom {
            tile_w: t.w,
            tile_h: t.h,
            gap: t.gap,
            frame: 2.0 * (t.well_pad + t.well_border),
        }
    }
}

/// How many of a gallery's `n` tiles go on a row at most `max_w` wide (all
/// of them with no limit, at least one), and the well's width then.
pub(crate) fn gallery_row(geom: GalleryGeom, n: usize, max_w: Option<f32>) -> (usize, f32) {
    let width = |k: usize| {
        let k = k as f32;
        k * geom.tile_w + (k - 1.0).max(0.0) * geom.gap + geom.frame
    };
    let mut k = n.max(1);
    if let Some(max_w) = max_w {
        while k > 1 && width(k) > max_w {
            k -= 1;
        }
    }
    (k, width(k))
}

// ---- the ribbonspec model (document and Project ribbons) ----

/// `icon_btn`: a 16px icon between `px_2` and a 1px border each side; with
/// its label, a `gap_1p5` and the label at 12px.
fn model_icon_btn_w(label: Option<&str>, tw: TextWidth) -> f32 {
    34.0 + label.map_or(0.0, |l| 6.0 + tw(l, 12.0))
}

/// One control's drawn width; a gallery wraps its tiles to `max_w` (a
/// flyout's limit) when one is given.
pub(crate) fn model_control_w<A>(
    c: &Control<A>,
    icon_only: bool,
    max_w: Option<f32>,
    tw: TextWidth,
) -> f32 {
    match c {
        Control::Toggle(_) => model_icon_btn_w(None, tw),
        // `large_btn`: `px_2` around the 26px icon or the label's lines.
        Control::Large(cmd) => 16.0 + lines_w(cmd.label, 11.0, tw).max(26.0),
        // `split_btn` and `dropdown_btn`: `px_1` around the icon or the
        // drop-down label.
        Control::Split { primary: cmd, .. } | Control::Dropdown { cmd, .. } => {
            8.0 + menu_label_w(cmd.label, 11.0, tw).max(26.0)
        }
        // Columns of at most three buttons, `gap_1` apart.
        Control::Column(cmds) => {
            let cols: Vec<f32> = cmds
                .chunks(rs::MAX_COLUMN_ROWS)
                .map(|chunk| {
                    chunk
                        .iter()
                        .map(|c| model_icon_btn_w((!icon_only).then_some(c.label), tw))
                        .fold(0.0, f32::max)
                })
                .collect();
            cols.iter().sum::<f32>() + 4.0 * (cols.len().saturating_sub(1)) as f32
        }
        // Rows of buttons and combos 1px apart; the widest row sets the width.
        Control::Rows(rows) => rows
            .iter()
            .map(|row| {
                let cells: f32 = row
                    .iter()
                    .map(|cell| match cell {
                        rs::Cell::Combo { wide: true, .. } => 104.0,
                        rs::Cell::Combo { wide: false, .. } => 46.0,
                        rs::Cell::Btn(_) => model_icon_btn_w(None, tw),
                    })
                    .sum();
                cells + row.len().saturating_sub(1) as f32
            })
            .fold(0.0, f32::max),
        Control::Gallery(gal) => gallery_row(gallery_geom(gal), gal.items.len(), max_w).1,
        // A 1px rule with `mx_1`.
        Control::Separator => 9.0,
    }
}

/// `render_group`'s width: its controls `gap_1` apart, or its title row when
/// that is wider, between `px_2` and the 1px right divider.
pub(crate) fn model_group_w<A>(g: &rs::Group<A>, icon_only: bool, tw: TextWidth) -> f32 {
    let controls: f32 = g
        .items
        .iter()
        .map(|c| model_control_w(c, icon_only, None, tw))
        .sum::<f32>()
        + 4.0 * g.items.len().saturating_sub(1) as f32;
    let title = tw(g.title, 9.0)
        + g.launcher
            .as_ref()
            .map_or(0.0, |_| 4.0 + tw("\u{2922}", 10.0));
    controls.max(title) + 16.0 + 1.0
}

pub(crate) fn model_spec<A>(g: &rs::Group<A>, tw: TextWidth) -> Spec {
    Spec {
        priority: g.priority,
        full: model_group_w(g, false, tw),
        icon: model_group_w(g, true, tw),
        collapsed: collapsed_w(g.title, tw),
    }
}

/// The icon a collapsed model group shows: its first command's.
pub(crate) fn model_group_icon<A>(g: &rs::Group<A>) -> Option<&'static str> {
    g.items.iter().find_map(|c| match c {
        Control::Toggle(cmd) | Control::Large(cmd) => Some(cmd.icon.0),
        Control::Split { primary: cmd, .. } | Control::Dropdown { cmd, .. } => Some(cmd.icon.0),
        Control::Column(cmds) => cmds.first().map(|c| c.icon.0),
        Control::Rows(rows) => rows.iter().flatten().find_map(|cell| match cell {
            rs::Cell::Btn(cmd) => Some(cmd.icon.0),
            rs::Cell::Combo { .. } => None,
        }),
        Control::Gallery(_) | Control::Separator => None,
    })
}

// ---- the spreadsheet ribbon ----

/// Whether a sheet command drops its label when its group goes icon-only:
/// a small row button with an icon (AutoSum, Fill, Clear, Cut, Copy).
pub(crate) fn sheet_cmd_drops_label(c: &SheetCmd) -> bool {
    matches!(c.shape, Shape::Row(Some(_)))
}

/// One sheet command's drawn width (`Docxy::sheet_cmd_el`).
pub(crate) fn sheet_cmd_w(c: &SheetCmd, icon_only: bool, on: Toggled, tw: TextWidth) -> f32 {
    let text = c.text(on(c.act));
    match c.shape {
        // `sheet_dropdown_btn`: `px_1p5` around a 22px icon or the label.
        Shape::Menu(_) => (12.0 + menu_label_w(c.label, 11.0, tw).max(22.0)).max(40.0),
        // `sheet_lb`: `px_1p5` around a 22px icon or the label's 10px lines.
        Shape::Large(_) | Shape::Split { .. } => {
            (12.0 + lines_w(text, 10.0, tw).max(22.0)).max(40.0)
        }
        // `sheet_rb`: `px_1`, a 14px icon, `gap_1p5`, the 11px label.
        Shape::Row(icon) => {
            let icon_w = if icon.is_some() { 14.0 } else { 0.0 };
            if icon_only && icon.is_some() {
                8.0 + icon_w
            } else {
                8.0 + icon_w + if icon.is_some() { 6.0 } else { 0.0 } + tw(text, 11.0)
            }
        }
        // `sheet_check`: `px_1`, the 12px box, `gap_1`, the 11px label.
        Shape::Check => 8.0 + tw("\u{2610}", 12.0) + 4.0 + tw(text, 11.0),
        Shape::Icon(_) => 22.0,
        Shape::Glyph(g) => (8.0 + tw(g, 12.0)).max(22.0),
        Shape::Combo { wide, .. } => {
            if wide {
                108.0
            } else {
                50.0
            }
        }
        Shape::NumFmt => 108.0,
    }
}

/// One slot of a sheet strip: a command, a column (its widest command), or
/// a drop-down button.
pub(crate) fn sheet_item_w(item: &Item, icon_only: bool, on: Toggled, tw: TextWidth) -> f32 {
    match item {
        Item::One(c) => sheet_cmd_w(c, icon_only, on, tw),
        Item::Col(col) => col
            .cmds
            .iter()
            .map(|c| sheet_cmd_w(c, icon_only, on, tw))
            .fold(0.0, f32::max),
        Item::Menu(m) => sheet_cmd_w(&m.button, icon_only, on, tw),
    }
}

pub(crate) fn gap_px(g: sheet_ribbon::Gap) -> f32 {
    match g {
        sheet_ribbon::Gap::Px(v) => v,
        sheet_ribbon::Gap::Rem(v) => v * 16.0,
    }
}

/// `sheet_group`'s width: its body, or its title row when that is wider,
/// between `px_1p5` and the 1px right divider.
pub(crate) fn sheet_group_w(
    g: &sheet_ribbon::Group,
    titles: sheet_ribbon::Titles,
    icon_only: bool,
    on: Toggled,
    tw: TextWidth,
) -> f32 {
    let body = match &g.body {
        Body::Strip { gap, items } => {
            let widths: f32 = items
                .iter()
                .map(|item| sheet_item_w(item, icon_only, on, tw))
                .sum();
            widths + gap_px(*gap) * items.len().saturating_sub(1) as f32
        }
        Body::Rows(r) => r
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|c| sheet_cmd_w(c, icon_only, on, tw))
                    .sum::<f32>()
                    + 2.0 * row.len().saturating_sub(1) as f32
            })
            .fold(0.0, f32::max),
    };
    let title = tw(g.title, 10.0)
        + match titles {
            sheet_ribbon::Titles::WithLaunchers if g.launcher => 4.0 + tw("\u{2921}", 9.0),
            _ => 0.0,
        };
    body.max(title) + 12.0 + 1.0
}

pub(crate) fn sheet_spec(
    g: &sheet_ribbon::Group,
    titles: sheet_ribbon::Titles,
    on: Toggled,
    tw: TextWidth,
) -> Spec {
    let full = sheet_group_w(g, titles, false, on, tw);
    let drops = g.commands().iter().any(|c| sheet_cmd_drops_label(c));
    Spec {
        priority: g.priority,
        full,
        icon: if drops {
            sheet_group_w(g, titles, true, on, tw)
        } else {
            full
        },
        collapsed: collapsed_w(g.title, tw),
    }
}

/// The icon a collapsed sheet group shows: its first command's.
pub(crate) fn sheet_group_icon(g: &sheet_ribbon::Group) -> Option<&'static str> {
    g.commands().iter().find_map(|c| match c.shape {
        Shape::Large(i) | Shape::Row(i) | Shape::Menu(i) | Shape::Split { icon: i, .. } => i,
        Shape::Icon(i) => Some(i),
        _ => None,
    })
}

/// A character-count text measure for tests: about the average advance of
/// the UI font.
#[cfg(test)]
pub(crate) fn approx_tw(text: &str, size: f32) -> f32 {
    text.chars().count() as f32 * size * 0.56
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Act, Kind};

    fn tw(text: &str, size: f32) -> f32 {
        approx_tw(text, size)
    }

    fn off(_: crate::SheetAct) -> bool {
        false
    }

    /// #1020 r2: a command whose label reads differently while its state is
    /// on (Freeze Panes, Unfreeze Panes) is measured as it reads now.
    #[test]
    fn a_toggled_label_is_measured_as_drawn() {
        let view = sheet_ribbon::tab_def(crate::RibbonTab::View);
        let freeze = view
            .commands()
            .into_iter()
            .find(|c| c.alt.is_some())
            .expect("View has Freeze Panes");
        let on = |_: crate::SheetAct| true;
        assert_eq!(
            sheet_cmd_w(freeze, false, &on, &tw),
            sheet_cmd_w(
                &SheetCmd {
                    text: freeze.alt,
                    alt: None,
                    ..*freeze
                },
                false,
                &off,
                &tw
            ),
        );
        assert!(sheet_cmd_w(freeze, false, &on, &tw) > sheet_cmd_w(freeze, false, &off, &tw));
    }

    /// Every ribbon tab the suite draws, by name: the document's and the
    /// Project's (contextual tabs too) from the model, the workbook's from
    /// the sheet table, each as its groups' specs.
    fn every_tab() -> Vec<(String, Vec<Spec>)> {
        let mut model: Vec<rs::Tab<Act>> = Vec::new();
        model.extend(crate::ribbon_for(Kind::Docx).tabs);
        model.extend(crate::ribbon_for(Kind::Project).tabs);
        model.push(crate::table_tab::table_design_tab());
        model.push(crate::table_tab::table_layout_tab());
        model.push(crate::hf_tab::hf_tab());
        model.push(crate::gantt_format_tab());
        let mut out: Vec<(String, Vec<Spec>)> = model
            .iter()
            .map(|t| {
                let specs = t.groups.iter().map(|g| model_spec(g, &tw)).collect();
                (format!("model {}", t.name), specs)
            })
            .collect();
        for t in sheet_ribbon::SHEET_RIBBON {
            let specs = t
                .groups
                .iter()
                .map(|g| sheet_spec(g, t.titles, &off, &tw))
                .collect();
            out.push((format!("sheet {}", crate::ribbon_tab_name(t.tab)), specs));
        }
        out
    }

    fn widths() -> impl Iterator<Item = f32> {
        (400..=2000).step_by(20).map(|w| w as f32)
    }

    /// #1020: at every width from 400 to 2000 px, every group of every tab is
    /// drawn (whole, or icon-only, which drops only labels) or collapsed (its
    /// button, or an entry of the chevron, opens it whole in a flyout), so
    /// every command of the tab is reachable; and what is laid out fits.
    #[test]
    fn fit_never_hides_a_command() {
        for (name, specs) in every_tab() {
            for width in widths() {
                let fit = fit(&specs, width);
                assert_eq!(fit.states.len(), specs.len(), "{name} at {width}");
                for &i in &fit.overflow {
                    assert!(fit.in_flyout(i), "{name} at {width}: chevron group {i}");
                }
                if !specs.is_empty() {
                    assert!(
                        fit.overflow.len() < specs.len(),
                        "{name} at {width}: a group stays drawn"
                    );
                }
                assert!(
                    fit.laid_out(&specs) <= width - GUTTER,
                    "{name} at {width}: {} laid out",
                    fit.laid_out(&specs)
                );
            }
        }
    }

    /// The commands a sheet group draws as an icon alone (icon-only) are the
    /// ones with an icon: a label is never dropped from a button with none.
    #[test]
    fn icon_only_drops_no_label_a_button_needs() {
        for t in sheet_ribbon::SHEET_RIBBON {
            for g in t.groups {
                for c in g.commands() {
                    if sheet_cmd_drops_label(c) {
                        assert!(matches!(c.shape, Shape::Row(Some(_))), "{}", c.id);
                    }
                }
            }
        }
    }

    /// The order groups collapse in, narrowing one px at a time down to where
    /// all are collapsed. At every width the collapsed groups are the first
    /// of the priority order (lowest first, the rightmost of a tie), and a
    /// group once collapsed stays so.
    fn collapse_order(name: &str, specs: &[Spec]) -> Vec<usize> {
        let mut order: Vec<usize> = (0..specs.len()).collect();
        order.sort_by_key(|&i| (specs[i].priority, std::cmp::Reverse(i)));
        let mut was = 0;
        for width in (100..=3000).rev() {
            let fit = fit(specs, width as f32);
            let n = (0..specs.len()).filter(|&i| fit.in_flyout(i)).count();
            assert!(n >= was, "{name}: a group came back at {width}");
            assert!(
                order[..n].iter().all(|&i| fit.in_flyout(i)),
                "{name} at {width}: {:?} collapsed out of order",
                fit.states
            );
            was = n;
        }
        assert_eq!(was, specs.len(), "{name}: all collapse at 100 px");
        order
    }

    fn model_tab(kind: Kind, name: &str) -> rs::Tab<Act> {
        crate::ribbon_for(kind)
            .tabs
            .into_iter()
            .find(|t| t.name == name)
            .unwrap()
    }

    /// #1020: groups collapse in priority order, and on Home Clipboard and
    /// Font are the last to go (Clipboard last); on Project's Task tab,
    /// Clipboard is.
    #[test]
    fn collapse_order_follows_priority() {
        let doc = model_tab(Kind::Docx, "Home");
        let task = model_tab(Kind::Project, "Task");
        let sheet = sheet_ribbon::tab_def(crate::RibbonTab::Home);
        let cases: Vec<(&str, Vec<&str>, Vec<Spec>)> = vec![
            (
                "document Home",
                doc.groups.iter().map(|g| g.title).collect(),
                doc.groups.iter().map(|g| model_spec(g, &tw)).collect(),
            ),
            (
                "Project Task",
                task.groups.iter().map(|g| g.title).collect(),
                task.groups.iter().map(|g| model_spec(g, &tw)).collect(),
            ),
            (
                "sheet Home",
                sheet.groups.iter().map(|g| g.title).collect(),
                sheet
                    .groups
                    .iter()
                    .map(|g| sheet_spec(g, sheet.titles, &off, &tw))
                    .collect(),
            ),
        ];
        for (name, titles, specs) in cases {
            let order = collapse_order(name, &specs);
            let last: Vec<&str> = order.iter().rev().map(|&i| titles[i]).collect();
            assert_eq!(last[0], "Clipboard", "{name}: {last:?}");
            if name != "Project Task" {
                assert_eq!(last[1], "Font", "{name}: {last:?}");
            }
        }
    }

    /// #1020: labels go first, one group at a time. No group collapses while
    /// another could still drop its labels, and on Project's Task tab (whose
    /// button columns have labels) some width has one group icon-only while
    /// another that could be is still full.
    #[test]
    fn icon_only_comes_before_collapse_and_is_per_group() {
        for (name, specs) in every_tab() {
            for width in widths() {
                let fit = fit(&specs, width);
                if fit.states.contains(&State::Collapsed) {
                    for (i, s) in specs.iter().enumerate() {
                        assert!(
                            s.icon == s.full || fit.states[i] != State::Full,
                            "{name} at {width}: group {i} kept its labels"
                        );
                    }
                }
            }
        }
        let task = model_tab(Kind::Project, "Task");
        let specs: Vec<Spec> = task.groups.iter().map(|g| model_spec(g, &tw)).collect();
        let one_at_a_time = widths().any(|w| {
            let fit = fit(&specs, w);
            fit.states.contains(&State::IconOnly)
                && (0..specs.len())
                    .any(|i| fit.states[i] == State::Full && specs[i].icon < specs[i].full)
        });
        assert!(one_at_a_time);
    }

    fn spec(priority: u8, full: f32) -> Spec {
        Spec {
            priority,
            full,
            icon: full,
            collapsed: 50.0,
        }
    }

    /// Too narrow even for the collapsed buttons: the lowest-priority ones go
    /// into the chevron, and one group always stays drawn.
    #[test]
    fn the_chevron_takes_what_does_not_fit_as_buttons() {
        let specs = [spec(30, 200.), spec(10, 200.), spec(20, 200.)];
        let wide = fit(&specs, 700.);
        assert_eq!(wide.states, vec![State::Full; 3]);
        assert!(wide.overflow.is_empty());
        // 3 x 50 + the gutter fit.
        let narrow = fit(&specs, 150.0 + GUTTER);
        assert_eq!(narrow.states, vec![State::Collapsed; 3]);
        assert!(narrow.overflow.is_empty());
        // 2 x 50 + the chevron (28) + the gutter fit.
        let tight = fit(&specs, 130.0 + GUTTER);
        assert_eq!(tight.overflow, vec![1], "the lowest priority");
        assert!(tight.laid_out(&specs) <= 130.0);
        let tiny = fit(&specs, 10.0);
        assert_eq!(tiny.overflow, vec![1, 2], "the highest stays drawn");
    }

    /// Project's Report tab has no groups yet (#370): nothing to fit.
    #[test]
    fn an_empty_tab_fits_with_nothing_shown() {
        for width in [0., 400., 1600.] {
            let fit = fit(&[], width);
            assert!(fit.states.is_empty() && fit.overflow.is_empty());
        }
    }

    /// The Paragraph group's two rows are all icon buttons; the wider row
    /// (7) sets the width, each at `icon_btn`'s rendered width and 1px apart,
    /// between the group's padding and divider. The `ribbon-fit.uit` cases
    /// check every group's estimate against the width drawn
    /// (`estimate_matches_drawn_width`).
    #[test]
    fn a_row_of_icon_buttons_is_estimated_at_their_rendered_width() {
        let home = model_tab(Kind::Docx, "Home");
        let para = home.groups.iter().find(|g| g.title == "Paragraph").unwrap();
        // `icon_btn` without its label: border 1 + `px_2` 8 + icon 16 + `px_2`
        // 8 + border 1, then the row's 1px gaps; `px_2` and the divider.
        // Written out, so changing the estimate means changing this too.
        let row = 7. * (1. + 8. + 16. + 8. + 1.) + 6.;
        assert_eq!(model_group_w(para, true, &tw), row + 16. + 1.);
    }

    /// The collapsed button is drawn at its estimate: the title (the last
    /// line with the drop-down mark) between its padding, at least 48 px.
    #[test]
    fn a_collapsed_button_is_as_wide_as_its_title() {
        let w = |t: &str| collapsed_w(t, &tw);
        assert_eq!(w("Font"), 48. + 1.);
        let long = "Write & Insert Fields";
        let lines = crate::label_lines(long);
        assert_eq!(lines.len(), 2);
        let text = tw(&format!("{} \u{25BE}", lines[1]), COLLAPSED_TITLE_PX)
            .max(tw(&lines[0], COLLAPSED_TITLE_PX));
        assert_eq!(w(long), (text + COLLAPSED_PAD).ceil().max(48.) + 1.);
    }

    #[test]
    fn rows_wrap_greedily_and_a_wide_item_gets_its_own() {
        assert_eq!(
            wrap_rows(&[], 4., 100.),
            Vec::<std::ops::Range<usize>>::new()
        );
        assert_eq!(wrap_rows(&[40., 40., 40.], 4., 200.), vec![0..3]);
        // 40 + 4 + 40 = 84 fits 90; a third does not.
        assert_eq!(wrap_rows(&[40., 40., 40.], 4., 90.), vec![0..2, 2..3]);
        assert_eq!(
            wrap_rows(&[30., 150., 30.], 4., 100.),
            vec![0..1, 1..2, 2..3]
        );
    }

    /// `gallery_row`'s width is the Styles well's as `style_gallery` draws it.
    #[test]
    fn a_style_gallery_row_is_as_wide_as_its_well() {
        let home = model_tab(Kind::Docx, "Home");
        let Some(Control::Gallery(gal)) = home
            .groups
            .iter()
            .flat_map(|g| &g.items)
            .find(|c| matches!(c, Control::Gallery(_)))
        else {
            panic!("Home has the Styles gallery");
        };
        let n = gal.items.len();
        let geom = gallery_geom(gal);
        assert_eq!(
            gallery_row(geom, n, None),
            (n, crate::style_gallery::well_width(n))
        );
        let (k, w) = gallery_row(geom, n, Some(200.));
        assert!(k < n && w <= 200.);
        assert_eq!(w, crate::style_gallery::well_width(k));
    }

    /// #1020 r1: a flyout never draws wider than the narrowest window. At the
    /// 460 px minimum (less the window's insets, the flyout's margins and
    /// padding, and the group's), every group of every tab wraps to rows no
    /// wider than that, every control at its full size: Table Design's Table
    /// Styles gallery (ten 58 px tiles) wraps its tiles.
    #[test]
    fn every_flyout_fits_the_narrowest_window() {
        let max_w = 460. - 2. * 13. - 8. - 10. - 17.;
        let mut model: Vec<rs::Tab<Act>> = Vec::new();
        model.extend(crate::ribbon_for(Kind::Docx).tabs);
        model.extend(crate::ribbon_for(Kind::Project).tabs);
        model.push(crate::table_tab::table_design_tab());
        model.push(crate::table_tab::table_layout_tab());
        model.push(crate::hf_tab::hf_tab());
        model.push(crate::gantt_format_tab());
        let mut wrapped_a_gallery = false;
        for t in &model {
            for g in &t.groups {
                let widths: Vec<f32> = g
                    .items
                    .iter()
                    .map(|c| model_control_w(c, false, Some(max_w), &tw))
                    .collect();
                for (c, &w) in g.items.iter().zip(&widths) {
                    assert!(
                        w <= max_w,
                        "{} / {}: a control {w} px wide",
                        t.name,
                        g.title
                    );
                    if let Control::Gallery(gal) = c {
                        wrapped_a_gallery |= w < model_control_w(c, false, None, &tw);
                        assert!(
                            gallery_row(gallery_geom(gal), gal.items.len(), Some(max_w)).0 >= 1
                        );
                    }
                }
                for row in wrap_rows(&widths, 4., max_w) {
                    let w: f32 =
                        widths[row.clone()].iter().sum::<f32>() + 4. * (row.len() - 1) as f32;
                    assert!(w <= max_w, "{} / {}: a row {w} px wide", t.name, g.title);
                }
            }
        }
        assert!(wrapped_a_gallery, "Table Styles is wider than the window");
        let sheet_max = 460. - 2. * 13. - 8. - 10. - 13.;
        for t in sheet_ribbon::SHEET_RIBBON {
            for g in t.groups {
                let Body::Strip { gap, items } = &g.body else {
                    continue;
                };
                let widths: Vec<f32> = items
                    .iter()
                    .map(|i| sheet_item_w(i, false, &off, &tw))
                    .collect();
                for row in wrap_rows(&widths, gap_px(*gap), sheet_max) {
                    let w: f32 = widths[row.clone()].iter().sum::<f32>()
                        + gap_px(*gap) * (row.len() - 1) as f32;
                    assert!(w <= sheet_max, "{}: a row {w} px wide", g.title);
                }
            }
        }
    }
}
