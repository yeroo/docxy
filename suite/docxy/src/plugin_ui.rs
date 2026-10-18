//! UI plugins on the Project surface (#1532): what the loaded `yppx_*`
//! libraries' manifests contribute (`projcore::uiplugin`), turned into the
//! ribbon, menu and dialog models the window draws, and the calls back into
//! them: `yppx_ui_state` when the plan, the selection or the view changed
//! (cached by `Editor::revision`, never per frame), `yppx_ui_command` for a
//! contributed control, menu item or dialog's OK (its ops as one undo step,
//! its `view` into the plan's formatting and the view's switches), and
//! `yppx_ui_overlay` for the shapes the chart paints after its bars (cached
//! by revision, scale and flags).
//!
//! Nothing here knows any plugin's features or names: a control is drawn
//! because a manifest declares one, a command runs because a manifest named
//! its id, a shape is painted because the overlay returned it. A contributed
//! control, menu item or Task Information page whose id (or page name) is a
//! built-in's replaces it in place; otherwise it is appended. The harness
//! reports contributed items with `"plugin": true`.
//!
//! The models hold `&'static str` names; a plugin's are built at run time,
//! so each distinct one is interned once ([`intern`]).

use crate::dialog::{self, Dialog, DialogOwner, catalog};
use crate::menu::{Entry, MenuItem};
use crate::page_setup::ok_cancel;
use crate::project_formats::global as formats;
use crate::{Act, Control, DocTab, ProjectAct, ProjectView, Surface, rs};
use ctlcore::json::Json as CtlJson;
use projcore::formats::plugin::UiPlugin;
use projcore::formatting::TextStyle;
use projcore::json::Json;
use projcore::uiplugin::{
    self as ui, Command, ControlKind as PluginControlKind, OverlayCtx, OverlayLink, OverlayRow,
    Palette, Selection, Shape, State, StateCtx,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

/// The loaded UI plugins, in load order.
fn plugins() -> &'static [Arc<UiPlugin>] {
    &formats().ui
}

/// Whether any UI plugin is loaded: every hook below is a no-op without one.
pub(crate) fn any() -> bool {
    !plugins().is_empty()
}

/// The loaded plugins' manifests, in load order.
fn manifests() -> Vec<&'static ui::Manifest> {
    plugins().iter().map(|p| p.manifest()).collect()
}

/// `s` as a `&'static str`, kept once per distinct string for the life of
/// the process (the names a build's plugins declare are a small fixed set).
pub(crate) fn intern(s: &str) -> &'static str {
    static STRINGS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut strings = STRINGS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(found) = strings.iter().find(|k| **k == s) {
        return found;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    strings.push(leaked);
    leaked
}

/// A plugin command `id` as a ribbon or menu act.
pub(crate) fn act_of(id: &str) -> Act {
    Act::Project(ProjectAct::Plugin(intern(id)))
}

/// Whether `act` is a plugin's command.
pub(crate) fn is_plugin(act: Act) -> bool {
    matches!(act, Act::Project(ProjectAct::Plugin(_)))
}

// ---- the ribbon --------------------------------------------------------------

fn cmd_of(id: &str, label: &str, icon: &str, tip: &str, keytip: &str) -> rs::Cmd<Act> {
    let tip = if tip.is_empty() { label } else { tip };
    rs::cmd(intern(id), intern(icon), intern(label), act_of(id))
        .tip(intern(tip), "", "")
        .key(intern(keytip))
}

fn item_cmd(item: &ui::Item) -> rs::Cmd<Act> {
    cmd_of(&item.id, &item.label, &item.icon, &item.tip, "")
}

/// A swatch's tile: its colour rides in the preview as `#RRGGBB`, which the
/// swatch gallery renderer reads ([`swatch_color`]).
fn swatch_item(s: &ui::Swatch) -> rs::GalleryItem<Act> {
    let preview = match s.color {
        Some(c) => format!("#{c:06X}"),
        None => s.preview.clone(),
    };
    rs::GalleryItem {
        label: intern(&s.label),
        preview: intern(&preview),
        act: act_of(&s.id),
    }
}

/// The colour a swatch tile's preview carries, when it is one.
pub(crate) fn swatch_color(preview: &str) -> Option<u32> {
    let hex = preview.strip_prefix('#')?;
    (hex.len() == 6)
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
}

/// A contributed control as the ribbon model holds it. A plain small button
/// comes back as a one-button column; [`controls_of`] stacks neighbours.
fn control_of(c: &ui::Control) -> Control<Act> {
    let cmd = cmd_of(&c.id, &c.label, &c.icon, &c.tip, &c.keytip);
    match c.kind {
        PluginControlKind::Button if c.large => Control::Large(cmd),
        PluginControlKind::Button => Control::Column(vec![cmd]),
        PluginControlKind::Toggle => Control::Toggle(cmd),
        PluginControlKind::Split => Control::Split {
            primary: cmd,
            menu: c.items.iter().map(item_cmd).collect(),
        },
        // A combo is drawn as a drop-down of its choices.
        PluginControlKind::Dropdown | PluginControlKind::Combo => Control::Dropdown {
            cmd,
            items: c.items.iter().map(item_cmd).collect(),
        },
        PluginControlKind::Gallery => Control::Gallery(rs::Gallery {
            id: intern(&c.id),
            tip: rs::ScreenTip {
                title: intern(if c.tip.is_empty() { &c.label } else { &c.tip }),
                body: "",
                shortcut: "",
            },
            items: c.swatches.iter().map(swatch_item).collect(),
        }),
    }
}

/// A group's contributed controls, small buttons that follow one another
/// stacked in columns of up to `rs::MAX_COLUMN_ROWS`, as the built-ins are.
fn controls_of(controls: &[ui::Control]) -> Vec<Control<Act>> {
    let mut out: Vec<Control<Act>> = Vec::new();
    for c in controls {
        if c.kind == PluginControlKind::Button && !c.large {
            let cmd = cmd_of(&c.id, &c.label, &c.icon, &c.tip, &c.keytip);
            match out.last_mut() {
                Some(Control::Column(last)) if last.len() < rs::MAX_COLUMN_ROWS => last.push(cmd),
                _ => out.push(Control::Column(vec![cmd])),
            }
            continue;
        }
        out.push(control_of(c));
    }
    out
}

