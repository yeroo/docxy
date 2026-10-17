//! The JSON a UI plugin exchanges with its host ([`crate::plugin_abi`]'s
//! optional symbols), as typed values: the version [`Target`] a library was
//! built for, its static [`Manifest`] (ribbon controls, menu items, dialog
//! specs, palettes, text style categories), the [`StateCtx`] a host sends and
//! the [`State`] it gets back (enabled and checked controls, dialog values),
//! a [`Command`] and its [`CommandResult`] (control-verb ops the host applies
//! as one undo step), and the [`OverlayCtx`] and [`Shape`]s of the Gantt
//! chart. Every shape is plain data; the host renders it and knows nothing
//! of any plugin's features or names.
//!
//! Unknown keys are ignored everywhere, so a later ABI can add some; a
//! missing optional key is its default. Colours are `"#RRGGBB"` strings or
//! numbers (`0xRRGGBB`).

use crate::json::Json;

// ---- the version target -------------------------------------------------------

/// What `yppx_plugin_target` returns: the host versions the library was
/// built for, as requirements ([`version_satisfies`]) by host name, and the
/// docxy commit it was built against.
///
/// ```json
/// {"suite":"0.1.*","yppxy":"0.1.*","built_against":"278bcfb2"}
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Target {
    /// Requirement by host name (`suite`, `yppxy`); a host not named is not
    /// targeted and skips the library.
    pub hosts: Vec<(String, String)>,
    pub built_against: String,
}

impl Target {
    pub fn parse(json: &str) -> Result<Target, String> {
        let v = Json::parse(json).map_err(|e| format!("the target JSON: {e}"))?;
        let fields = v.as_object().ok_or("the target JSON is not an object")?;
        let mut target = Target::default();
        for (key, value) in fields {
            match (key.as_str(), value) {
                ("built_against", Json::Str(s)) => target.built_against = s.clone(),
                ("built_against", _) => {}
                (host, Json::Str(req)) => target.hosts.push((host.to_string(), req.clone())),
                (host, Json::Null) => {
                    let _ = host;
                }
                (host, _) => return Err(format!("the target for \"{host}\" is not a string")),
            }
        }
        Ok(target)
    }

    /// The requirement for host `name`.
    pub fn requirement(&self, name: &str) -> Option<&str> {
        self.hosts
            .iter()
            .find(|(h, _)| h == name)
            .map(|(_, r)| r.as_str())
    }

    /// Whether host `name` at `version` may load the library: `Ok(())`, or
    /// why not ("targets suite 0.1.*, this is 0.2.0").
    pub fn check(&self, name: &str, version: &str) -> Result<(), String> {
        let Some(req) = self.requirement(name) else {
            return Err(format!(
                "targets no {name} (built against {})",
                self.built_against
            ));
        };
        match version_satisfies(req, version) {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!("targets {name} {req}, this is {version}")),
            Err(e) => Err(format!("targets {name} \"{req}\": {e}")),
        }
    }

    pub fn json(&self) -> String {
        let mut pairs: Vec<(&str, Json)> = self
            .hosts
            .iter()
            .map(|(h, r)| (h.as_str(), Json::str(r.clone())))
            .collect();
        pairs.push(("built_against", Json::str(self.built_against.clone())));
        Json::obj(pairs).to_string()
    }
}

/// A version as `major.minor.patch` (a pre-release or build suffix is
/// dropped, a missing part is 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    pub fn parse(text: &str) -> Result<Version, String> {
        let core = text.trim().split(['-', '+']).next().unwrap_or_default();
        let mut parts = [0u64; 3];
        let mut n = 0;
        for (i, part) in core.split('.').enumerate() {
            if i >= 3 {
                return Err(format!("\"{text}\" has more than three parts"));
            }
            parts[i] = part
                .parse()
                .map_err(|_| format!("\"{text}\" is not a version"))?;
            n = i + 1;
        }
        if n == 0 {
            return Err(format!("\"{text}\" is not a version"));
        }
        Ok(Version(parts[0], parts[1], parts[2]))
    }
}

/// Whether `version` satisfies the requirement `req`: comma-separated
/// comparators, each `=`, `>`, `>=`, `<`, `<=` and a version; `^x.y.z` or a
/// bare `x.y.z` (compatible: the same leading non-zero part); `~x.y` (the
/// same major and minor); `x.y.*`, `x.*` and `*` (wildcards, `x` too). A
/// comparator with fewer parts compares on the parts it has (`>=0.1` is
/// `>=0.1.0`, `<0.2` is below `0.2.0`).
pub fn version_satisfies(req: &str, version: &str) -> Result<bool, String> {
    let v = Version::parse(version)?;
    let req = req.trim();
    if req.is_empty() {
        return Err("an empty requirement".into());
    }
    for part in req.split(',') {
        if !comparator_holds(part.trim(), v)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn comparator_holds(part: &str, v: Version) -> Result<bool, String> {
    if part.is_empty() {
        return Err("an empty comparator".into());
    }
    let (op, rest) = ["<=", ">=", "=", "<", ">", "^", "~"]
        .into_iter()
        .find_map(|op| part.strip_prefix(op).map(|r| (op, r.trim())))
        .unwrap_or(("^", part));
    // Wildcards: `*`, `x`, `X` as a whole part.
    let wild = |s: &str| matches!(s, "*" | "x" | "X");
    if wild(rest) {
        return Ok(true);
    }
    let parts: Vec<&str> = rest.split('.').collect();
    if parts.len() > 3 {
        return Err(format!("\"{part}\" has more than three parts"));
    }
    let fixed: Vec<u64> = parts
        .iter()
        .take_while(|p| !wild(p))
        .map(|p| {
            p.parse::<u64>()
                .map_err(|_| format!("\"{part}\" is not a version"))
        })
        .collect::<Result<_, _>>()?;
    if fixed.is_empty() {
        return Err(format!("\"{part}\" is not a version"));
    }
    if parts.len() > fixed.len() && parts[fixed.len()..].iter().any(|p| !wild(p)) {
        return Err(format!("\"{part}\" mixes a wildcard and a number"));
    }
    let wildcard = parts.len() > fixed.len();
    let n = fixed.len();
    let want = Version(
        fixed[0],
        fixed.get(1).copied().unwrap_or(0),
        fixed.get(2).copied().unwrap_or(0),
    );
    let same_prefix = |k: usize| {
        (k == 0 || v.0 == want.0) && (k <= 1 || v.1 == want.1) && (k <= 2 || v.2 == want.2)
    };
    Ok(match op {
        _ if wildcard => same_prefix(n),
        "=" => same_prefix(n),
        ">" => {
            // `>0.1` is above every 0.1.x.
            let top = match n {
                1 => Version(want.0, u64::MAX, u64::MAX),
                2 => Version(want.0, want.1, u64::MAX),
                _ => want,
            };
            v > top
        }
        ">=" => v >= want,
        "<" => v < want,
        "<=" => {
            let top = match n {
                1 => Version(want.0, u64::MAX, u64::MAX),
                2 => Version(want.0, want.1, u64::MAX),
                _ => want,
            };
            v <= top
        }
        "~" => v >= want && (n == 1 && v.0 == want.0 || n > 1 && v.0 == want.0 && v.1 == want.1),
        // Caret: the same leading non-zero part, at least the version named.
        _ => {
            if v < want {
                return Ok(false);
            }
            if want.0 > 0 || n == 1 {
                v.0 == want.0
            } else if want.1 > 0 || n == 2 {
                v.0 == want.0 && v.1 == want.1
            } else {
                v == want
            }
        }
    })
}

// ---- the manifest -----------------------------------------------------------

/// What `yppx_ui_manifest` returns: the plugin's static contributions.
///
/// ```json
/// {"ribbon":[{"tab":"Task","groups":[{"title":"Extras","controls":[
///     {"id":"x.hello","kind":"button","label":"Hello","tip":"Say hello","keytip":"XH","large":true}]}]}],
///  "menus":[{"menu":"row","items":[{"id":"x.hello","label":"Hello","after":"Notes..."}]}],
///  "dialogs":[{"id":"x.options","title":"Options","pages":[{"name":"General","controls":[
///     {"name":"note","label":"&Note:","kind":"multiline"}]}]}],
///  "palettes":[{"id":"x.blue","label":"Blue","bars":[{"kind":"on-track","color":"#3366CC","shape":"bar"}]}],
///  "text_style_categories":["Late Tasks"]}
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Manifest {
    pub ribbon: Vec<RibbonTab>,
    pub menus: Vec<MenuItems>,
    pub dialogs: Vec<DialogSpec>,
    pub palettes: Vec<Palette>,
    pub text_style_categories: Vec<String>,
}

/// One project ribbon tab's contributions: groups appended to the tab, or
/// controls added to a group the tab already has (by `title`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RibbonTab {
    /// `Task`, `Resource`, `Report`, `Project`, `View`, `Help` or `Gantt
    /// Chart Format`.
    pub tab: String,
    pub groups: Vec<RibbonGroup>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RibbonGroup {
    pub title: String,
    pub controls: Vec<Control>,
}

/// A ribbon control. Its `id` is what `yppx_ui_state` reports on and what
/// `yppx_ui_command` receives; it never collides with a built-in's.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Control {
    pub id: String,
    pub kind: ControlKind,
    pub label: String,
    /// The screentip's title; the label when empty.
    pub tip: String,
    /// The Alt-access letters.
    pub keytip: String,
    /// An icon of the host's set, or empty.
    pub icon: String,
    /// A big icon-over-label button.
    pub large: bool,
    /// A split or drop-down's menu, a combo's choices.
    pub items: Vec<Item>,
    /// A gallery's tiles.
    pub swatches: Vec<Swatch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ControlKind {
    #[default]
    Button,
    /// Checked as `yppx_ui_state` says.
    Toggle,
    /// A primary command and a menu of items.
    Split,
    /// A button whose press opens a menu of items.
    Dropdown,
    /// A well of swatches; the checked one is `yppx_ui_state`'s.
    Gallery,
    /// A box showing the chosen item, with the items to pick from.
    Combo,
}

impl ControlKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Button => "button",
            Self::Toggle => "toggle",
            Self::Split => "split",
            Self::Dropdown => "dropdown",
            Self::Gallery => "gallery",
            Self::Combo => "combo",
        }
    }

    pub fn from_name(s: &str) -> Option<ControlKind> {
        Some(match s {
            "button" => Self::Button,
            "toggle" => Self::Toggle,
            "split" => Self::Split,
            "dropdown" => Self::Dropdown,
            "gallery" => Self::Gallery,
            "combo" => Self::Combo,
            _ => return None,
        })
    }
}

/// A menu item of a split or drop-down, or a combo's choice: a command id.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Item {
    pub id: String,
    pub label: String,
    pub tip: String,
    pub icon: String,
}

/// A gallery tile: a command id with a colour and a preview hint.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Swatch {
    pub id: String,
    pub label: String,
    /// `0xRRGGBB`.
    pub color: Option<u32>,
    /// A palette id ([`Palette`]) the tile previews, or any text.
    pub preview: String,
}

/// Items for one context menu.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MenuItems {
    /// `row`, `bar`, `column`, `chart` or `heading-filter`.
    pub menu: String,
    pub items: Vec<MenuItem>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct MenuItem {
    pub id: String,
    pub label: String,
    pub icon: String,
    /// The label of the built-in item to go after; at the end when empty or
    /// unknown.
    pub after: String,
}

/// A dialog by id. `task-information` adds pages to the host's Task
/// Information dialog (one named like a built-in page replaces it).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DialogSpec {
    pub id: String,
    pub title: String,
    pub pages: Vec<DialogPage>,
    /// `OK`, `Cancel`, `Help`, in order; OK and Cancel when empty.
    pub buttons: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DialogPage {
    pub name: String,
    pub controls: Vec<DialogControl>,
}