/// The command in `control` with id `id`, to replace it in place.
fn cmd_mut<'a>(control: &'a mut Control<Act>, id: &str) -> Option<&'a mut rs::Cmd<Act>> {
    match control {
        Control::Large(c) | Control::Toggle(c) => (c.id == id).then_some(c),
        Control::Column(cs) => cs.iter_mut().find(|c| c.id == id),
        Control::Split { primary, menu } => std::iter::once(primary)
            .chain(menu.iter_mut())
            .find(|c| c.id == id),
        Control::Dropdown { cmd, items } => std::iter::once(cmd)
            .chain(items.iter_mut())
            .find(|c| c.id == id),
        Control::Rows(rows) => rows.iter_mut().flatten().find_map(|cell| match cell {
            rs::Cell::Btn(c) | rs::Cell::Combo { cmd: c, .. } => (c.id == id).then_some(c),
        }),
        Control::Gallery(_) | Control::Separator => None,
    }
}

/// Add every loaded plugin's contributions for `tab` (by its name): a
/// control whose id is a built-in command's replaces that command in place
/// (its label, icon, tip, KeyTip and act; the control keeps its shape); the
/// rest go into the group of that title, or into a new group after the
/// tab's own.
pub(crate) fn extend_tab(tab: &mut rs::Tab<Act>) {
    extend_tab_with(tab, &manifests());
}

/// [`extend_tab`] from `manifests`.
pub(crate) fn extend_tab_with(tab: &mut rs::Tab<Act>, manifests: &[&ui::Manifest]) {
    for manifest in manifests {
        for contributed in manifest
            .ribbon
            .iter()
            .filter(|t| t.tab.eq_ignore_ascii_case(tab.name))
        {
            for group in &contributed.groups {
                let mut added: Vec<&ui::Control> = Vec::new();
                for c in &group.controls {
                    let replaced = tab
                        .groups
                        .iter_mut()
                        .flat_map(|g| g.items.iter_mut())
                        .find_map(|control| cmd_mut(control, &c.id));
                    match replaced {
                        Some(slot) => *slot = cmd_of(&c.id, &c.label, &c.icon, &c.tip, &c.keytip),
                        None => added.push(c),
                    }
                }
                if added.is_empty() {
                    continue;
                }
                let controls: Vec<ui::Control> = added.into_iter().cloned().collect();
                let controls = controls_of(&controls);
                match tab
                    .groups
                    .iter_mut()
                    .find(|g| g.title.eq_ignore_ascii_case(&group.title))
                {
                    Some(g) => g.items.extend(controls),
                    None => {
                        // Before or after the built-in group the manifest
                        // names (#1574), else after the tab's own.
                        let position = |title: &str| {
                            tab.groups
                                .iter()
                                .position(|g| g.title.eq_ignore_ascii_case(title))
                        };
                        let at = if let Some(i) = position(&group.before) {
                            i
                        } else if let Some(i) = position(&group.after) {
                            i + 1
                        } else {
                            tab.groups.len()
                        };
                        tab.groups.insert(
                            at,
                            rs::Group {
                                title: intern(&group.title),
                                launcher: None,
                                priority: 50,
                                items: controls,
                            },
                        );
                    }
                }
            }
        }
    }
}

// ---- state -------------------------------------------------------------------

/// What the state and the overlay were last computed for.
#[derive(Debug, Clone, PartialEq)]
struct StateKey {
    revision: u64,
    uid: Option<i32>,
    col: usize,
    flags: (bool, bool, bool),
}

#[derive(Debug, Clone, PartialEq)]
struct OverlayKey {
    revision: u64,
    origin_day: i64,
    days: i64,
    flags: (bool, bool, bool),
    /// The shown rows' tasks: a collapsed or expanded summary changes the
    /// rows (and their links) without a change of the plan.
    shown: Vec<usize>,
    /// The selection the request carries (#1552): a plugin may draw the
    /// selected task's path, so moving the cursor asks again (#1565).
    selected: Vec<i32>,
}

/// The plugins' answers a view keeps between changes.
#[derive(Default)]
pub(crate) struct PluginCache {
    state_key: Option<StateKey>,
    /// One `State` per loaded UI plugin, in load order.
    states: Vec<State>,
    overlay_key: Option<OverlayKey>,
    shapes: Rc<Vec<Shape>>,
}

/// The view's switches as the plugins see them (`view` in every request),
/// with the plan's formatting choices an overlay follows (#1548).
/// The view JSON the state and overlay requests and a command carry: the
/// view's switches, the plan's formatting (palette, link style, gridlines,
/// the plugins' switches, every text style category, and the selected
/// cells' styles, #1572) in the shape `Formatting::apply_view` reads.
fn view_json(v: &ProjectView) -> Json {
    let formatting = &v.ed.project().formatting;
    let text = |s: &Option<String>| s.clone().map_or(Json::Null, Json::Str);
    let selection = selection_of(v);
    let text_styles = formatting
        .text_styles
        .iter()
        .map(|c| (c.category.clone(), c.style.to_json()))
        .collect();
    let cells = formatting
        .cell_styles
        .iter()
        .filter(|c| selection.uids.contains(&c.uid) && selection.cols.contains(&c.column))
        .map(|c| {
            let mut pairs = vec![
                ("uid".to_string(), Json::Num(f64::from(c.uid))),
                ("column".to_string(), Json::str(c.column.clone())),
            ];
            if let Json::Obj(style) = c.style.to_json() {
                pairs.extend(style);
            }
            Json::Obj(pairs)
        })
        .collect();
    Json::obj(vec![
        ("critical", Json::Bool(v.show_critical)),
        ("baseline", Json::Bool(v.show_baseline)),
        ("timeline", Json::Bool(v.timeline)),
        ("palette", text(&formatting.palette)),
        ("link_style", text(&formatting.link_style)),
        ("gridlines", text(&formatting.gridlines)),
        ("switches", formatting.switches_json()),
        ("text_styles", Json::Obj(text_styles)),
        ("cells", Json::Arr(cells)),
    ])
}

fn flags_of(v: &ProjectView) -> (bool, bool, bool) {
    (v.show_critical, v.show_baseline, v.timeline)
}