/// A dialog control. `kind` is one of `text`, `number`, `date`, `duration`,
/// `checkbox`, `radio`, `dropdown`, `list`, `grid`, `multiline`, `label`,
/// `color`, `font`; the host draws the ones it has and the rest as text.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DialogControl {
    pub name: String,
    pub label: String,
    pub kind: String,
    /// A radio group, dropdown or list's items.
    pub items: Vec<String>,
    /// A grid's columns.
    pub columns: Vec<GridColumn>,
    /// The value the control opens on when `yppx_ui_state` gives none.
    pub value: Json,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct GridColumn {
    pub name: String,
    pub kind: String,
}

/// A bar palette preset for the gallery and the plan's formatting.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Palette {
    pub id: String,
    pub label: String,
    pub bars: Vec<BarStyle>,
}

/// One bar kind's colour and shape in a palette.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BarStyle {
    /// `critical`, `on-track`, `summary`, `milestone`, `baseline`,
    /// `progress`, `delay`, `inactive`.
    pub kind: String,
    /// `0xRRGGBB`.
    pub color: u32,
    /// `bar`, `hollow`, `summary`, `diamond`, `thin`; `bar` when empty.
    pub shape: String,
}

fn parse_color(v: Option<&Json>) -> Option<u32> {
    match v? {
        Json::Num(n) if *n >= 0.0 && *n <= 0xFFFFFF as f64 => Some(*n as u32),
        Json::Str(s) => {
            let hex = s.trim().trim_start_matches('#');
            (hex.len() == 6)
                .then(|| u32::from_str_radix(hex, 16).ok())
                .flatten()
        }
        _ => None,
    }
}

fn items_of(v: &Json) -> Vec<Item> {
    v.list("items")
        .iter()
        .map(|it| Item {
            id: it.text("id"),
            label: it.text("label"),
            tip: it.text("tip"),
            icon: it.text("icon"),
        })
        .collect()
}

impl Manifest {
    pub fn parse(json: &str) -> Result<Manifest, String> {
        let v = Json::parse(json).map_err(|e| format!("the manifest JSON: {e}"))?;
        if v.as_object().is_none() {
            return Err("the manifest JSON is not an object".into());
        }
        let mut m = Manifest::default();
        for tab in v.list("ribbon") {
            let mut groups = Vec::new();
            for g in tab.list("groups") {
                let mut controls = Vec::new();
                for c in g.list("controls") {
                    let kind = c.text("kind");
                    let kind = if kind.is_empty() {
                        ControlKind::Button
                    } else {
                        ControlKind::from_name(&kind).ok_or_else(|| {
                            format!("control \"{}\": unknown kind \"{kind}\"", c.text("id"))
                        })?
                    };
                    let id = c.text("id");
                    if id.is_empty() {
                        return Err(format!(
                            "a control of group \"{}\" has no id",
                            g.text("title")
                        ));
                    }
                    controls.push(Control {
                        id,
                        kind,
                        label: c.text("label"),
                        tip: c.text("tip"),
                        keytip: c.text("keytip"),
                        icon: c.text("icon"),
                        large: c.flag("large"),
                        items: items_of(c),
                        swatches: c
                            .list("swatches")
                            .iter()
                            .map(|s| Swatch {
                                id: s.text("id"),
                                label: s.text("label"),
                                color: parse_color(s.get("color")),
                                preview: s.text("preview"),
                            })
                            .collect(),
                    });
                }
                groups.push(RibbonGroup {
                    title: g.text("title"),
                    controls,
                });
            }
            m.ribbon.push(RibbonTab {
                tab: tab.text("tab"),
                groups,
            });
        }
        for menu in v.list("menus") {
            m.menus.push(MenuItems {
                menu: menu.text("menu"),
                items: menu
                    .list("items")
                    .iter()
                    .map(|it| MenuItem {
                        id: it.text("id"),
                        label: it.text("label"),
                        icon: it.text("icon"),
                        after: it.text("after"),
                    })
                    .collect(),
            });
        }
        for d in v.list("dialogs") {
            let id = d.text("id");
            if id.is_empty() {
                return Err("a dialog has no id".into());
            }
            m.dialogs.push(DialogSpec {
                id,
                title: d.text("title"),
                pages: d
                    .list("pages")
                    .iter()
                    .map(|p| DialogPage {
                        name: p.text("name"),
                        controls: p
                            .list("controls")
                            .iter()
                            .map(|c| DialogControl {
                                name: c.text("name"),
                                label: c.text("label"),
                                kind: c.text("kind"),
                                items: c.texts("items"),
                                columns: c
                                    .list("columns")
                                    .iter()
                                    .map(|col| GridColumn {
                                        name: col.text("name"),
                                        kind: col.text("kind"),
                                    })
                                    .collect(),
                                value: c.get("value").cloned().unwrap_or(Json::Null),
                            })
                            .collect(),
                    })
                    .collect(),
                buttons: d.texts("buttons"),
            });
        }
        for p in v.list("palettes") {
            m.palettes.push(Palette {
                id: p.text("id"),
                label: p.text("label"),
                bars: p
                    .list("bars")
                    .iter()
                    .map(|b| BarStyle {
                        kind: b.text("kind"),
                        color: parse_color(b.get("color")).unwrap_or(0),
                        shape: b.text("shape"),
                    })
                    .collect(),
            });
        }
        m.text_style_categories = v.texts("text_style_categories");
        Ok(m)
    }

    /// Every control id in the ribbon, menus included (a split or drop-down
    /// item's too).
    pub fn command_ids(&self) -> Vec<&str> {
        let mut out = Vec::new();
        for tab in &self.ribbon {
            for g in &tab.groups {
                for c in &g.controls {
                    out.push(c.id.as_str());
                    out.extend(c.items.iter().map(|i| i.id.as_str()));
                    out.extend(c.swatches.iter().map(|s| s.id.as_str()));
                }
            }
        }
        for m in &self.menus {
            out.extend(m.items.iter().map(|i| i.id.as_str()));
        }
        out
    }

    pub fn dialog(&self, id: &str) -> Option<&DialogSpec> {
        self.dialogs.iter().find(|d| d.id == id)
    }
}

// ---- state -------------------------------------------------------------------

/// The table's selection, as the host sends it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Selection {
    /// The selected tasks' uids, the cursor's first.
    pub uids: Vec<i32>,
    /// The selected columns by heading.
    pub cols: Vec<String>,
}

impl Selection {
    fn json(&self) -> Json {
        Json::obj(vec![
            (
                "uids",
                Json::Arr(self.uids.iter().map(|u| Json::Num(f64::from(*u))).collect()),
            ),
            ("cols", Json::strs(&self.cols)),
        ])
    }
}

/// What the host sends `yppx_ui_state`: the plan (MSPDI), the selection,
/// the view's switches, and the dialog about to open, when one is.
#[derive(Debug, Clone, PartialEq)]
pub struct StateCtx {
    pub plan: String,
    pub selection: Selection,
    pub view: Json,
    /// `(dialog id, task uid)` when the host is about to open a dialog and
    /// wants its values prefilled.
    pub dialog: Option<(String, Option<i32>)>,
}

impl StateCtx {
    pub fn json(&self) -> String {
        let mut pairs = vec![
            ("plan", Json::str(self.plan.clone())),
            ("selection", self.selection.json()),
            ("view", self.view.clone()),
        ];
        if let Some((id, uid)) = &self.dialog {
            pairs.push((
                "dialog",
                Json::obj(vec![
                    ("id", Json::str(id.clone())),
                    ("uid", uid.map_or(Json::Null, |u| Json::Num(f64::from(u)))),
                ]),
            ));
        }
        Json::obj(pairs).to_string()
    }
}

/// What `yppx_ui_state` returns.
///
/// ```json
/// {"controls":{"x.toggle":{"enabled":true,"checked":false}},
///  "gallery":{"x.palette":"x.blue"},
///  "values":{"note":"text","done":true,"grid":[["a","1"]]}}
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
pub struct State {
    pub controls: Vec<(String, ControlState)>,
    /// The checked swatch per gallery id.
    pub gallery: Vec<(String, String)>,
    /// A dialog's values by control name.
    pub values: Vec<(String, Json)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ControlState {
    /// `None` leaves the control enabled.
    pub enabled: Option<bool>,
    pub checked: Option<bool>,
}

impl State {
    pub fn parse(json: &str) -> Result<State, String> {
        let v = Json::parse(json).map_err(|e| format!("the state JSON: {e}"))?;
        let mut state = State::default();
        for (id, st) in v.get("controls").and_then(Json::as_object).unwrap_or(&[]) {
            state.controls.push((
                id.clone(),
                ControlState {
                    enabled: st.get("enabled").and_then(Json::as_bool),
                    checked: st.get("checked").and_then(Json::as_bool),
                },
            ));
        }
        for (id, item) in v.get("gallery").and_then(Json::as_object).unwrap_or(&[]) {
            if let Some(item) = item.as_str() {
                state.gallery.push((id.clone(), item.to_string()));
            }
        }
        for (name, value) in v.get("values").and_then(Json::as_object).unwrap_or(&[]) {
            state.values.push((name.clone(), value.clone()));
        }
        Ok(state)
    }

    pub fn control(&self, id: &str) -> ControlState {
        self.controls
            .iter()
            .find(|(k, _)| k == id)
            .map_or(ControlState::default(), |(_, s)| *s)
    }

    pub fn checked_swatch(&self, gallery: &str) -> Option<&str> {
        self.gallery
            .iter()
            .find(|(k, _)| k == gallery)
            .map(|(_, v)| v.as_str())
    }

    pub fn value(&self, name: &str) -> Option<&Json> {
        self.values.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }
}

// ---- commands ----------------------------------------------------------------

/// What the host sends `yppx_ui_command`: the command's id, the plan
/// (MSPDI), the selection, the view, and a dialog's values on its OK.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub id: String,
    pub plan: String,
    pub selection: Selection,
    pub view: Json,
    pub values: Option<Json>,
}

impl Command {
    pub fn json(&self) -> String {
        let mut pairs = vec![
            ("id", Json::str(self.id.clone())),
            ("plan", Json::str(self.plan.clone())),
            ("selection", self.selection.json()),
            ("view", self.view.clone()),
        ];
        if let Some(values) = &self.values {
            pairs.push(("values", values.clone()));
        }
        Json::obj(pairs).to_string()
    }
}

/// One control verb the host applies: `{"verb":"task.set","args":{...}}`
/// (`projctl`'s editor verbs: `task.set`, `task.add`, `task.del`,
/// `link.add`, `link.del`, `assign.add`, `assign.set`, `assign.del`, ...).
#[derive(Debug, Clone, PartialEq)]
pub struct Op {
    pub verb: String,
    pub args: Json,
}

/// What `yppx_ui_command` returns.
///
/// ```json
/// {"ops":[{"verb":"task.set","args":{"uid":3,"name":"Renamed"}}],
///  "status":"Renamed task 3","dialog":"x.options","view":{"palette":"x.blue"}}
/// ```
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CommandResult {
    pub ops: Vec<Op>,
    pub status: Option<String>,
    /// A dialog spec id to open; its OK sends the command again with `values`.
    pub dialog: Option<String>,
    /// View settings to apply (palette id, text styles, flags), by key.
    pub view: Option<Json>,
}