/// The table's selection: the cursor's task first, then the range's.
fn selection_of(v: &ProjectView) -> Selection {
    let column = |c: usize| {
        crate::COLUMNS
            .get(c)
            .copied()
            .unwrap_or_default()
            .to_string()
    };
    let cursor = v.selected_uid();
    match v.selection() {
        Some(sel) => {
            let mut uids: Vec<i32> = cursor.into_iter().collect();
            uids.extend(sel.uids.iter().copied().filter(|u| Some(*u) != cursor));
            Selection {
                uids,
                cols: sel.cols.clone().map(column).collect(),
            }
        }
        None => Selection {
            uids: cursor.into_iter().collect(),
            cols: vec![column(v.col)],
        },
    }
}

/// `yppx_ui_state` for the view as it is, asked again only when the plan's
/// revision, the cursor or the view's switches changed. A plugin whose call
/// fails answers with an empty state.
fn with_states<T>(v: &ProjectView, f: impl FnOnce(&[State]) -> T) -> T {
    let plugins = plugins();
    if plugins.is_empty() {
        return f(&[]);
    }
    let key = StateKey {
        revision: v.ed.revision(),
        uid: v.selected_uid(),
        col: v.col,
        flags: flags_of(v),
    };
    let mut cache = v.plugin_cache.borrow_mut();
    if cache.state_key.as_ref() != Some(&key) {
        let ctx = StateCtx {
            plan: projcore::mspdi::write_mspdi(v.ed.project()),
            selection: selection_of(v),
            view: view_json(v),
            dialog: None,
            dialog_values: None,
        };
        cache.states = plugins
            .iter()
            .map(|p| p.state(&ctx).unwrap_or_default())
            .collect();
        cache.state_key = Some(key);
    }
    f(&cache.states)
}

/// Whether the plugin control `id` is enabled: the first plugin that says
/// decides; silence is enabled.
pub(crate) fn enabled(v: &ProjectView, id: &str) -> bool {
    with_states(v, |states| {
        states
            .iter()
            .find_map(|s| s.control(id).enabled)
            .unwrap_or(true)
    })
}

/// Whether the plugin toggle `id` is drawn checked.
pub(crate) fn checked(v: &ProjectView, id: &str) -> bool {
    with_states(v, |states| {
        states
            .iter()
            .find_map(|s| s.control(id).checked)
            .unwrap_or(false)
    })
}

/// Whether the swatch `id` is its gallery's checked one.
pub(crate) fn swatch_checked(v: &ProjectView, id: &str) -> bool {
    let Some(gallery) = manifests().iter().find_map(|m| {
        m.ribbon.iter().find_map(|t| {
            t.groups.iter().find_map(|g| {
                g.controls
                    .iter()
                    .find(|c| c.swatches.iter().any(|s| s.id == id))
                    .map(|c| c.id.clone())
            })
        })
    }) else {
        return false;
    };
    with_states(v, |states| {
        states
            .iter()
            .find_map(|s| s.checked_swatch(&gallery))
            .is_some_and(|checked| checked == id)
    })
}

/// A dialog's prefill: `yppx_ui_state` with `dialog: {id, uid}`, every
/// plugin's `values` merged (the first plugin naming a control wins).
/// What the plugins' state gives a dialog about to open: values, items and
/// enabled flags by control name, the first plugin to name a control
/// winning (#1556).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Prefill {
    pub values: Vec<(String, Json)>,
    pub items: Vec<(String, Vec<String>)>,
    pub enabled: Vec<(String, bool)>,
}

impl Prefill {
    fn value(&self, name: &str) -> Option<&Json> {
        self.values.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    fn items(&self, name: &str) -> Option<&[String]> {
        self.items
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_slice())
    }

    fn enabled(&self, name: &str) -> Option<bool> {
        self.enabled
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, on)| *on)
    }
}

fn prefill(v: &ProjectView, dialog: &str, uid: Option<i32>) -> Prefill {
    prefill_with(v, dialog, uid, None)
}

/// [`prefill`] for an open dialog asked again (#1602): `values` are the
/// dialog's current values, sent as `dialog.values`.
fn prefill_with(v: &ProjectView, dialog: &str, uid: Option<i32>, values: Option<Json>) -> Prefill {
    let ctx = StateCtx {
        plan: projcore::mspdi::write_mspdi(v.ed.project()),
        selection: selection_of(v),
        view: view_json(v),
        dialog: Some((dialog.to_string(), uid)),
        dialog_values: values,
    };
    let mut out = Prefill::default();
    for p in plugins() {
        if let Ok(state) = p.state(&ctx) {
            for (name, value) in state.values {
                if out.value(&name).is_none() {
                    out.values.push((name, value));
                }
            }
            for (name, items) in state.items {
                if out.items(&name).is_none() {
                    out.items.push((name, items));
                }
            }
            for (name, on) in state.enabled {
                if out.enabled(&name).is_none() {
                    out.enabled.push((name, on));
                }
            }
        }
    }
    out
}

// ---- commands ----------------------------------------------------------------

/// What a command left: the status, the dialog to open, and whether the
/// dialog whose button sent it closes (#1602).
pub(crate) struct Outcome {
    pub status: Option<String>,
    pub dialog: Option<Dialog>,
    pub close: bool,
}

/// Run the plugin command `id` on the view's plan: the first plugin that
/// owns the id answers; its ops and its `view` (the plan's formatting) are
/// one undo step, its view switches set the view, and its dialog is built
/// with the plugin's prefill. `values` are a dialog's on its OK, `button`
/// the id of one of the dialog's own buttons when one sent them, `edit` the
/// grid cell just committed when a `commit_command` grid did (#1602).
pub(crate) fn run_command(
    v: &mut ProjectView,
    id: &str,
    values: Option<Json>,
    button: Option<&str>,
    edit: Option<Json>,
) -> Result<Outcome, String> {
    let command = Command {
        id: id.to_string(),
        plan: projcore::mspdi::write_mspdi(v.ed.project()),
        selection: selection_of(v),
        view: view_json(v),
        values,
        button: button.map(str::to_string),
        edit,
    };
    let mut answer = None;
    for plugin in plugins() {
        if let Some(result) = plugin.command(&command)? {
            answer = Some(result);
            break;
        }
    }
    let Some(result) = answer else {
        return Err(format!("No plugin runs the command '{id}'"));
    };
    // The plugin may have changed its own state (a toggle): ask again.
    {
        let mut cache = v.plugin_cache.borrow_mut();
        cache.state_key = None;
        cache.overlay_key = None;
    }
    let view = result.view.clone();
    v.ed.batch(|ed| {
        projctl::apply_ops_in(ed, &result.ops)?;
        if let Some(view) = &view {
            let mut failed = None;
            ed.edit_formatting(|f| {
                if let Err(e) = f.apply_view(view) {
                    failed = Some(e);
                }
            });
            if let Some(e) = failed {
                return Err(e);
            }
        }
        Ok(())
    })?;
    if let Some(view) = &view {
        for (key, flag) in [
            ("critical", &mut v.show_critical),
            ("baseline", &mut v.show_baseline),
            ("timeline", &mut v.timeline),
        ] {
            if let Some(on) = view.get(key).and_then(Json::as_bool) {
                *flag = on;
            }
        }
    }
    let dialog = match &result.dialog {
        Some(spec_id) => Some(open_dialog(v, spec_id, id)?),
        None => None,
    };
    Ok(Outcome {
        status: result.status,
        dialog,
        close: result.close,
    })
}

// ---- menus -------------------------------------------------------------------

/// Add every plugin's items for `menu` (`row`, `bar`, `column`, `chart`) to
/// `items`: one whose id is an existing item's replaces it in place; one
/// with an `after` hint goes after the item of that label; the rest go at
/// the end, after a separator. Enabled as `yppx_ui_state` says.
pub(crate) fn extend_menu(v: &ProjectView, menu: &str, items: &mut Vec<MenuItem>) {
    if !any() {
        return;
    }
    extend_menu_with(menu, items, &manifests(), |id| {
        (enabled(v, id), checked(v, id))
    });
}

/// [`extend_menu`] from `manifests`, `state` giving an item's (enabled,
/// checked).
pub(crate) fn extend_menu_with(
    menu: &str,
    items: &mut Vec<MenuItem>,
    manifests: &[&ui::Manifest],
    state: impl Fn(&str) -> (bool, bool),
) {
    let label_matches = |entry: &Entry, want: &str| {
        let strip = |s: &str| s.trim_end_matches('.').trim().to_ascii_lowercase();
        strip(&entry.label) == strip(want)
    };
    let mut appended = false;
    for manifest in manifests {
        for contributed in manifest
            .menus
            .iter()
            .filter(|m| m.menu.eq_ignore_ascii_case(menu))
        {
            for item in &contributed.items {
                let (enabled, checked) = state(&item.id);
                let entry = Entry::new(
                    &item.id,
                    &item.label,
                    intern(&item.icon),
                    act_of(&item.id),
                    enabled,
                )
                .checked(checked)
                .plugin();
                let existing = items
                    .iter()
                    .position(|i| matches!(i, MenuItem::Item(e) if e.id == item.id));
                if let Some(at) = existing {
                    items[at] = MenuItem::Item(entry);
                    continue;
                }
                let after = (!item.after.is_empty())
                    .then(|| {
                        items.iter().position(
                            |i| matches!(i, MenuItem::Item(e) if label_matches(e, &item.after)),
                        )
                    })
                    .flatten();
                match after {
                    Some(at) => items.insert(at + 1, MenuItem::Item(entry)),
                    None => {
                        if !appended {
                            items.push(MenuItem::Separator);
                            appended = true;
                        }
                        items.push(MenuItem::Item(entry));
                    }
                }
            }
        }
    }
}

// ---- dialogs -----------------------------------------------------------------

fn kind_of(kind: &str) -> dialog::ControlKind {
    use dialog::ControlKind as K;
    match kind {
        "number" => K::Number,
        "date" => K::Date,
        "duration" => K::Duration,
        "multiline" => K::Multiline,
        "checkbox" => K::Checkbox,
        "check" => K::Check,
        "radio" => K::Radio,
        "dropdown" => K::Dropdown,
        "list" => K::List,
        "grid" => K::Grid,
        "label" => K::Label,
        // `color` and `font` are typed as text until the host has pickers.
        _ => K::Text,
    }
}

/// A JSON value as a control of `kind` holds it.
fn value_of(kind: dialog::ControlKind, items: &[String], value: &Json) -> dialog::Value {
    use dialog::{ControlKind as K, Value};
    match kind {
        K::Checkbox | K::Check => Value::Bool(value.as_bool().unwrap_or(false)),
        K::Radio | K::Dropdown | K::List => Value::Choice(match value {
            Json::Num(n) => Some(*n as usize).filter(|i| *i < items.len()),
            Json::Str(s) => items.iter().position(|i| i == s),
            _ => None,
        }),
        K::Grid => Value::Rows(
            value
                .as_array()
                .unwrap_or(&[])
                .iter()
                .map(|row| {
                    row.as_array()
                        .unwrap_or(&[])
                        .iter()
                        .map(cell_text)
                        .collect()
                })
                .collect(),
        ),
        K::CheckList => Value::Checks(
            value
                .as_array()
                .unwrap_or(&[])
                .iter()
                .map(|b| b.as_bool().unwrap_or(false))
                .collect(),
        ),
        _ => Value::Text(cell_text(value)),
    }
}