impl CommandResult {
    pub fn parse(json: &str) -> Result<CommandResult, String> {
        let v = Json::parse(json).map_err(|e| format!("the command reply JSON: {e}"))?;
        if v.as_object().is_none() {
            return Err("the command reply is not an object".into());
        }
        let mut out = CommandResult::default();
        for (n, op) in v.list("ops").iter().enumerate() {
            let verb = op.text("verb");
            if verb.is_empty() {
                return Err(format!("op {} has no verb", n + 1));
            }
            out.ops.push(Op {
                verb,
                args: op
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| Json::Obj(Vec::new())),
            });
        }
        out.status = v.get_str("status").map(str::to_string);
        out.dialog = v.get_str("dialog").map(str::to_string);
        out.view = v.get("view").filter(|w| !w.is_null()).cloned();
        Ok(out)
    }

    pub fn json(&self) -> String {
        let mut pairs = vec![(
            "ops",
            Json::Arr(
                self.ops
                    .iter()
                    .map(|op| {
                        Json::obj(vec![
                            ("verb", Json::str(op.verb.clone())),
                            ("args", op.args.clone()),
                        ])
                    })
                    .collect(),
            ),
        )];
        if let Some(s) = &self.status {
            pairs.push(("status", Json::str(s.clone())));
        }
        if let Some(d) = &self.dialog {
            pairs.push(("dialog", Json::str(d.clone())));
        }
        if let Some(w) = &self.view {
            pairs.push(("view", w.clone()));
        }
        Json::obj(pairs).to_string()
    }
}

// ---- the overlay -------------------------------------------------------------

/// What the host sends `yppx_ui_overlay`: the chart as drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayCtx {
    /// The rows in view, top to bottom.
    pub rows: Vec<OverlayRow>,
    /// Day number at x = 0 and the width of a day.
    pub origin_day: i64,
    pub day_w: f32,
    pub row_h: f32,
    pub links: Vec<OverlayLink>,
    /// The plan's status date, `YYYY-MM-DDTHH:MM`, when set.
    pub status_date: Option<String>,
    /// The view's switches (`critical`, `baseline`, ...), by key.
    pub flags: Json,
}

/// One row of the chart: its task and its bar's rectangle in chart pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayRow {
    pub uid: i32,
    pub row_y: f32,
    /// `(x, y, w, h)` of the bar, or the milestone's diamond.
    pub bar: Option<(f32, f32, f32, f32)>,
    /// `critical`, `on-track`, `summary` or `milestone`.
    pub kind: String,
    pub summary: bool,
    pub milestone: bool,
    pub critical: bool,
    pub inactive: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OverlayLink {
    pub pred: i32,
    pub succ: i32,
    /// `FS`, `SS`, `FF` or `SF`.
    pub kind: String,
    /// The lag as the predecessor cell shows it (`2d`, `-1h`, `50%`).
    pub lag: String,
}

impl OverlayCtx {
    pub fn json(&self) -> String {
        let num = |f: f32| Json::Num(f64::from(f));
        let rows = self
            .rows
            .iter()
            .map(|r| {
                Json::obj(vec![
                    ("uid", Json::Num(f64::from(r.uid))),
                    ("row_y", num(r.row_y)),
                    (
                        "bar",
                        r.bar.map_or(Json::Null, |(x, y, w, h)| {
                            Json::obj(vec![
                                ("x", num(x)),
                                ("y", num(y)),
                                ("w", num(w)),
                                ("h", num(h)),
                            ])
                        }),
                    ),
                    ("kind", Json::str(r.kind.clone())),
                    ("summary", Json::Bool(r.summary)),
                    ("milestone", Json::Bool(r.milestone)),
                    ("critical", Json::Bool(r.critical)),
                    ("inactive", Json::Bool(r.inactive)),
                ])
            })
            .collect();
        let links = self
            .links
            .iter()
            .map(|l| {
                Json::obj(vec![
                    ("pred", Json::Num(f64::from(l.pred))),
                    ("succ", Json::Num(f64::from(l.succ))),
                    ("type", Json::str(l.kind.clone())),
                    ("lag", Json::str(l.lag.clone())),
                ])
            })
            .collect();
        Json::obj(vec![
            ("rows", Json::Arr(rows)),
            (
                "scale",
                Json::obj(vec![
                    ("origin_day", Json::Num(self.origin_day as f64)),
                    ("day_w", num(self.day_w)),
                    ("row_h", num(self.row_h)),
                ]),
            ),
            ("links", Json::Arr(links)),
            (
                "status_date",
                self.status_date.clone().map_or(Json::Null, Json::Str),
            ),
            ("flags", self.flags.clone()),
        ])
        .to_string()
    }
}

/// A shape drawn over the chart, in chart pixels (x from the scale's
/// origin, y from the top of the first row). Colours `0xRRGGBB`.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Polyline {
        points: Vec<(f32, f32)>,
        color: u32,
        width: f32,
    },
    /// A polyline drawn dashed.
    Dashed {
        points: Vec<(f32, f32)>,
        color: u32,
        width: f32,
    },
    /// A line from `from` to `to` with an arrowhead at `to`.
    Arrow {
        from: (f32, f32),
        to: (f32, f32),
        color: u32,
        width: f32,
    },
    /// A bar `x0..x1` at `y` with down ticks `tick` tall at both ends.
    Bracket {
        x0: f32,
        x1: f32,
        y: f32,
        tick: f32,
        color: u32,
        width: f32,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: u32,
        filled: bool,
    },
    /// Text with its top-left corner at `(x, y)`, `size` pixels tall.
    Text {
        x: f32,
        y: f32,
        text: String,
        color: u32,
        size: f32,
    },
}