/// A JSON scalar as a cell's text.
fn cell_text(value: &Json) -> String {
    match value {
        Json::Str(s) => s.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}

/// A control's staged value as the plugin reads it: text (a number field's
/// as a number when it parses), a bool, a choice's label, a grid's rows.
fn value_back(c: &dialog::Control) -> Json {
    use dialog::{ControlKind as K, Value};
    match &c.value {
        Value::Text(s) if c.kind == K::Number => s
            .trim()
            .parse::<f64>()
            .map_or_else(|_| Json::Str(s.clone()), Json::Num),
        Value::Text(s) => Json::Str(s.clone()),
        Value::Bool(b) => Json::Bool(*b),
        Value::Choice(i) => i
            .and_then(|i| c.items.get(i))
            .map_or(Json::Null, |s| Json::Str(s.clone())),
        Value::Rows(rows) => Json::Arr(rows.iter().map(Json::strs).collect()),
        Value::Checks(checks) => Json::Arr(checks.iter().map(|b| Json::Bool(*b)).collect()),
    }
}

/// The plugin controls of `d` (every page), by name, for a command's
/// `values`; a grid's selected row rides along as `name.selected_row`
/// (#1602).
pub(crate) fn values_of(d: &Dialog) -> Json {
    let mut out = Vec::new();
    for c in d.controls.iter().filter(|c| c.plugin) {
        out.push((c.name.to_string(), value_back(c)));
        if c.kind == dialog::ControlKind::Grid {
            out.push((
                format!("{}.selected_row", c.name),
                c.row.map_or(Json::Null, |r| Json::Num(r as f64)),
            ));
        }
    }
    Json::Obj(out)
}

/// A spec's control as the dialog model holds it, on `page`, with the
/// prefill's value, items and enabled flag when the plugin gave them.
fn control_of_spec(
    c: &ui::DialogControl,
    page: Option<usize>,
    prefill: &Prefill,
) -> dialog::Control {
    let kind = kind_of(&c.kind);
    let value = prefill.value(&c.name).unwrap_or(&c.value);
    let items = prefill.items(&c.name).unwrap_or(&c.items);
    let mut out = dialog::Control::new(
        intern(&c.name),
        &c.label,
        kind,
        value_of(kind, items, value),
    );
    out.items = items.to_vec();
    if let Some(on) = prefill.enabled(&c.name) {
        out.enabled = on;
    }
    out.columns = c.columns.iter().map(|col| col.name.clone()).collect();
    out.column_kinds = c.columns.iter().map(|col| kind_of(&col.kind)).collect();
    out.commit_command = c.commit_command;
    out.page = page;
    out.plugin = true;
    apply_selected_row(&mut out, prefill);
    out
}

/// A grid's selected row as the plugin's state gives it under
/// `name.selected_row` (#1602): a number selects that row (clamped to the
/// rows), null none; absent leaves the selection alone.
fn apply_selected_row(c: &mut dialog::Control, prefill: &Prefill) {
    if c.kind != dialog::ControlKind::Grid {
        return;
    }
    let Some(v) = prefill.value(&format!("{}.selected_row", c.name)) else {
        return;
    };
    let rows = match &c.value {
        dialog::Value::Rows(rows) => rows.len(),
        _ => 0,
    };
    c.row = match v {
        Json::Num(n) if *n >= 0.0 && rows > 0 => Some((*n as usize).min(rows - 1)),
        _ => None,
    };
}

/// The dialog spec `spec_id` of any loaded plugin.
fn spec(spec_id: &str) -> Option<&'static ui::DialogSpec> {
    manifests().iter().find_map(|m| m.dialog(spec_id))
}

/// Build the dialog spec `spec_id` for the view, prefilled by the plugins;
/// its OK sends `command` again with the values.
fn open_dialog(v: &ProjectView, spec_id: &str, command: &str) -> Result<Dialog, String> {
    let spec = spec(spec_id).ok_or_else(|| format!("No plugin declares the dialog '{spec_id}'"))?;
    let values = prefill(v, spec_id, v.selected_uid());
    let values = &values;
    let mut d = Dialog::message(
        catalog::PLUGIN,
        if spec.title.is_empty() {
            spec_id
        } else {
            &spec.title
        },
        String::new(),
        &[],
        DialogOwner::Plugin {
            command: intern(command),
            spec: intern(spec_id),
        },
    );
    d.text = None;
    d.modeless = spec.modeless;
    d.seen = Some((v.ed.revision(), v.selected_uid()));
    let tabbed = spec.pages.len() > 1;
    if tabbed {
        d.tabs = spec.pages.iter().map(|p| p.name.clone()).collect();
    }
    d.controls = spec
        .pages
        .iter()
        .enumerate()
        .flat_map(|(i, p)| {
            let page = tabbed.then_some(i);
            p.controls
                .iter()
                .map(move |c| control_of_spec(c, page, values))
        })
        .collect();
    // OK and Cancel, or the spec's own buttons (#1602): `ok` accepts (the
    // command gets the values and the dialog closes), a cancel button
    // closes, any other sends the command with its id and stays open
    // unless the reply closes it. Help is not drawn: the host has no help
    // pages for a plugin. The state greys a button by its id.
    let mut buttons = if spec.buttons.is_empty() {
        ok_cancel()
    } else {
        spec.buttons
            .iter()
            .filter(|b| !b.id.eq_ignore_ascii_case("help"))
            .map(|b| {
                let role = if b.cancel {
                    dialog::ButtonRole::Cancel
                } else if b.id.eq_ignore_ascii_case("ok") {
                    dialog::ButtonRole::Accept
                } else {
                    dialog::ButtonRole::Apply
                };
                dialog::Button {
                    id: b.id.clone(),
                    default: b.default,
                    ..dialog::Button::new(&b.label, role)
                }
            })
            .collect()
    };
    for b in &mut buttons {
        if let Some(on) = values.enabled(&b.id) {
            b.enabled = on;
        }
    }
    d.buttons = buttons;
    d.mark_opened();
    Ok(d)
}

/// Ask the plugin's state again for the top dialog when a plugin control
/// changed since (#1567) or, for a modeless dialog, the plan or the
/// selection moved under it (#1602), and apply the reply: items and enabled
/// flags always (a chosen item that is still there stays chosen), the
/// values only when the plan or selection moved (a grid keeps its selected
/// row and the cell being typed into). Called before a frame is drawn or a
/// reply is written, so a read sees the state the plugin would show.
pub(crate) fn refresh_dialog(tab: &mut DocTab) {
    let DocTab {
        dialogs,
        surface,
        status,
        ..
    } = tab;
    let Surface::Project(v) = surface else {
        return;
    };
    // A committed cell of a `commit_command` grid sends the command first
    // (#1602): its ops apply at once, the dialog stays unless the reply
    // closes it.
    let committed = dialogs.top_dialog_mut().ok().and_then(|d| {
        let (i, row, col, value) = d.committed.take()?;
        let DialogOwner::Plugin { command, .. } = d.owner else {
            return None;
        };
        let edit = Json::obj(vec![
            ("control", Json::str(d.controls[i].name)),
            ("row", Json::Num(row as f64)),
            ("col", Json::Num(col as f64)),
            ("value", Json::str(value)),
        ]);
        Some((command, values_of(d), edit))
    });
    if let Some((command, values, edit)) = committed {
        match run_command(v, command, Some(values), None, Some(edit)) {
            Ok(outcome) => {
                if let Some(s) = outcome.status {
                    *status = s.into();
                }
                if outcome.close {
                    dialogs.pop();
                }
                if let Some(dialog) = outcome.dialog {
                    dialogs.push(dialog);
                }
            }
            Err(e) => *status = e.into(),
        }
    }
    let Ok(d) = dialogs.top_dialog_mut() else {
        return;
    };
    let (spec_id, uid) = match d.owner {
        DialogOwner::Plugin { spec, .. } => (spec, v.selected_uid()),
        DialogOwner::TaskInfo { uid } if d.controls.iter().any(|c| c.plugin) => {
            ("task-information", Some(uid))
        }
        _ => return,
    };
    let now = (v.ed.revision(), v.selected_uid());
    let moved = d.modeless && d.seen != Some(now);
    if !d.stale && !moved {
        return;
    }
    let values = values_of(d);
    let p = prefill_with(v, spec_id, uid, Some(values));
    for c in d.controls.iter_mut().filter(|c| c.plugin) {
        if let Some(on) = p.enabled(c.name) {
            c.enabled = on;
        }
        if let Some(items) = p.items(c.name) {
            let chosen = match &c.value {
                dialog::Value::Choice(Some(i)) => c.items.get(*i).cloned(),
                _ => None,
            };
            c.items = items.to_vec();
            if c.kind.has_items() {
                c.value = dialog::Value::Choice(
                    chosen.and_then(|was| c.items.iter().position(|it| *it == was)),
                );
            }
        }
        if moved && let Some(value) = p.value(c.name) {
            c.value = value_of(c.kind, &c.items, value);
            if c.kind == dialog::ControlKind::Grid {
                let rows = match &c.value {
                    dialog::Value::Rows(rows) => rows.len(),
                    _ => 0,
                };
                c.row = c.row.filter(|r| *r < rows);
            }
        }
        if moved {
            // The plugin may keep the selection on the same row of a grid
            // whose rows just moved (`name.selected_row`).
            apply_selected_row(c, &p);
        }
    }
    for b in &mut d.buttons {
        if let Some(on) = p.enabled(&b.id) {
            b.enabled = on;
        }
    }
    d.stale = false;
    d.seen = Some(now);
}

/// Add the plugins' Task Information pages for task `uid` to `d`, whose
/// tabs are the built-in pages: a page named like one replaces that page's
/// controls, another is a new tab after them; the controls open on the
/// plugins' prefill. Before `mark_opened`.
pub(crate) fn extend_task_info(d: &mut Dialog, v: &ProjectView, uid: i32) {
    let Some(spec) = spec("task-information") else {
        return;
    };
    let values = prefill(v, "task-information", Some(uid));
    extend_task_info_with(d, spec, &values);
}

/// [`extend_task_info`] from `spec`, prefilled with `values`.
pub(crate) fn extend_task_info_with(d: &mut Dialog, spec: &ui::DialogSpec, values: &Prefill) {
    for page in &spec.pages {
        let at = d
            .tabs
            .iter()
            .position(|t| t.eq_ignore_ascii_case(&page.name));
        let index = match at {
            Some(i) => {
                d.controls.retain(|c| c.page != Some(i));
                i
            }
            None => {
                d.tabs.push(page.name.clone());
                d.tabs.len() - 1
            }
        };
        d.controls.extend(
            page.controls
                .iter()
                .map(|c| control_of_spec(c, Some(index), values)),
        );
    }
}

/// A press on a dialog with plugin controls: a plugin's own dialog
/// ([`DialogOwner::Plugin`]), whose OK sends its command again with the
/// values, or Task Information with plugin pages, whose OK applies the
/// built-in fields and then the plugins' `task-information` command in one
/// undo step. `None` for any other dialog or button.
pub(crate) fn dialog_click(tab: &mut DocTab, button: &str) -> Option<Result<(), String>> {
    let top = tab.dialogs.top()?;
    let command = match top.owner {
        DialogOwner::Plugin { command, .. } => Some(command),
        DialogOwner::TaskInfo { .. } if top.controls.iter().any(|c| c.plugin) => None,
        _ => return None,
    };
    let pressed = top.buttons.iter().find(|b| {
        crate::dialog::fold_label(&b.label) == crate::dialog::fold_label(button)
            || b.id.eq_ignore_ascii_case(button.trim())
    });
    let (is_accept, own) = match pressed {
        Some(b) => (
            matches!(
                b.role,
                dialog::ButtonRole::Accept | dialog::ButtonRole::Apply
            ),
            // One of the dialog's own buttons (#1602): the command learns
            // which; OK sends the values alone, as before.
            (b.role == dialog::ButtonRole::Apply && command.is_some()).then(|| b.id.clone()),
        ),
        None => (false, None),
    };
    if !is_accept {
        return None;
    }
    let DocTab {
        dialogs, surface, ..
    } = tab;
    let Surface::Project(v) = surface else {
        return Some(Err("this dialog belongs to a Project".into()));
    };
    let mut outcome: Option<Outcome> = None;
    let done = dialogs.click(button, |d| {
        let values = values_of(d);
        let result = match (command, d.owner) {
            (Some(command), _) => run_command(v, command, Some(values), own.as_deref(), None)?,
            (None, DialogOwner::TaskInfo { uid }) => {
                // The built-in fields and the plugins' ops as one step:
                // the command first (it reads the plan as it is), then both.
                let ops = task_info_ops(v, uid, values)?;
                let (ops, status) = ops;
                v.ed.batch(|ed| {
                    crate::project::apply_task_info_in(ed, uid, d)?;
                    projctl::apply_ops_in(ed, &ops)
                })?;
                Outcome {
                    status,
                    dialog: None,
                    close: false,
                }
            }
            _ => return Err("this dialog is not a plugin's".into()),
        };
        outcome = Some(result);
        Ok(())
    });
    if let Some(outcome) = outcome {
        if let Some(status) = outcome.status {
            tab.status = status.into();
        }
        // A button of the dialog's own stays open unless the reply closes
        // it (#1602); OK closed it already.
        if outcome.close && own.is_some() {
            tab.dialogs.pop();
        }
        if let Some(dialog) = outcome.dialog {
            tab.dialogs.push(dialog);
        }
    }
    crate::complete_project(tab, true);
    Some(done)
}