/// Parse `yppx_ui_overlay`'s reply: `{"shapes":[...]}` or a bare array.
///
/// ```json
/// {"shapes":[
///   {"kind":"polyline","points":[[10,5],[10,30],[40,30]],"color":"#000000","width":1},
///   {"kind":"arrow","from":[40,30],"to":[48,30],"color":"#000000"},
///   {"kind":"bracket","x0":0,"x1":100,"y":12,"tick":6,"color":"#333333"},
///   {"kind":"rect","x":0,"y":8,"w":50,"h":4,"color":"#2AA79B","filled":true},
///   {"kind":"dashed","points":[[60,0],[60,200]],"color":"#DC322F"},
///   {"kind":"text","x":120,"y":4,"text":"Alice","color":"#333333","size":11}]}
/// ```
pub fn parse_shapes(json: &str) -> Result<Vec<Shape>, String> {
    let v = Json::parse(json).map_err(|e| format!("the overlay JSON: {e}"))?;
    let items = match &v {
        Json::Arr(items) => items.as_slice(),
        Json::Obj(_) => v.list("shapes"),
        _ => return Err("the overlay JSON is not an object or an array".into()),
    };
    let f = |o: &Json, key: &str| o.num(key).map(|n| n as f32);
    let point = |p: &Json| -> Option<(f32, f32)> {
        let xy = p.as_array()?;
        Some((xy.first()?.as_f64()? as f32, xy.get(1)?.as_f64()? as f32))
    };
    let points = |o: &Json| -> Result<Vec<(f32, f32)>, String> {
        o.list("points")
            .iter()
            .map(|p| point(p).ok_or_else(|| "a point is not [x, y]".to_string()))
            .collect()
    };
    let color = |o: &Json| parse_color(o.get("color")).unwrap_or(0);
    let width = |o: &Json| f(o, "width").unwrap_or(1.0);
    let mut out = Vec::new();
    for (n, o) in items.iter().enumerate() {
        let kind = o.text("kind");
        let shape = match kind.as_str() {
            "polyline" => Shape::Polyline {
                points: points(o)?,
                color: color(o),
                width: width(o),
            },
            "dashed" => Shape::Dashed {
                points: points(o)?,
                color: color(o),
                width: width(o),
            },
            "arrow" => Shape::Arrow {
                from: o
                    .get("from")
                    .and_then(point)
                    .ok_or_else(|| format!("shape {}: an arrow needs from [x, y]", n + 1))?,
                to: o
                    .get("to")
                    .and_then(point)
                    .ok_or_else(|| format!("shape {}: an arrow needs to [x, y]", n + 1))?,
                color: color(o),
                width: width(o),
            },
            "bracket" => Shape::Bracket {
                x0: f(o, "x0").unwrap_or(0.0),
                x1: f(o, "x1").unwrap_or(0.0),
                y: f(o, "y").unwrap_or(0.0),
                tick: f(o, "tick").unwrap_or(4.0),
                color: color(o),
                width: width(o),
            },
            "rect" => Shape::Rect {
                x: f(o, "x").unwrap_or(0.0),
                y: f(o, "y").unwrap_or(0.0),
                w: f(o, "w").unwrap_or(0.0),
                h: f(o, "h").unwrap_or(0.0),
                color: color(o),
                filled: o.get("filled").and_then(Json::as_bool).unwrap_or(true),
            },
            "text" => Shape::Text {
                x: f(o, "x").unwrap_or(0.0),
                y: f(o, "y").unwrap_or(0.0),
                text: o.text("text"),
                color: color(o),
                size: f(o, "size").unwrap_or(11.0),
            },
            other => return Err(format!("shape {}: unknown kind \"{other}\"", n + 1)),
        };
        out.push(shape);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(Version::parse("0.1.0").unwrap(), Version(0, 1, 0));
        assert_eq!(Version::parse("1.2").unwrap(), Version(1, 2, 0));
        assert_eq!(Version::parse("1.2.3-beta+5").unwrap(), Version(1, 2, 3));
        assert!(Version::parse("").is_err());
        assert!(Version::parse("1.x").is_err());
        assert!(Version::parse("1.2.3.4").is_err());
        assert!(Version(0, 2, 0) > Version(0, 1, 9));
    }

    #[test]
    fn requirements_match_as_cargo_reads_them() {
        let ok = |req: &str, v: &str| version_satisfies(req, v).unwrap();
        // Wildcards.
        assert!(ok("0.1.*", "0.1.0") && ok("0.1.*", "0.1.7") && !ok("0.1.*", "0.2.0"));
        assert!(ok("0.1.x", "0.1.3") && ok("*", "9.9.9") && ok("0.*", "0.5.0"));
        // Caret, bare and explicit.
        assert!(ok("0.1.0", "0.1.0") && ok("^0.1.0", "0.1.9") && !ok("0.1.0", "0.2.0"));
        assert!(ok("1.2", "1.9.0") && !ok("1.2", "2.0.0") && !ok("^1.2.3", "1.2.2"));
        assert!(ok("0.0.3", "0.0.3") && !ok("0.0.3", "0.0.4"));
        assert!(ok("0.1", "0.1.5") && !ok("0.1", "0.2.0"));
        // Tilde.
        assert!(ok("~0.1.2", "0.1.9") && !ok("~0.1.2", "0.2.0") && !ok("~0.1.2", "0.1.1"));
        assert!(ok("~1", "1.9.9") && !ok("~1", "2.0.0"));
        // Comparators and lists.
        assert!(ok("=0.1.0", "0.1.0") && !ok("=0.1.0", "0.1.1") && ok("=0.1", "0.1.4"));
        assert!(ok(">=0.1, <0.2", "0.1.3") && !ok(">=0.1, <0.2", "0.2.0"));
        assert!(ok(">0.1", "0.2.0") && !ok(">0.1", "0.1.9") && ok("<=0.1", "0.1.9"));
        assert!(ok("<0.1.0", "0.0.9") && !ok("<0.1.0", "0.1.0"));
        // Errors.
        for bad in ["", "abc", "1.2.*.4", "1.*.3", ">=", "0.1, "] {
            assert!(version_satisfies(bad, "0.1.0").is_err(), "{bad}");
        }
        assert!(version_satisfies("0.1", "zero").is_err());
    }

    #[test]
    fn the_target_checks_the_host_by_name() {
        let t = Target::parse(
            r#"{"suite":"0.1.*","yppxy":">=0.1, <0.3","built_against":"abc1234","later":null}"#,
        )
        .unwrap();
        assert_eq!(t.built_against, "abc1234");
        assert_eq!(t.requirement("suite"), Some("0.1.*"));
        assert_eq!(t.check("suite", "0.1.0"), Ok(()));
        assert_eq!(
            t.check("suite", "0.2.0").unwrap_err(),
            "targets suite 0.1.*, this is 0.2.0"
        );
        assert_eq!(t.check("yppxy", "0.2.9"), Ok(()));
        assert_eq!(
            t.check("docxy", "0.5.0").unwrap_err(),
            "targets no docxy (built against abc1234)"
        );
        let back = Target::parse(&t.json()).unwrap();
        assert_eq!(back, t);
        assert!(Target::parse("[]").is_err());
        assert!(Target::parse(r#"{"suite":1}"#).is_err());
        let bad = Target::parse(r#"{"suite":"nope"}"#).unwrap();
        assert!(
            bad.check("suite", "0.1.0")
                .unwrap_err()
                .contains("not a version")
        );
    }

    const MANIFEST: &str = r##"{
      "ribbon":[{"tab":"Task","groups":[{"title":"Fixture","controls":[
         {"id":"fx.hello","kind":"button","label":"Hello","tip":"Say hello","keytip":"XH","large":true,"icon":"comment"},
         {"id":"fx.flag","kind":"toggle","label":"Flag"},
         {"id":"fx.more","kind":"split","label":"More","items":[{"id":"fx.a","label":"A"},{"id":"fx.b","label":"B","icon":"cut"}]},
         {"id":"fx.palette","kind":"gallery","label":"Palette","swatches":[
            {"id":"fx.blue","label":"Blue","color":"#3366CC","preview":"fx.blue"},
            {"id":"fx.red","label":"Red","color":13382451}]}]}]}],
      "menus":[{"menu":"row","items":[{"id":"fx.hello","label":"Hello (fixture)","after":"Notes..."}]}],
      "dialogs":[{"id":"task-information","pages":[{"name":"Fixture","controls":[
         {"name":"fx.note","label":"&Note:","kind":"multiline","value":""},
         {"name":"fx.grid","label":"Items","kind":"grid","columns":[{"name":"Name","kind":"text"},{"name":"Done","kind":"checkbox"}],"value":[["a","false"]]}]}]},
         {"id":"fx.options","title":"Fixture Options","pages":[{"name":"General","controls":[{"name":"n","label":"N:","kind":"number"}]}],"buttons":["OK","Cancel","Help"]}],
      "palettes":[{"id":"fx.blue","label":"Blue","bars":[{"kind":"on-track","color":"#3366CC","shape":"bar"},{"kind":"critical","color":"#CC3333"}]}],
      "text_style_categories":["Late Tasks"],
      "since":{"abi":2}
    }"##;

    #[test]
    fn the_manifest_parses_every_contribution() {
        let m = Manifest::parse(MANIFEST).unwrap();
        assert_eq!(m.ribbon.len(), 1);
        let g = &m.ribbon[0].groups[0];
        assert_eq!(
            (m.ribbon[0].tab.as_str(), g.title.as_str()),
            ("Task", "Fixture")
        );
        assert_eq!(g.controls.len(), 4);
        let hello = &g.controls[0];
        assert_eq!(hello.kind, ControlKind::Button);
        assert!(hello.large);
        assert_eq!(
            (
                hello.tip.as_str(),
                hello.keytip.as_str(),
                hello.icon.as_str()
            ),
            ("Say hello", "XH", "comment")
        );
        assert_eq!(g.controls[1].kind, ControlKind::Toggle);
        assert_eq!(g.controls[2].kind, ControlKind::Split);
        assert_eq!(g.controls[2].items[1].icon, "cut");
        let gallery = &g.controls[3];
        assert_eq!(gallery.kind, ControlKind::Gallery);
        assert_eq!(gallery.swatches[0].color, Some(0x3366CC));
        assert_eq!(gallery.swatches[1].color, Some(0xCC3333));
        assert_eq!(gallery.swatches[0].preview, "fx.blue");
        assert_eq!(m.menus[0].menu, "row");
        assert_eq!(m.menus[0].items[0].after, "Notes...");
        let ti = m.dialog("task-information").unwrap();
        assert_eq!(ti.pages[0].name, "Fixture");
        assert_eq!(ti.pages[0].controls[1].columns[1].kind, "checkbox");
        assert_eq!(
            ti.pages[0].controls[1].value,
            Json::Arr(vec![Json::strs(["a", "false"])])
        );
        assert_eq!(
            m.dialog("fx.options").unwrap().buttons,
            ["OK", "Cancel", "Help"]
        );
        assert_eq!(m.palettes[0].bars[0].color, 0x3366CC);
        assert_eq!(m.palettes[0].bars[1].shape, "");
        assert_eq!(m.text_style_categories, ["Late Tasks"]);
        assert_eq!(
            m.command_ids(),
            [
                "fx.hello",
                "fx.flag",
                "fx.more",
                "fx.a",
                "fx.b",
                "fx.palette",
                "fx.blue",
                "fx.red",
                "fx.hello"
            ]
        );
        assert_eq!(Manifest::parse("{}").unwrap(), Manifest::default());
        assert!(Manifest::parse("[]").is_err());
        assert!(Manifest::parse(r#"{"ribbon":[{"tab":"Task","groups":[{"title":"G","controls":[{"id":"x","kind":"knob"}]}]}]}"#).is_err());
        assert!(Manifest::parse(r#"{"ribbon":[{"tab":"Task","groups":[{"title":"G","controls":[{"kind":"button"}]}]}]}"#).is_err());
        assert!(Manifest::parse(r#"{"dialogs":[{"title":"X"}]}"#).is_err());
    }

    #[test]
    fn state_round_trips_the_documented_shape() {
        let ctx = StateCtx {
            plan: "<Project/>".into(),
            selection: Selection {
                uids: vec![3, 4],
                cols: vec!["Name".into()],
            },
            view: Json::obj(vec![("critical", Json::Bool(true))]),
            dialog: Some(("task-information".into(), Some(3))),
        };
        let j = Json::parse(&ctx.json()).unwrap();
        assert_eq!(j.text("plan"), "<Project/>");
        assert_eq!(j.get("selection").unwrap().list("uids").len(), 2);
        assert_eq!(j.get("dialog").unwrap().int("uid"), Some(3));
        let state = State::parse(
            r#"{"controls":{"fx.flag":{"enabled":true,"checked":true},"fx.hello":{"enabled":false}},
                "gallery":{"fx.palette":"fx.blue"},"values":{"fx.note":"hi","fx.done":true}}"#,
        )
        .unwrap();
        assert_eq!(
            state.control("fx.flag"),
            ControlState {
                enabled: Some(true),
                checked: Some(true)
            }
        );
        assert_eq!(state.control("fx.hello").enabled, Some(false));
        assert_eq!(state.control("other"), ControlState::default());
        assert_eq!(state.checked_swatch("fx.palette"), Some("fx.blue"));
        assert_eq!(state.value("fx.note").and_then(Json::as_str), Some("hi"));
        assert_eq!(State::parse("{}").unwrap(), State::default());
    }

    #[test]
    fn commands_and_results_round_trip() {
        let cmd = Command {
            id: "fx.rename".into(),
            plan: "<Project/>".into(),
            selection: Selection::default(),
            view: Json::Obj(Vec::new()),
            values: Some(Json::obj(vec![("n", Json::Num(2.0))])),
        };
        let j = Json::parse(&cmd.json()).unwrap();
        assert_eq!(j.text("id"), "fx.rename");
        assert_eq!(j.get("values").unwrap().int("n"), Some(2));
        let result = CommandResult::parse(
            r#"{"ops":[{"verb":"task.set","args":{"uid":3,"name":"Renamed"}},{"verb":"task.del","args":{"uid":4}}],
                "status":"Renamed task 3","dialog":"fx.options","view":{"palette":"fx.blue"}}"#,
        )
        .unwrap();
        assert_eq!(result.ops.len(), 2);
        assert_eq!(result.ops[0].verb, "task.set");
        assert_eq!(result.ops[0].args.int("uid"), Some(3));
        assert_eq!(result.status.as_deref(), Some("Renamed task 3"));
        assert_eq!(result.dialog.as_deref(), Some("fx.options"));
        assert_eq!(result.view.as_ref().unwrap().text("palette"), "fx.blue");
        assert_eq!(CommandResult::parse(&result.json()).unwrap(), result);
        assert_eq!(
            CommandResult::parse("{}").unwrap(),
            CommandResult::default()
        );
        assert!(CommandResult::parse(r#"{"ops":[{"args":{}}]}"#).is_err());
        assert!(CommandResult::parse("[]").is_err());
    }

    #[test]
    fn the_overlay_context_and_shapes_round_trip() {
        let ctx = OverlayCtx {
            rows: vec![
                OverlayRow {
                    uid: 1,
                    row_y: 0.0,
                    bar: Some((22.0, 5.0, 66.0, 14.0)),
                    kind: "critical".into(),
                    summary: false,
                    milestone: false,
                    critical: true,
                    inactive: false,
                },
                OverlayRow {
                    uid: 2,
                    row_y: 28.0,
                    bar: None,
                    kind: "summary".into(),
                    summary: true,
                    milestone: false,
                    critical: false,
                    inactive: false,
                },
            ],
            origin_day: 20000,
            day_w: 22.0,
            row_h: 28.0,
            links: vec![OverlayLink {
                pred: 1,
                succ: 2,
                kind: "FS".into(),
                lag: "2d".into(),
            }],
            status_date: Some("2026-01-05T08:00".into()),
            flags: Json::obj(vec![("critical", Json::Bool(true))]),
        };
        let j = Json::parse(&ctx.json()).unwrap();
        assert_eq!(j.list("rows")[0].get("bar").unwrap().num("w"), Some(66.0));
        assert!(j.list("rows")[1].get("bar").unwrap().is_null());
        assert_eq!(j.get("scale").unwrap().int("origin_day"), Some(20000));
        assert_eq!(j.list("links")[0].text("type"), "FS");
        assert_eq!(j.text("status_date"), "2026-01-05T08:00");
        let shapes = parse_shapes(
            r##"{"shapes":[
              {"kind":"polyline","points":[[10,5],[10,30]],"color":"#000000","width":2},
              {"kind":"arrow","from":[40,30],"to":[48,30],"color":"#112233"},
              {"kind":"bracket","x0":0,"x1":100,"y":12,"tick":6,"color":"#333333"},
              {"kind":"rect","x":0,"y":8,"w":50,"h":4,"color":"#2AA79B","filled":false},
              {"kind":"dashed","points":[[60,0],[60,200]],"color":"#DC322F"},
              {"kind":"text","x":120,"y":4,"text":"Alice","color":"#333333","size":11}]}"##,
        )
        .unwrap();
        assert_eq!(shapes.len(), 6);
        assert_eq!(
            shapes[0],
            Shape::Polyline {
                points: vec![(10.0, 5.0), (10.0, 30.0)],
                color: 0,
                width: 2.0
            }
        );
        assert_eq!(
            shapes[1],
            Shape::Arrow {
                from: (40.0, 30.0),
                to: (48.0, 30.0),
                color: 0x112233,
                width: 1.0
            }
        );
        assert!(matches!(shapes[2], Shape::Bracket { x1, tick, .. } if x1 == 100.0 && tick == 6.0));
        assert!(matches!(
            shapes[3],
            Shape::Rect {
                filled: false,
                color: 0x2AA79B,
                ..
            }
        ));
        assert!(matches!(&shapes[4], Shape::Dashed { points, .. } if points.len() == 2));
        assert!(
            matches!(&shapes[5], Shape::Text { text, size, .. } if text == "Alice" && *size == 11.0)
        );
        assert_eq!(parse_shapes("[]").unwrap(), Vec::<Shape>::new());
        assert!(parse_shapes(r#"[{"kind":"blob"}]"#).is_err());
        assert!(parse_shapes(r#"[{"kind":"arrow","from":[1,2]}]"#).is_err());
        assert!(parse_shapes(r#"[{"kind":"polyline","points":[[1]]}]"#).is_err());
    }
}