/// The ops the plugins' `task-information` command returns for `values`
/// (none when no plugin owns it), with its status.
fn task_info_ops(
    v: &ProjectView,
    uid: i32,
    values: Json,
) -> Result<(Vec<ui::Op>, Option<String>), String> {
    let command = Command {
        id: "task-information".to_string(),
        plan: projcore::mspdi::write_mspdi(v.ed.project()),
        selection: Selection {
            uids: vec![uid],
            cols: Vec::new(),
        },
        view: view_json(v),
        values: Some(values),
        button: None,
        edit: None,
    };
    for plugin in plugins() {
        if let Some(result) = plugin.command(&command)? {
            return Ok((result.ops, result.status));
        }
    }
    Ok((Vec::new(), None))
}

// ---- the chart ---------------------------------------------------------------

/// The bar palette the plan's formatting names, from the manifests.
pub(crate) fn palette(v: &ProjectView) -> Option<Palette> {
    let id = v.ed.project().formatting.palette.as_deref()?;
    palette_with(id, &manifests())
}

/// [`palette`] from `manifests`.
pub(crate) fn palette_with(id: &str, manifests: &[&ui::Manifest]) -> Option<Palette> {
    manifests
        .iter()
        .find_map(|m| m.palettes.iter().find(|pal| pal.id == id))
        .cloned()
}

/// The rows of the chart as drawn, for the overlay: every shown row with
/// its bar's rectangle in chart pixels.
fn overlay_ctx(v: &ProjectView) -> OverlayCtx {
    let scale = v.chart_scale();
    let ed = &v.ed;
    let tasks = &ed.project().tasks;
    let shown = ed.display_rows();
    let sched = ed.schedule();
    let reader = projcore::editor::FieldReader::new(ed);
    // An instant's x in chart pixels, within its day.
    let x_at = |d: projcore::DateTime| {
        (d.minutes() - scale.origin_day * 1440) as f32 / 1440.0 * crate::DAY_W
    };
    let stamp = |d: projcore::DateTime| {
        let p = d.parts();
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}",
            p.year, p.month, p.day, p.hour, p.minute
        )
    };
    let rows: Vec<OverlayRow> = shown
        .iter()
        .enumerate()
        .filter_map(|(row, &i)| {
            let task = tasks.get(i)?;
            let bar = crate::gantt_bar(ed, task, scale).map(|b| v.styled_bar(b));
            let row_y = row as f32 * crate::ROW_H;
            let dated = !matches!(
                task.constraint,
                projcore::ConstraintType::AsSoonAsPossible
                    | projcore::ConstraintType::AsLateAsPossible
            );
            let result = sched.get(task.uid);
            Some(OverlayRow {
                uid: task.uid,
                row_y,
                bar: bar.map(|b| {
                    let (x, y, w, h) = crate::bar_rect(&b);
                    (x, row_y + y, w, h)
                }),
                kind: bar.map_or("", |b| crate::bar_kind_name(b.kind)).to_string(),
                summary: task.summary,
                milestone: task.is_milestone(),
                critical: bar.is_some_and(|b| b.kind == crate::BarKind::Critical),
                inactive: bar.is_some_and(|b| b.inactive),
                name: task.name.clone(),
                percent_complete: task.percent_complete.unwrap_or(0).min(100),
                deadline_x: task.deadline.map(x_at),
                constraint: task
                    .constraint_date
                    .filter(|_| dated)
                    .map(|d| (task.constraint.abbrev().to_string(), x_at(d))),
                resource_names: projcore::editor::format_resource_names(ed.project(), task.uid),
                status: reader.read(task, projcore::editor::Field::Status).text,
                late_start_x: result.map(|r| x_at(r.late_start)),
                late_finish_x: result.map(|r| x_at(r.late_finish)),
                slack_x: result
                    .zip(ed.disp_finish(task.uid))
                    .and_then(|(r, finish)| {
                        ed.working_time_after(task.uid, finish, r.free_slack_min)
                    })
                    .map(x_at),
                total_slack_min: result.map_or(0, |r| r.total_slack_min),
                free_slack_min: result.map_or(0, |r| r.free_slack_min),
                baselines: task
                    .baselines
                    .iter()
                    .filter_map(|b| {
                        let x = x_at(b.start?);
                        Some((b.number, x, (x_at(b.finish?) - x).max(0.0)))
                    })
                    .collect(),
            })
        })
        .collect();
    let shown_uids: Vec<i32> = rows.iter().map(|r| r.uid).collect();
    let links = tasks
        .iter()
        .filter(|t| shown_uids.contains(&t.uid))
        .flat_map(|t| {
            let driving = ed.driving_predecessors(t.uid);
            t.predecessors
                .iter()
                .filter(|p| shown_uids.contains(&p.uid))
                .map(move |p| OverlayLink {
                    pred: p.uid,
                    succ: t.uid,
                    driving: driving.contains(&p.uid),
                    kind: match p.link {
                        projcore::LinkType::FinishStart => "FS",
                        projcore::LinkType::StartStart => "SS",
                        projcore::LinkType::FinishFinish => "FF",
                        projcore::LinkType::StartFinish => "SF",
                    }
                    .to_string(),
                    lag: projcore::editor::format_lag(p, ed.project()),
                })
        })
        .collect();
    let status_date = ed.project().status_date().map(stamp);
    let current_date = ed
        .project()
        .option("CurrentDate")
        .and_then(projcore::DateTime::parse_mspdi)
        .map(stamp);
    OverlayCtx {
        rows,
        origin_day: scale.origin_day,
        day_w: crate::DAY_W,
        row_h: crate::ROW_H,
        links,
        status_date,
        current_date,
        selected: selection_of(v).uids,
        project_start_x: Some(x_at(sched.project_start)),
        project_finish_x: Some(x_at(sched.project_finish)),
        flags: view_json(v),
    }
}

/// The shapes every plugin's overlay draws for the chart as it is, asked
/// again only when the plan's revision, the scale or the view's switches
/// changed. Empty without a UI plugin.
pub(crate) fn overlay_shapes(v: &ProjectView) -> Rc<Vec<Shape>> {
    let plugins = plugins();
    if plugins.is_empty() {
        return Rc::new(Vec::new());
    }
    let scale = v.chart_scale();
    let key = OverlayKey {
        revision: v.ed.revision(),
        origin_day: scale.origin_day,
        days: scale.days,
        flags: flags_of(v),
        shown: v.ed.display_rows(),
        selected: selection_of(v).uids,
    };
    let mut cache = v.plugin_cache.borrow_mut();
    if cache.overlay_key.as_ref() != Some(&key) {
        let ctx = overlay_ctx(v);
        let shapes: Vec<Shape> = plugins
            .iter()
            .flat_map(|p| p.overlay(&ctx).unwrap_or_default())
            .collect();
        cache.shapes = Rc::new(shapes);
        cache.overlay_key = Some(key);
    }
    Rc::clone(&cache.shapes)
}

/// `project-overlay`: the shapes as the harness reads them.
pub(crate) fn overlay_json(v: &ProjectView) -> CtlJson {
    let shapes = overlay_shapes(v);
    let num = |f: f32| CtlJson::Num(f64::from(f));
    let pt = |(x, y): (f32, f32)| CtlJson::Arr(vec![num(x), num(y)]);
    let pts = |points: &[(f32, f32)]| CtlJson::Arr(points.iter().map(|p| pt(*p)).collect());
    let color = |c: u32| CtlJson::Str(format!("#{c:06X}"));
    let items = shapes
        .iter()
        .map(|s| match s {
            Shape::Polyline {
                points,
                color: c,
                width,
            } => CtlJson::obj(vec![
                ("kind", CtlJson::Str("polyline".into())),
                ("points", pts(points)),
                ("color", color(*c)),
                ("width", num(*width)),
            ]),
            Shape::Dashed {
                points,
                color: c,
                width,
            } => CtlJson::obj(vec![
                ("kind", CtlJson::Str("dashed".into())),
                ("points", pts(points)),
                ("color", color(*c)),
                ("width", num(*width)),
            ]),
            Shape::Arrow {
                from,
                to,
                color: c,
                width,
            } => CtlJson::obj(vec![
                ("kind", CtlJson::Str("arrow".into())),
                ("from", pt(*from)),
                ("to", pt(*to)),
                ("color", color(*c)),
                ("width", num(*width)),
            ]),
            Shape::Bracket {
                x0,
                x1,
                y,
                tick,
                color: c,
                width,
            } => CtlJson::obj(vec![
                ("kind", CtlJson::Str("bracket".into())),
                ("x0", num(*x0)),
                ("x1", num(*x1)),
                ("y", num(*y)),
                ("tick", num(*tick)),
                ("color", color(*c)),
                ("width", num(*width)),
            ]),
            Shape::Rect {
                x,
                y,
                w,
                h,
                color: c,
                filled,
            } => CtlJson::obj(vec![
                ("kind", CtlJson::Str("rect".into())),
                ("x", num(*x)),
                ("y", num(*y)),
                ("w", num(*w)),
                ("h", num(*h)),
                ("color", color(*c)),
                ("filled", CtlJson::Bool(*filled)),
            ]),
            Shape::Text {
                x,
                y,
                text,
                color: c,
                size,
            } => CtlJson::obj(vec![
                ("kind", CtlJson::Str("text".into())),
                ("x", num(*x)),
                ("y", num(*y)),
                ("text", CtlJson::Str(text.clone())),
                ("color", color(*c)),
                ("size", num(*size)),
            ]),
        })
        .collect();
    CtlJson::obj(vec![
        ("count", CtlJson::Num(shapes.len() as f64)),
        ("shapes", CtlJson::Arr(items)),
    ])
}

// ---- text styles -------------------------------------------------------------

/// The text style categories a task row falls in, the general first.
pub(crate) fn row_categories(
    task: &projcore::Task,
    critical: bool,
    inactive: bool,
) -> Vec<&'static str> {
    let mut out = vec!["All"];
    if !task.summary && !task.is_milestone() {
        out.push(if critical {
            "Critical Tasks"
        } else {
            "Noncritical Tasks"
        });
    }
    if task.is_milestone() {
        out.push("Milestone Tasks");
    }
    if task.summary {
        out.push(if task.uid == 0 {
            "Project Summary Task"
        } else {
            "Summary Tasks"
        });
    }
    if task.manual {
        out.push("Manually Scheduled Tasks");
    }
    if inactive {
        out.push("Inactive Tasks");
    }
    out
}

/// A task row's style from the plan's formatting; `None` when nothing
/// applies.
pub(crate) fn row_style(
    v: &ProjectView,
    task: &projcore::Task,
    critical: bool,
    inactive: bool,
) -> Option<TextStyle> {
    let formatting = &v.ed.project().formatting;
    if formatting.text_styles.is_empty() {
        return None;
    }
    let style = formatting.style_for(&row_categories(task, critical, inactive));
    (!style.is_empty()).then_some(style)
}

/// A cell's style: the row's with the cell's own on top.
pub(crate) fn cell_style(
    v: &ProjectView,
    row: Option<&TextStyle>,
    uid: Option<i32>,
    column: &str,
) -> Option<TextStyle> {
    let own = uid.and_then(|uid| v.ed.project().formatting.cell(uid, column));
    match (row, own) {
        (None, None) => None,
        (Some(r), None) => Some(r.clone()),
        (None, Some(o)) => Some(o.clone()),
        (Some(r), Some(o)) => Some(r.clone().over(o)),
    }
}

/// The style of one category alone (`Column Titles`, `Middle Timescale
/// Tier`), when set.
pub(crate) fn category_style(v: &ProjectView, category: &str) -> Option<TextStyle> {
    v.ed.project()
        .formatting
        .category(category)
        .filter(|s| !s.is_empty())
        .cloned()
}

/// `el` drawn in `style`: font, size (points), weight, italic, underline,
/// colour and background, each only when the style sets it.
pub(crate) fn styled<E: gpui::Styled>(el: E, style: &TextStyle) -> E {
    use gpui::{FontWeight, px};
    let mut el = el;
    if let Some(font) = &style.font {
        el = el.font_family(font.clone());
    }
    if let Some(size) = style.size {
        el = el.text_size(px(size * 96. / 72.));
    }
    match style.bold {
        Some(true) => el = el.font_weight(FontWeight::BOLD),
        Some(false) => el = el.font_weight(FontWeight::NORMAL),
        None => {}
    }
    if style.italic == Some(true) {
        el = el.italic();
    }
    if style.underline == Some(true) {
        el = el.underline();
    }
    if let Some(color) = style.color {
        el = el.text_color(crate::hsla_u(color));
    }
    if let Some(bg) = style.background {
        el = el.bg(crate::hsla_u(bg));
    }
    el
}

/// A view's cache cell.
pub(crate) fn new_cache() -> RefCell<PluginCache> {
    RefCell::new(PluginCache::default())
}

#[cfg(test)]
mod tests;
