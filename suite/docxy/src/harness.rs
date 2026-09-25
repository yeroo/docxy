//! The suite's **opt-in** UI test harness surface: a [`ctlcore`] control server
//! that lets a test script drive this instance by verbs instead of by synthetic
//! desktop input.
//!
//! Wiring follows `docxy/src/control.rs` — the same `ctlcore::serve` listener,
//! the same token check (inside ctlcore), the same `reply_ok`/`reply_err`
//! shapes — so there is one style of control surface in the repo rather than
//! two. What differs is the pump: the terminal editor owns its event loop and
//! can select over requests, while here the app's thread belongs to gpui, so
//! requests are drained on the window's own foreground task (see [`attach`]).
//!
//! ## Two things this module refuses to do
//!
//! 1. **Start without isolation.** `--harness` is only honoured when
//!    `DOCXY_CONFIG_DIR` names a directory that is not the real config root.
//!    Task 1 measured why an `APPDATA` override cannot stand in for it:
//!    `dirs::config_dir()` asks the Windows known-folder API and ignores
//!    `APPDATA` entirely, so a test instance relying on it would keep writing
//!    `session.json` and the hot sidecars into the user's own profile — over
//!    the documents they have open. See [`gate`].
//! 2. **Publish its socket somewhere else.** The discovery file goes under
//!    [`control_dir`] — derived from the sandbox root — rather than through
//!    `ctlcore::config_ctl_dir`, which does its own `APPDATA` lookup and could
//!    put the socket in a different sandbox from the session state.
//!
//! Without the flag the separate Project-only control server runs instead;
//! harness verbs and its UI dialog overrides remain disabled.

use crate::control::Done;
use crate::{CONFIG_DIR_ENV, RefTarget, SheetView};
use ctlcore::json::Json;
use docxcore::editor::{Editor, FlatDocument, StoryOffset};
use docxcore::model::{Align, VertAlign};
use gpui::{App, Context, Entity, KeyDownEvent, Keystroke, Window};
use gridcore::sheet::{cell_name, parse_cell_name, parse_range_name};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

/// The command-line flag that turns the harness on.
pub const HARNESS_FLAG: &str = "--harness";

/// The environment variable that turns the harness on, for launchers that
/// cannot add an argument (equivalent to passing [`HARNESS_FLAG`]).
pub const HARNESS_ENV: &str = "DOCXY_HARNESS";

/// The app name the control surface publishes itself under. Not `"docxy"`: the
/// terminal editor already owns that, and a harness instance is a different
/// thing to address even though it is the same product.
const CTL_APP: &str = "suite";

// ---------------------------------------------------------------------------
// Command line
// ---------------------------------------------------------------------------

/// The command line, parsed. Everything that is not a flag is a candidate file
/// to open; whether it exists is the caller's business, so this stays pure.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cli {
    /// `--harness` was passed.
    pub harness: bool,
    /// Positional arguments, in order.
    pub files: Vec<PathBuf>,
    /// `--`-prefixed arguments that are not ours. Kept rather than silently
    /// dropped so a mistyped `--harnes` is reported instead of being treated as
    /// a file that does not exist and quietly ignored.
    pub unknown_flags: Vec<String>,
}

/// Parse the arguments *after* the executable name.
///
/// A literal `--` ends flag parsing, so a file genuinely named `--harness` can
/// still be opened.
pub fn parse_args<I: IntoIterator<Item = OsString>>(args: I) -> Cli {
    let mut cli = Cli::default();
    let mut positional_only = false;
    for arg in args {
        if !positional_only {
            if arg == "--" {
                positional_only = true;
                continue;
            }
            if arg == HARNESS_FLAG {
                cli.harness = true;
                continue;
            }
            if let Some(s) = arg.to_str()
                && s.starts_with("--")
            {
                cli.unknown_flags.push(s.to_string());
                continue;
            }
        }
        cli.files.push(PathBuf::from(arg));
    }
    cli
}

/// Whether [`HARNESS_ENV`]'s value means "on". Unset is off; so are the usual
/// written-out falsehoods, so `DOCXY_HARNESS=0` in a shell profile does not
/// silently enable it. Anything else set is on — including a value that is not
/// valid UTF-8, which cannot be one of the off-words.
pub fn env_flag(value: Option<OsString>) -> bool {
    match value {
        None => false,
        Some(v) => match v.to_str() {
            None => true,
            Some(s) => !matches!(
                s.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "no" | "off"
            ),
        },
    }
}

// ---------------------------------------------------------------------------
// The isolation gate
// ---------------------------------------------------------------------------

/// Decide whether a harness may start, returning the sandbox root it must use.
///
/// `over` is `DOCXY_CONFIG_DIR`'s value and `os_config` is `dirs::config_dir()`.
/// Refuses when the override is missing or blank, and when it points at the
/// real config directory — the two ways a mistyped invocation would end up
/// driving, and overwriting, the installed app's own state.
pub fn gate(over: Option<&OsStr>, os_config: Option<&Path>) -> Result<PathBuf, String> {
    let root = match over {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            return Err(format!(
                "{HARNESS_FLAG} requires {CONFIG_DIR_ENV} to point at a throwaway directory. \
                 Without it this instance writes session.json and the hot sidecars into the \
                 installed app's config and overwrites whatever the user has open. \
                 (An APPDATA override is not isolation: dirs::config_dir() ignores it.)"
            ));
        }
    };
    if let Some(os) = os_config
        && (same_dir(&root, os) || resolves_same(&root, os))
    {
        return Err(format!(
            "{CONFIG_DIR_ENV} points at the real config directory ({}); \
             the harness will not drive the installed app's own state",
            os.display()
        ));
    }
    Ok(root)
}

/// Whether two paths name the same directory textually: separators, `.`/`..`,
/// and a trailing separator are noise, and Windows paths are case-insensitive.
/// This check does not require the proposed sandbox to exist.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    norm(&lexical_normalize(a)) == norm(&lexical_normalize(b))
}

/// Whether two paths resolve to the same directory through their nearest
/// existing ancestors.
///
/// [`same_dir`] handles lexical `.`/`..` aliases, but filesystem aliases such as
/// an 8.3 short name or a junction laid over the profile need canonicalization.
/// Accepting one as a sandbox would make `session_path` and `hot_dir` write over
/// the user's own open documents, which is the single thing the gate exists to
/// stop.
///
/// Resolving the nearest existing ancestor matters for a new sandbox below a
/// junction as well as for an existing path. The remaining tail is appended and
/// normalized without creating anything on disk.
pub fn resolves_same(a: &Path, b: &Path) -> bool {
    let mut canonicalize = |path: &Path| std::fs::canonicalize(path);
    resolves_same_with(a, b, &mut canonicalize)
}

fn resolves_same_with<F>(a: &Path, b: &Path, resolve: &mut F) -> bool
where
    F: FnMut(&Path) -> std::io::Result<PathBuf>,
{
    match (
        resolve_for_compare_with(a, resolve),
        resolve_for_compare_with(b, resolve),
    ) {
        (Ok(ca), Ok(cb)) => norm(&ca) == norm(&cb),
        _ => false,
    }
}

fn resolve_for_compare_with<F>(path: &Path, resolve: &mut F) -> std::io::Result<PathBuf>
where
    F: FnMut(&Path) -> std::io::Result<PathBuf>,
{
    for ancestor in path.ancestors() {
        let candidate = if ancestor.as_os_str().is_empty() {
            Path::new(".")
        } else {
            ancestor
        };
        if let Ok(mut base) = resolve(candidate) {
            let tail = path.strip_prefix(ancestor).unwrap_or(path);
            base.push(tail);
            return Ok(lexical_normalize(&base));
        }
    }
    resolve(path)
}

fn lexical_normalize(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut base = PathBuf::new();
    let mut rooted = false;
    let mut tail: Vec<OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) => base.push(component.as_os_str()),
            Component::RootDir => {
                base.push(component.as_os_str());
                rooted = true;
            }
            Component::CurDir => {}
            Component::ParentDir => match tail.last() {
                Some(last) if last != ".." => {
                    tail.pop();
                }
                _ if !rooted => tail.push(OsString::from("..")),
                _ => {}
            },
            Component::Normal(name) => tail.push(name.to_os_string()),
        }
    }
    for component in tail {
        base.push(component);
    }
    base
}

/// A path reduced to what a comparison should care about: forward separators,
/// no trailing one, and folded case on Windows.
fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/').to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

// ---------------------------------------------------------------------------
// The control server
// ---------------------------------------------------------------------------

/// Where this instance publishes its discovery file: `<root>/suite/ctl`.
/// Derived from the sandbox root rather than looked up, so the socket and the
/// session state can never land in two different places.
pub fn control_dir(root: &Path) -> PathBuf {
    root.join(CTL_APP).join("ctl")
}

/// This instance's control name — `suite-<AGWINTERM_SESSION_ID|pid>`, the same
/// convention the terminal editors use.
pub fn instance_name() -> String {
    ctlcore::instance_name(CTL_APP)
}

/// Start the control server for a sandbox rooted at `root`.
pub fn start(root: &Path) -> std::io::Result<(ctlcore::Server, Receiver<ctlcore::Request>)> {
    ctlcore::serve(&control_dir(root), &instance_name())
}

/// Bring the harness up on `view`: drain requests on the window's foreground
/// task, apply each one to the app, and answer it.
pub fn attach(
    view: &Entity<crate::Docxy>,
    server: ctlcore::Server,
    rx: Receiver<ctlcore::Request>,
    window: &mut Window,
    cx: &mut App,
) {
    let link = crate::control::attach_with_dispatch(view, server, rx, window, cx, dispatch);
    view.update(cx, |this, _| this.harness = Some(link));
}

// ---------------------------------------------------------------------------
// Verbs
// ---------------------------------------------------------------------------

/// The most cells a `drag` verb walks through. A pointer crosses every cell on
/// its way, and so does the verb — but a drag across a thousand rows would run
/// a thousand `sheet_drag_over` calls for a selection its last one decides.
/// Beyond this the path is sampled; the ends are always kept, so the selection
/// is the same and only the cells swept in between are fewer.
const MAX_DRAG_STEPS: usize = 256;

// ---- argument parsing (pure) ----------------------------------------------

/// A required string argument.
pub fn arg_str<'a>(args: &'a Json, key: &str) -> Result<&'a str, String> {
    match args.get(key) {
        Some(Json::Str(s)) => Ok(s),
        Some(_) => Err(format!("'{key}' must be a string")),
        None => Err(format!("missing argument '{key}'")),
    }
}

/// A required non-negative integer argument.
pub fn arg_usize(args: &Json, key: &str) -> Result<usize, String> {
    match args.get(key) {
        Some(v) => v
            .as_usize()
            .ok_or_else(|| format!("'{key}' must be a whole number, not below zero")),
        None => Err(format!("missing argument '{key}'")),
    }
}

/// An optional boolean argument, defaulting to `false`.
pub fn arg_flag(args: &Json, key: &str) -> Result<bool, String> {
    match args.get(key) {
        None | Some(Json::Null) => Ok(false),
        Some(Json::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("'{key}' must be true or false")),
    }
}

/// One A1 cell reference. Deliberately strict: `A0`, `A1:B2` and `banana` are
/// all refusals rather than a silent clamp to a cell that exists, because a
/// test that drove the wrong cell would still pass.
pub fn parse_cell(text: &str) -> Result<(u32, u32), String> {
    parse_cell_name(text.trim())
        .ok_or_else(|| format!("'{text}' is not a cell reference (expected something like B3)"))
}

/// A required A1 cell argument.
pub fn cell_arg(args: &Json, key: &str) -> Result<(u32, u32), String> {
    parse_cell(arg_str(args, key)?)
}

/// The two ends of a drag: the cell it starts on and the cell it ends on.
pub type DragEnds = ((u32, u32), (u32, u32));

/// The two ends of a `drag`, given either as `from`/`to` cells or as one
/// `range`. Both spellings resolve to the same press-and-sweep.
pub fn drag_args(args: &Json) -> Result<DragEnds, String> {
    if let Some(v) = args.get("range") {
        let text = v
            .as_str()
            .ok_or_else(|| "'range' must be a string".to_string())?;
        let (r0, c0, r1, c1) = parse_range_name(text.trim())
            .ok_or_else(|| format!("'{text}' is not a range (expected something like A1:C5)"))?;
        return Ok(((r0, c0), (r1, c1)));
    }
    Ok((cell_arg(args, "from")?, cell_arg(args, "to")?))
}

/// The cells a pointer dragged from `from` to `to` would cross, in order,
/// starting with `from` itself — a real drag always moves inside its origin
/// cell before it crosses a boundary, and that first move is what plants the
/// selection.
///
/// The path is the straight line between the two, sampled once per cell of the
/// longer axis (and no more than [`MAX_DRAG_STEPS`] times), with consecutive
/// repeats collapsed. `to` is always the last cell.
pub fn drag_path(from: (u32, u32), to: (u32, u32)) -> Vec<(u32, u32)> {
    let (r0, c0) = (from.0 as i64, from.1 as i64);
    let (r1, c1) = (to.0 as i64, to.1 as i64);
    let span = (r1 - r0).abs().max((c1 - c0).abs()) as usize;
    let steps = span.min(MAX_DRAG_STEPS);
    let mut path = vec![from];
    for i in 1..=steps {
        let f = i as f64 / steps as f64;
        let r = r0 + ((r1 - r0) as f64 * f).round() as i64;
        let c = c0 + ((c1 - c0) as f64 * f).round() as i64;
        let cell = (r as u32, c as u32);
        if path.last() != Some(&cell) {
            path.push(cell);
        }
    }
    if path.last() != Some(&to) {
        path.push(to);
    }
    path
}

/// The keys a `key` verb accepts by name — gpui's own spelling, so a test says
/// what the platform would deliver. Single characters are keys too and are not
/// in this list.
const NAMED_KEYS: &[&str] = &[
    "enter",
    "escape",
    "tab",
    "backspace",
    "delete",
    "insert",
    "home",
    "end",
    "pageup",
    "pagedown",
    "up",
    "down",
    "left",
    "right",
    "space",
    "menu",
    "alt",
];

/// Parse a key spec — `enter`, `ctrl+c`, `shift+down`, `ctrl+shift+z`, `A` —
/// into the keystroke the platform would have delivered.
///
/// Shaped to match gpui's Windows key handling, because the app reads both
/// halves: `key` is the character printed on the key (a letter stays lower
/// case, with `shift` set beside it), and `key_char` is what would have been
/// typed — `None` under Ctrl/Alt/Win, since those produce control characters
/// the platform filters out, and `None` for the named keys for the same reason.
/// Getting this wrong would make `ctrl+c` type a "c" into a cell.
pub fn parse_key(spec: &str) -> Result<Keystroke, String> {
    let s = spec.trim();
    if s.is_empty() {
        return Err("expected a key like 'enter', 'ctrl+c' or 'shift+down'".to_string());
    }
    // A trailing '+' is the plus KEY, not an empty one ("+", "ctrl++").
    let (mods, key) = match s.strip_suffix('+') {
        Some(rest) => (rest, "+"),
        None => match s.rsplit_once('+') {
            Some((m, k)) => (m, k),
            None => ("", s),
        },
    };
    let mut modifiers = gpui::Modifiers::default();
    for m in mods.split('+').filter(|p| !p.is_empty()) {
        match m.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers.control = true,
            "shift" => modifiers.shift = true,
            "alt" | "option" => modifiers.alt = true,
            "cmd" | "win" | "super" | "meta" | "platform" => modifiers.platform = true,
            other => return Err(format!("unknown modifier '{other}' in key '{spec}'")),
        }
    }
    if key.is_empty() {
        return Err(format!("'{spec}' names modifiers but no key"));
    }
    let mut chars = key.chars();
    let (key, key_char) = match (chars.next(), chars.next()) {
        // A single character: the key itself. An upper-case letter is that key
        // WITH shift, which is how the platform reports it.
        (Some(ch), None) => {
            if ch.is_uppercase() {
                modifiers.shift = true;
            }
            (
                ch.to_lowercase().to_string(),
                Some(if modifiers.shift {
                    ch.to_uppercase().to_string()
                } else {
                    ch.to_string()
                }),
            )
        }
        _ => {
            let name = key.trim().to_ascii_lowercase();
            let known = NAMED_KEYS.contains(&name.as_str())
                || (name.starts_with('f')
                    && name[1..]
                        .parse::<u32>()
                        .is_ok_and(|n| (1..=24).contains(&n)));
            if !known {
                return Err(format!(
                    "unknown key '{key}' (a single character, or one of: {}, f1..f24)",
                    NAMED_KEYS.join(", ")
                ));
            }
            // Space is the one named key that types something.
            let ch = (name == "space").then(|| " ".to_string());
            (name, ch)
        }
    };
    // Ctrl/Alt/Win turn what would have been typed into a control character,
    // which the platform drops — so a chord carries no `key_char` at all.
    let typed = !(modifiers.control || modifiers.alt || modifiers.platform);
    Ok(Keystroke {
        modifiers,
        key,
        key_char: key_char.filter(|_| typed),
    })
}

/// The keystrokes that typing `text` would deliver, one per character.
pub fn typed_keys(text: &str) -> Result<Vec<Keystroke>, String> {
    if text.is_empty() {
        return Err("'text' is empty; there is nothing to type".to_string());
    }
    let mut out = Vec::new();
    for ch in text.chars() {
        let stroke = match ch {
            '\r' => continue, // a CRLF types one Enter, not two
            '\n' => Keystroke {
                modifiers: gpui::Modifiers::default(),
                key: "enter".into(),
                key_char: None,
            },
            '\t' => Keystroke {
                modifiers: gpui::Modifiers::default(),
                key: "tab".into(),
                key_char: None,
            },
            ' ' => Keystroke {
                modifiers: gpui::Modifiers::default(),
                key: "space".into(),
                key_char: Some(" ".into()),
            },
            c if c.is_control() => {
                return Err(format!(
                    "'text' contains the control character U+{:04X}; use the 'key' verb for those",
                    c as u32
                ));
            }
            c => Keystroke {
                modifiers: gpui::Modifiers {
                    shift: c.is_uppercase(),
                    ..Default::default()
                },
                key: c.to_lowercase().to_string(),
                key_char: Some(c.to_string()),
            },
        };
        out.push(stroke);
    }
    if out.is_empty() {
        return Err("'text' is empty; there is nothing to type".to_string());
    }
    Ok(out)
}

// ---- regions and their geometry (pure) ------------------------------------

/// A named piece of the window a test can ask for the rectangle of.
///
/// The app answers these because the layout is the only thing that knows where
/// they are. A harness that worked out "the grid starts 120px down" would be
/// asserting against its own copy of the layout: it would break on a ribbon
/// change, need correcting for DPI, and could not name `cell:A1` at all, since
/// where that lands depends on the scroll position, the row heights and the
/// frozen panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// The window's whole client area — everything a capture contains except
    /// the frame the OS draws.
    Window,
    /// The scrolling cell area of the grid: below the column header, above the
    /// sheet-tab row.
    Grid,
    /// The Chart panel down the right-hand side, when one is open.
    ChartPanel,
    /// One cell, or the box a range of them covers.
    Cells(u32, u32, u32, u32),
    /// A chart card on the sheet, by index — the same index `select-chart` uses.
    Chart(usize),
    /// Visible Project Gantt chart body, excluding its date header.
    Gantt,
    /// Visible part of a task bar, addressed by displayed task ID.
    Bar(i32),
    /// The Project table's horizontal scrollbar strip.
    ProjectHbarTable,
    /// The Project chart's horizontal scrollbar strip.
    ProjectHbarChart,
    /// The Project's vertical scrollbar strip, shared by table and chart.
    ProjectVbar,
    /// The Project Timeline pane above the Gantt view, when it is shown.
    ProjectTimeline,
}

/// Parse a region name: `window`, `grid`, `chart-panel`, `cell:B3`,
/// `cell:A1:C5`, `chart:0`, `gantt`, `bar:3`, `project-hbar-table`,
/// `project-hbar-chart`, `project-vbar`, `project-timeline`.
///
/// `cell:` takes a range as readily as a single cell, so an assertion about a
/// selection border names the selection rather than its two corners.
pub fn parse_region(name: &str) -> Result<Region, String> {
    let name = name.trim();
    let (head, arg) = match name.split_once(':') {
        Some((h, a)) => (h.trim(), Some(a.trim())),
        None => (name, None),
    };
    match head.to_ascii_lowercase().as_str() {
        "window" if arg.is_none() => Ok(Region::Window),
        "grid" if arg.is_none() => Ok(Region::Grid),
        "chart-panel" if arg.is_none() => Ok(Region::ChartPanel),
        "gantt" if arg.is_none() => Ok(Region::Gantt),
        "project-hbar-table" if arg.is_none() => Ok(Region::ProjectHbarTable),
        "project-hbar-chart" if arg.is_none() => Ok(Region::ProjectHbarChart),
        "project-vbar" if arg.is_none() => Ok(Region::ProjectVbar),
        "project-timeline" if arg.is_none() => Ok(Region::ProjectTimeline),
        "window" | "grid" | "chart-panel" | "gantt" | "project-hbar-table"
        | "project-hbar-chart" | "project-vbar" | "project-timeline" => {
            Err(format!("'{head}' does not take an argument; use '{head}'"))
        }
        "cell" | "cells" => {
            let a = arg.filter(|a| !a.is_empty()).ok_or_else(|| {
                format!("'{head}' needs a cell or a range, e.g. {head}:B3 or {head}:A1:C5")
            })?;
            if let Some((r0, c0, r1, c1)) = parse_range_name(a) {
                return Ok(Region::Cells(r0, c0, r1, c1));
            }
            let (r, c) = parse_cell(a)?;
            Ok(Region::Cells(r, c, r, c))
        }
        "bar" => {
            let a = arg
                .filter(|a| !a.is_empty())
                .ok_or("'bar' needs a task ID, e.g. bar:3")?;
            let id = a
                .parse::<i32>()
                .map_err(|_| format!("'{a}' is not a task ID"))?;
            Ok(Region::Bar(id))
        }
        "chart" => {
            let a = arg
                .filter(|a| !a.is_empty())
                .ok_or_else(|| "'chart' needs an index, e.g. chart:0".to_string())?;
            let i: usize = a
                .parse()
                .map_err(|_| format!("'{a}' is not a chart index (they count from 0)"))?;
            Ok(Region::Chart(i))
        }
        other => Err(format!(
            "unknown region '{other}' (window, grid, chart-panel, cell:B3, cell:A1:C5, chart:0, gantt, bar:3, project-hbar-table, project-hbar-chart, project-vbar, project-timeline)"
        )),
    }
}

/// The name a region reports itself under — [`parse_region`]'s inverse, so a
/// reply names the same thing the request did.
pub fn region_name(region: Region) -> String {
    match region {
        Region::Window => "window".into(),
        Region::Grid => "grid".into(),
        Region::ChartPanel => "chart-panel".into(),
        Region::Cells(r0, c0, r1, c1) => format!("cell:{}", a1_range((r0, c0, r1, c1))),
        Region::Chart(i) => format!("chart:{i}"),
        Region::Gantt => "gantt".into(),
        Region::Bar(id) => format!("bar:{id}"),
        Region::ProjectHbarTable => "project-hbar-table".into(),
        Region::ProjectHbarChart => "project-hbar-chart".into(),
        Region::ProjectVbar => "project-vbar".into(),
        Region::ProjectTimeline => "project-timeline".into(),
    }
}

/// A rectangle in physical screen pixels: what a harness crops a window capture
/// to. `x`/`y` are signed because a window on a monitor to the left of the
/// primary one has negative screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl ScreenRect {
    /// As the `rect` verb reports it.
    pub fn json(&self) -> Vec<(&'static str, Json)> {
        vec![
            ("x", Json::Num(self.x as f64)),
            ("y", Json::Num(self.y as f64)),
            ("w", Json::Num(self.w as f64)),
            ("h", Json::Num(self.h as f64)),
        ]
    }
}

/// Turn a window-relative logical rectangle into physical screen pixels.
///
/// `rect` is `(x, y, w, h)` from the client area's top-left; `origin` is where
/// that corner sits on the desktop, in the same logical units; `scale` is the
/// display's scale factor.
///
/// Both EDGES are rounded, rather than the origin being rounded and the size
/// scaled separately. Adjacent regions therefore keep sharing an edge in the
/// result exactly as they do on screen: rounding each size independently would
/// leave a one-pixel seam between two cells at some scroll positions and an
/// overlap at others, and a probe that samples a border would read the wrong
/// side of it.
pub fn screen_rect(rect: (f32, f32, f32, f32), origin: (f32, f32), scale: f32) -> ScreenRect {
    // A scale factor of zero or worse would collapse every rect onto a point;
    // 1.0 is the only sane fallback and matches an unscaled display.
    let s = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let (x, y, w, h) = rect;
    let left = ((origin.0 + x) * s).round();
    let top = ((origin.1 + y) * s).round();
    let right = ((origin.0 + x + w.max(0.0)) * s).round();
    let bottom = ((origin.1 + y + h.max(0.0)) * s).round();
    ScreenRect {
        x: left as i32,
        y: top as i32,
        w: (right - left).max(0.0) as u32,
        h: (bottom - top).max(0.0) as u32,
    }
}

/// The reference fields a test can name, and the target each one is.
pub fn parse_field(name: &str) -> Result<RefTarget, String> {
    let name = name.trim();
    let (head, index) = match name.split_once(':') {
        Some((h, i)) => {
            let n: usize = i.trim().parse().map_err(|_| {
                format!("'{name}': '{i}' is not a series number (they count from 0)")
            })?;
            (h.trim(), Some(n))
        }
        None => (name, None),
    };
    let indexed = |t: fn(usize) -> RefTarget| match index {
        Some(i) => Ok(t(i)),
        None => Err(format!("'{head}' needs a series number, e.g. {head}:0")),
    };
    match head.to_ascii_lowercase().as_str() {
        "chart-range" => Ok(RefTarget::ChartRange),
        "chart-title" => Ok(RefTarget::ChartTitle),
        "categories" => Ok(RefTarget::Categories),
        "series-name" => indexed(RefTarget::SeriesName),
        "series-values" => indexed(RefTarget::SeriesValues),
        "cond-format" => Ok(RefTarget::CondFormat),
        "validation" => Ok(RefTarget::Validation),
        "sort" => Ok(RefTarget::Sort),
        "text-to-columns" => Ok(RefTarget::TextToColumns),
        other => Err(format!(
            "unknown field '{other}' (chart-range, chart-title, categories, \
             series-name:N, series-values:N, cond-format, validation, sort, text-to-columns)"
        )),
    }
}

/// The name a field reports itself under — [`parse_field`]'s inverse, so a
/// reply can be fed straight back into the next verb.
pub fn field_name(target: RefTarget) -> String {
    match target {
        RefTarget::ChartRange => "chart-range".into(),
        RefTarget::ChartTitle => "chart-title".into(),
        RefTarget::Categories => "categories".into(),
        RefTarget::SeriesName(i) => format!("series-name:{i}"),
        RefTarget::SeriesValues(i) => format!("series-values:{i}"),
        RefTarget::CondFormat => "cond-format".into(),
        RefTarget::Validation => "validation".into(),
        RefTarget::Sort => "sort".into(),
        RefTarget::TextToColumns => "text-to-columns".into(),
    }
}

/// `A1` for a cell.
fn a1(cell: (u32, u32)) -> String {
    cell_name(cell.0, cell.1)
}

/// `A1:C5` for a range (`A1` when it is one cell).
fn a1_range((r0, c0, r1, c1): (u32, u32, u32, u32)) -> String {
    if (r0, c0) == (r1, c1) {
        a1((r0, c0))
    } else {
        format!("{}:{}", a1((r0, c0)), a1((r1, c1)))
    }
}

fn str_or_null(v: Option<String>) -> Json {
    v.map(Json::Str).unwrap_or(Json::Null)
}

fn num_or_null(v: Option<usize>) -> Json {
    v.map(|n| Json::Num(n as f64)).unwrap_or(Json::Null)
}

// ---- reading the app ------------------------------------------------------

/// Snapshot of view controls needed to serialize a document without a window.
#[derive(Clone, Copy)]
struct ViewFlags {
    hf_edit: bool,
    page: bool,
    marks: bool,
    ruler: bool,
    navigation: bool,
    comments: bool,
    notes: bool,
    zoom: f32,
    dark: bool,
}

impl ViewFlags {
    fn live(app: &crate::Docxy, window: &Window) -> Self {
        Self {
            hf_edit: app.hf_active(),
            page: app.page_view,
            marks: app.show_marks,
            ruler: app.show_ruler,
            navigation: app.show_nav,
            comments: app.show_comments,
            notes: app.show_notes,
            zoom: app.zoom,
            dark: app.theme_pref.resolve(window.appearance()) == gpui_component::ThemeMode::Dark,
        }
    }
}

fn signed(n: i32) -> Json {
    Json::Num(n as f64)
}
fn optional_signed(n: Option<i32>) -> Json {
    n.map(signed).unwrap_or(Json::Null)
}
fn optional_u32(n: Option<u32>) -> Json {
    n.map(|v| Json::Num(v as f64)).unwrap_or(Json::Null)
}
fn endpoint(position: &StoryOffset) -> Json {
    Json::obj(vec![
        ("story", Json::Str(position.story.clone())),
        ("offset", Json::Num(position.offset as f64)),
    ])
}

/// The document-only reply, shared by `state` and the `doc` verb.
fn doc_state(editor: &Editor, flags: &ViewFlags) -> Json {
    let flat = FlatDocument::new(&editor.doc);
    let caret = flat.locate(&editor.caret);
    let anchor = editor
        .anchor
        .as_ref()
        .and_then(|c| flat.locate(c))
        .or_else(|| caret.clone());
    let cross_story = matches!((&caret, &anchor), (Some(c), Some(a)) if c.story != a.story);
    let sel = match (&caret, &anchor) {
        (Some(c), Some(a)) if c.story == a.story => Json::obj(vec![
            ("story", Json::Str(c.story.clone())),
            ("start", Json::Num(c.offset.min(a.offset) as f64)),
            ("end", Json::Num(c.offset.max(a.offset) as f64)),
        ]),
        _ => Json::Null,
    };
    let p = editor.caret_para_props();
    let r = editor.caret_props();
    let alignment = match p.align {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
        Align::Justify => "justify",
    };
    let vert_align = match r.vert_align {
        VertAlign::Baseline => "baseline",
        VertAlign::Superscript => "superscript",
        VertAlign::Subscript => "subscript",
    };
    let para_index = caret.as_ref().and_then(|c| {
        flat.story(&c.story)
            .and_then(|s| s.paragraph_index(&editor.caret))
    });
    let para = Json::obj(vec![
        ("index", num_or_null(para_index)),
        ("style", str_or_null(p.style_id)),
        ("alignment", Json::Str(alignment.into())),
        (
            "ind",
            Json::obj(vec![
                ("left", signed(p.indent)),
                ("right", signed(p.indent_right)),
                ("first_line", signed(p.first_line)),
            ]),
        ),
        (
            "spacing",
            Json::obj(vec![
                ("before", optional_signed(p.spacing.before)),
                ("after", optional_signed(p.spacing.after)),
                ("line", optional_signed(p.spacing.line)),
                ("rule", str_or_null(p.spacing.line_rule)),
            ]),
        ),
        (
            "list",
            p.num_id
                .map(|id| Json::obj(vec![("num_id", signed(id)), ("level", signed(p.ilvl))]))
                .unwrap_or(Json::Null),
        ),
    ]);
    let run = Json::obj(vec![
        ("bold", Json::Bool(r.bold)),
        ("italic", Json::Bool(r.italic)),
        ("underline", Json::Bool(r.underline)),
        ("strike", Json::Bool(r.strike)),
        ("font", str_or_null(r.font)),
        ("size_half_pts", optional_u32(r.size_half_pts)),
        ("color", str_or_null(r.color)),
        ("highlight", str_or_null(r.highlight)),
        ("vert_align", Json::Str(vert_align.into())),
    ]);
    let view = Json::obj(vec![
        (
            "layout",
            Json::Str(if flags.page { "print" } else { "web" }.into()),
        ),
        ("marks", Json::Bool(flags.marks)),
        ("ruler", Json::Bool(flags.ruler)),
        ("navigation", Json::Bool(flags.navigation)),
        ("comments_pane", Json::Bool(flags.comments)),
        ("notes_pane", Json::Bool(flags.notes)),
        ("zoom", Json::Num(flags.zoom as f64)),
        (
            "theme",
            Json::Str(if flags.dark { "dark" } else { "light" }.into()),
        ),
    ]);
    Json::obj(vec![
        ("text", Json::Str(flat.main().text.clone())),
        (
            "textboxes",
            Json::Arr(
                flat.stories
                    .iter()
                    .skip(1)
                    .map(|s| {
                        Json::obj(vec![
                            ("story", Json::Str(s.id.clone())),
                            ("text", Json::Str(s.text.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("sel", sel),
        (
            "anchor",
            anchor.as_ref().map(endpoint).unwrap_or(Json::Null),
        ),
        ("caret", caret.as_ref().map(endpoint).unwrap_or(Json::Null)),
        ("cross_story", Json::Bool(cross_story)),
        ("hf_edit", Json::Bool(flags.hf_edit)),
        ("para", para),
        ("run", run),
        ("view", view),
    ])
}

fn active_doc(app: &crate::Docxy) -> Result<&Editor, String> {
    match app.tabs.get(app.active).map(|t| &t.surface) {
        Some(crate::Surface::Doc(ed)) => Ok(ed),
        _ => Err("the active tab is not a document".into()),
    }
}

/// Ribbon verbs address only surfaces that render this ribbon model.
fn ribbon_surface(app: &crate::Docxy) -> Result<(), String> {
    if app.backstage {
        return Err("the ribbon is hidden while File (backstage) is open".into());
    }
    if app.ribbon_min {
        return Err("the ribbon is collapsed".into());
    }
    match app.tabs.get(app.active).map(|t| &t.surface) {
        Some(crate::Surface::Doc(_) | crate::Surface::Project(_)) => Ok(()),
        _ => Err("the active tab has no document or Project ribbon".into()),
    }
}

/// One live status-line item in the order the app draws it.
fn status_json(status: &str) -> Json {
    Json::obj(vec![(
        "items",
        Json::Arr(vec![Json::obj(vec![("text", Json::Str(status.into()))])]),
    )])
}

/// Validate both main-story offsets before changing the editor's selection.
fn select_offsets(editor: &mut Editor, start: usize, end: usize) -> Result<(), String> {
    let flat = FlatDocument::new(&editor.doc);
    let limit = flat.main().len();
    let validate = |n| {
        flat.main().caret(n).ok_or_else(|| format!("offset {n} is not addressable in the main story (valid: 0..{}; final paragraph mark has no following caret)", limit.saturating_sub(1)))
    };
    let anchor = validate(start)?;
    let caret = validate(end)?;
    editor.clear_selection();
    editor.set_caret(anchor);
    editor.extend_selection(true);
    editor.set_caret(caret);
    Ok(())
}

/// Owned command data extracted from one ribbonspec control.
#[derive(Clone)]
struct RibbonCommand {
    id: String,
    label: String,
    tip_title: String,
    tip_body: String,
    key_tip: String,
    act: crate::Act,
    gallery: bool,
}

impl RibbonCommand {
    fn from_cmd(cmd: &crate::rs::Cmd<crate::Act>) -> Self {
        Self {
            id: cmd.id.into(),
            label: cmd.label.into(),
            tip_title: cmd.tip.title.into(),
            tip_body: cmd.tip.body.into(),
            key_tip: cmd.key_tip.into(),
            act: cmd.act,
            gallery: false,
        }
    }
    fn json(&self, checked: &impl Fn(&RibbonCommand) -> bool) -> Json {
        Json::obj(vec![
            ("id", Json::Str(self.id.clone())),
            ("label", Json::Str(self.label.clone())),
            (
                "tip",
                Json::obj(vec![
                    ("title", Json::Str(self.tip_title.clone())),
                    ("body", Json::Str(self.tip_body.clone())),
                ]),
            ),
            ("key_tip", Json::Str(self.key_tip.clone())),
            ("checked", Json::Bool(checked(self))),
        ])
    }
}

/// Flatten all command-bearing control shapes in visual order.
fn control_commands(control: &crate::Control<crate::Act>, out: &mut Vec<RibbonCommand>) {
    use crate::rs::{Cell, Control};
    match control {
        Control::Large(c) | Control::Toggle(c) => out.push(RibbonCommand::from_cmd(c)),
        Control::Column(cs) => out.extend(cs.iter().map(RibbonCommand::from_cmd)),
        Control::Split { primary, menu } => {
            out.push(RibbonCommand::from_cmd(primary));
            out.extend(menu.iter().map(RibbonCommand::from_cmd));
        }
        Control::Dropdown { cmd, items } => {
            out.push(RibbonCommand::from_cmd(cmd));
            out.extend(items.iter().map(RibbonCommand::from_cmd));
        }
        Control::Gallery(g) => out.extend(g.items.iter().map(|item| RibbonCommand {
            id: format!("{}:{}", g.id, item.label),
            label: item.label.into(),
            tip_title: g.tip.title.into(),
            tip_body: g.tip.body.into(),
            key_tip: String::new(),
            act: item.act,
            gallery: true,
        })),
        Control::Rows(rows) => {
            for cell in rows.iter().flatten() {
                match cell {
                    Cell::Btn(c) | Cell::Combo { cmd: c, .. } => {
                        out.push(RibbonCommand::from_cmd(c))
                    }
                }
            }
        }
        Control::Separator => {}
    }
}

/// Commands offered on one tab, used by click resolution.
fn tab_commands(tab: &crate::rs::Tab<crate::Act>) -> Vec<RibbonCommand> {
    let mut out = Vec::new();
    for group in &tab.groups {
        for control in &group.items {
            control_commands(control, &mut out);
        }
    }
    out
}

/// Groups and controls as the renderer presents one tab.
fn tab_json(tab: &crate::rs::Tab<crate::Act>, checked: &impl Fn(&RibbonCommand) -> bool) -> Json {
    let groups = tab
        .groups
        .iter()
        .map(|group| {
            let mut commands = Vec::new();
            let mut galleries = Vec::new();
            for control in &group.items {
                control_commands(control, &mut commands);
                if let crate::Control::Gallery(g) = control {
                    galleries.push(Json::obj(vec![
                        ("id", Json::Str(g.id.into())),
                        (
                            "items",
                            Json::Arr(g.items.iter().map(|i| Json::Str(i.label.into())).collect()),
                        ),
                    ]));
                }
            }
            Json::obj(vec![
                ("title", Json::Str(group.title.into())),
                ("launcher", Json::Bool(group.launcher.is_some())),
                (
                    "commands",
                    Json::Arr(commands.iter().map(|c| c.json(checked)).collect()),
                ),
                ("galleries", Json::Arr(galleries)),
            ])
        })
        .collect();
    Json::obj(vec![
        ("name", Json::Str(tab.name.into())),
        ("key_tip", Json::Str(tab.key_tip.into())),
        ("kind", Json::Str("ribbon".into())),
        ("groups", Json::Arr(groups)),
    ])
}

/// Pure ribbon snapshot for a tab kind and table context.
fn ribbon_json_for(
    kind: crate::Kind,
    in_table: bool,
    checked: impl Fn(&RibbonCommand) -> bool,
) -> Json {
    let ribbon = crate::ribbon_for(kind);
    let (_, file_name, file_tip) = crate::ribbon_tab_set(kind)[0];
    let mut tabs = vec![Json::obj(vec![
        ("name", Json::Str(file_name.into())),
        ("key_tip", Json::Str(file_tip.into())),
        ("kind", Json::Str("backstage".into())),
        ("groups", Json::Arr(Vec::new())),
    ])];
    tabs.extend(ribbon.tabs.iter().map(|t| tab_json(t, &checked)));
    if kind == crate::Kind::Docx && in_table {
        tabs.push(tab_json(&crate::table_tab(), &checked));
    }
    let tab_count = tabs.len();
    let qat = crate::QAT_ITEMS
        .iter()
        .map(|item| {
            Json::obj(vec![
                ("id", Json::Str(item.id.into())),
                ("label", Json::Str(item.label.into())),
                (
                    "tip",
                    Json::obj(vec![
                        ("title", Json::Str(item.tip.into())),
                        ("body", Json::Str(String::new())),
                    ]),
                ),
                ("key_tip", Json::Str(String::new())),
                ("checked", Json::Bool(false)),
            ])
        })
        .collect();
    Json::obj(vec![
        ("tabs", Json::Arr(tabs)),
        ("tab_count", Json::Num(tab_count as f64)),
        ("qat", Json::Arr(qat)),
    ])
}

/// Ribbon snapshot using the active tab and live checked states.
fn ribbon_json(app: &crate::Docxy) -> Json {
    ribbon_json_for(app.ribbon_kind(), app.caret_table().is_some(), |command| {
        if command.gallery {
            app.gallery_item_selected(command.act)
        } else {
            app.act_active(command.act)
        }
    })
}

/// Resolve a currently valid tab name before a synthetic ribbon click.
fn ribbon_tab_by_name(kind: crate::Kind, name: &str) -> Result<crate::RibbonTab, String> {
    crate::ribbon_tab_set(kind)
        .iter()
        .find_map(|(tab, label, _)| (*label == name).then_some(*tab).flatten())
        .or_else(|| {
            (kind == crate::Kind::Docx && name == "Table").then_some(crate::RibbonTab::Table)
        })
        .ok_or_else(|| format!("'{name}' is not a ribbon tab for the active document"))
}

/// Find a command on the actual active-kind ribbon definition.
fn resolve_ribbon_command(
    app: &crate::Docxy,
    tab_name: &str,
    query: &str,
) -> Result<crate::Act, String> {
    let kind = app.ribbon_kind();
    if tab_name == "File" {
        return Err("File is backstage; use the backstage verb".into());
    }
    ribbon_tab_by_name(kind, tab_name)?;
    let in_table = app.caret_table().is_some();
    let tab = if tab_name == "Table" {
        if !in_table {
            return Err("Table tab is not active outside a table".into());
        }
        crate::table_tab()
    } else {
        crate::ribbon_for(kind)
            .tabs
            .into_iter()
            .find(|t| t.name == tab_name)
            .ok_or_else(|| format!("'{tab_name}' is not a ribbon tab for the active document"))?
    };
    let commands = tab_commands(&tab);
    resolve_commands(&commands, tab_name, query)
}

/// Resolve by id first, then by label only when it is unique.
fn resolve_commands(
    commands: &[RibbonCommand],
    tab_name: &str,
    query: &str,
) -> Result<crate::Act, String> {
    let matches: Vec<_> = {
        let ids: Vec<_> = commands.iter().filter(|c| c.id == query).collect();
        if ids.is_empty() {
            commands.iter().filter(|c| c.label == query).collect()
        } else {
            ids
        }
    };
    match matches.as_slice() {
        [only] => Ok(only.act),
        [] => Err(format!("command '{query}' is not on tab '{tab_name}'")),
        many => Err(format!(
            "command '{query}' is ambiguous on tab '{tab_name}': {}",
            many.iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The active spreadsheet, or the refusal every cell verb needs.
fn sheet(app: &crate::Docxy) -> Result<&SheetView, String> {
    app.active_sheet()
        .ok_or_else(|| "the active tab is not a spreadsheet".to_string())
}

/// Whether a tab's status line says the load did not happen.
///
/// The loaders answer `loaded`, `loaded (markdown)` or `loaded — N sheets` when
/// they read the file, and `read error: …` / `load error: …` / `xlsx load
/// error: …` when they did not — and the failing branches hand back a real tab
/// either way (an empty document, or a placeholder surface). So "it starts with
/// `loaded`" is the whole test, and anything else is the app saying it did not
/// open the file.
fn load_failed(status: &str) -> bool {
    !status.starts_with("loaded")
}

/// The active tab's reason for not being the file that was asked for, if it is
/// not.
fn load_failure(app: &crate::Docxy) -> Option<String> {
    let t = app.tabs.get(app.active)?;
    load_failed(&t.status).then(|| t.status.to_string())
}

/// Everything a cell verb's caller might assert on, in one reply: what is
/// selected, what is being edited, which chart owns the selection, which field
/// has the keyboard, and the previews the grid would be drawing.
///
/// One shape for every driving verb, so a test reads the same keys whichever
/// one it just sent. The sheet half is absent when the active tab is not a
/// spreadsheet (`type` and `key` reach the document surface too).
fn state(app: &crate::Docxy, window: &Window) -> Json {
    let mut out = vec![
        ("tab", Json::Num(app.active as f64)),
        ("tabs", Json::Num(app.tabs.len() as f64)),
        ("ask_on_close", Json::Bool(app.ask_on_close)),
        (
            "ribbon_tab",
            Json::Str(
                crate::ribbon_tab_name(crate::valid_ribbon_tab(
                    app.ribbon_kind(),
                    app.ribbon_tab,
                    app.caret_table().is_some(),
                ))
                .into(),
            ),
        ),
        (
            "title",
            Json::Str(
                app.tabs
                    .get(app.active)
                    .map(|t| t.title.to_string())
                    .unwrap_or_default(),
            ),
        ),
        (
            "dirty",
            Json::Bool(app.tabs.get(app.active).is_some_and(|t| t.dirty)),
        ),
        // What the tab's status line says. Reported because a refusal a modal
        // dialog would otherwise have made is written here (that is what the
        // `harness.is_none()` gates leave behind), and because a document that
        // failed to load is still a tab with a title — the status is the only
        // place the failure shows.
        (
            "status",
            Json::Str(
                app.tabs
                    .get(app.active)
                    .map(|t| t.status.to_string())
                    .unwrap_or_default(),
            ),
        ),
        ("sheet_tab", Json::Bool(app.active_is_sheet())),
    ];
    if let Some(v) = app.active_sheet() {
        out.extend([
            ("sheet", Json::Str(v.sheet().name.clone())),
            ("sel", Json::Str(a1(v.sel))),
            ("anchor", Json::Str(a1(v.anchor))),
            ("range", Json::Str(a1_range(v.range()))),
            ("editing", Json::Bool(v.editing.is_some())),
            ("edit", str_or_null(v.editing.clone())),
        ]);
    }
    let ov = app.grid_overlay();
    out.extend([
        ("chart_sel", num_or_null(app.chart_sel)),
        ("panel_chart", num_or_null(app.panel_chart_shown())),
        ("charts", Json::Num(app.chart_count() as f64)),
        (
            "field",
            str_or_null(app.range_edit.as_ref().map(|f| field_name(f.target))),
        ),
        (
            "field_text",
            str_or_null(app.range_edit.as_ref().map(|f| f.buf.clone())),
        ),
        // The two the auto-fill regression is about: whether a sweep armed the
        // fill handle, and the box it would write.
        ("filling", Json::Bool(app.sheet_fill.is_some())),
        ("fill_preview", str_or_null(ov.fill_preview.map(a1_range))),
        ("dragging", Json::Bool(app.sheet_dragging)),
        // Point mode: the grid is picking cells into a field, which is what
        // makes the selection border dashed rather than solid.
        ("picking", Json::Bool(ov.picking)),
        ("range_preview", str_or_null(ov.range_preview.map(a1_range))),
        ("sel_hidden", Json::Bool(ov.sel_hidden)),
    ]);
    let mut out: Vec<_> = out.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    if let Ok(ed) = active_doc(app) {
        if let Json::Obj(fields) = doc_state(ed, &ViewFlags::live(app, window)) {
            out.extend(fields);
        }
    }
    if let Some(crate::Surface::Project(v)) = app.tabs.get(app.active).map(|t| &t.surface) {
        let body_h = app
            .probes
            .borrow()
            .get("project-body")
            .map(|b| f32::from(b.size.height));
        out.extend(crate::project_state(v, body_h));
    }
    Json::Obj(out)
}

// ---- the verb table -------------------------------------------------------

/// Route one harness verb against the live app.
///
/// Every driving verb goes through the same entry point the pointer or the
/// keyboard would: `click-cell` is the cell's own click handler, `drag` is a
/// press plus one move per cell crossed plus the release, ordinary `key`/`type`
/// input uses [`crate::Docxy::on_key`], action-bound Tab variants use their
/// `tab_key`/`shift_tab_key` handlers, and `select-chart` is the press on a chart
/// card.
/// `selection-set` is setup: it uses the editor's caret API after validating
/// both offsets because the UI has no pointer-by-offset operation.
/// A verb that reached past those into the state they maintain could pass while
/// the handler under test was broken — the one way this harness could be worse
/// than nothing.
pub fn dispatch(
    app: &mut crate::Docxy,
    verb: &str,
    args: &Json,
    window: &mut Window,
    cx: &mut Context<crate::Docxy>,
) -> Result<Done, String> {
    match verb {
        "doc" => Done::ok(doc_state(active_doc(app)?, &ViewFlags::live(app, window))),
        "selection-set" => {
            let (start, end) = (arg_usize(args, "start")?, arg_usize(args, "end")?);
            if app.hf_active() {
                return Err("selection-set cannot address the body while a header or footer is being edited".into());
            }
            let ed = app
                .active_editor()
                .ok_or("the active tab is not a document")?;
            select_offsets(ed, start, end)?;
            app.refocus(window, cx);
            Done::ok(state(app, window))
        }
        "status-read" => {
            let tab = app.tabs.get(app.active).ok_or("there is no active tab")?;
            Done::ok(status_json(&tab.status))
        }
        "backstage" => {
            match arg_str(args, "action")? {
                "open" => app.open_backstage(cx),
                "close" => app.backstage_back(window, cx),
                "read" => {}
                _ => return Err("'action' must be open, close or read".into()),
            }
            let items = crate::backstage_rail_items(app.active_is_project());
            Done::ok(Json::obj(vec![
                ("open", Json::Bool(app.backstage)),
                (
                    "items",
                    Json::Arr(items.map(|item| Json::Str(item.display.into())).collect()),
                ),
            ]))
        }
        "ribbon-read" => {
            ribbon_surface(app)?;
            Done::ok(ribbon_json(app))
        }
        "ribbon-click" => {
            ribbon_surface(app)?;
            let tab = arg_str(args, "tab")?.to_string();
            let command = arg_str(args, "command")?.to_string();
            let act = resolve_ribbon_command(app, &tab, &command)?;
            app.select_ribbon_tab(ribbon_tab_by_name(app.ribbon_kind(), &tab)?, window, cx);
            app.dispatch(act, window, cx);
            Done::ok(state(app, window))
        }
        "close-tab" => {
            let index = match args.get("index") {
                Some(_) => arg_usize(args, "index")?,
                None => app.active,
            };
            if index >= app.tabs.len() {
                return Err(format!("no tab {index}"));
            }
            let answer = match args.get("answer") {
                None => None,
                Some(Json::Str(value)) => Some(match value.as_str() {
                    "save" => crate::close::CloseAnswer::Save,
                    "discard" => crate::close::CloseAnswer::Discard,
                    "cancel" => crate::close::CloseAnswer::Cancel,
                    _ => return Err("'answer' must be save, discard or cancel".into()),
                }),
                Some(_) => return Err("'answer' must be save, discard or cancel".into()),
            };
            app.close_tab_with(index, answer, window, cx);
            Done::ok(state(app, window))
        }
        // The same handler as the Backstage rail item, not a synthetic click.
        "backstage-close" => {
            app.backstage_close(window, cx);
            Done::ok(state(app, window))
        }
        "ask-on-close" => {
            let Some(Json::Bool(on)) = args.get("on") else {
                return Err("'on' must be a boolean".into());
            };
            app.set_ask_on_close(*on, cx);
            Done::ok(state(app, window))
        }
        "ping" => Done::ok(Json::obj(vec![
            ("instance", Json::Str(instance_name())),
            ("pid", Json::Num(std::process::id() as f64)),
            (
                "config_root",
                Json::Str(crate::config_root().display().to_string()),
            ),
            ("tabs", Json::Num(app.tabs.len() as f64)),
        ])),

        // Open a file, exactly as a path on the command line does.
        "open" => {
            let raw = arg_str(args, "path")?;
            let path = PathBuf::from(raw);
            if !path.is_file() {
                return Err(format!("no such file: {raw}"));
            }
            app.open_args(vec![path], cx);
            // ⚠️ A load that failed still produces a tab. `doc_from_path`
            // substitutes an empty document and records the reason in the
            // tab's status, so the title is still the fixture's file name and
            // nothing else in the reply tells the two apart — the step would
            // be green and every assertion after it would be about a document
            // that was never read. The status is the app's own word for how
            // the load went, so it is what decides.
            match load_failure(app) {
                Some(why) => Err(format!("{raw}: {why}")),
                None => Done::ok(state(app, window)),
            }
        }

        // A click on a cell: press, click, release — the three events the
        // pointer delivers, in that order.
        "click-cell" => {
            let cell = cell_arg(args, "cell")?;
            let (shift, dbl) = (arg_flag(args, "shift")?, arg_flag(args, "double")?);
            if app.active_is_project() {
                let tab = &mut app.tabs[app.active];
                let crate::Surface::Project(v) = &tab.surface else {
                    return Err("Project is not loaded".into());
                };
                if cell.0 as usize >= v.ed.project().tasks.len() || cell.1 >= 7 {
                    return Err("Project cell is outside the entry table".into());
                }
                crate::project_cell_click(tab, cell.0 as usize, Some(cell.1 as usize), dbl);
                app.refocus(window, cx);
                return Done::ok(state(app, window));
            }
            sheet(app)?;
            app.grid_press_cell(cell, cx);
            app.cell_click(cell.0, cell.1, shift, dbl, window, cx);
            app.grid_release(cx);
            Done::ok(state(app, window))
        }

        // A drag: the press plants the anchor, each cell crossed is a move, the
        // release commits whatever the moves armed.
        "drag" => {
            let (from, to) = drag_args(args)?;
            sheet(app)?;
            app.grid_press_cell(from, cx);
            for (r, c) in drag_path(from, to) {
                app.grid_drag_over(r, c, cx);
            }
            app.grid_release(cx);
            Done::ok(state(app, window))
        }

        // Type text, one key event per character.
        "type" => {
            for stroke in typed_keys(arg_str(args, "text")?)? {
                press(app, stroke, window, cx);
            }
            Done::ok(state(app, window))
        }

        // One key, or a list of them ("keys": ["ctrl+c", "down", "ctrl+v"]).
        "key" => {
            let specs: Vec<String> = match args.get("keys") {
                Some(Json::Arr(items)) => items
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .ok_or_else(|| "'keys' must be an array of strings".to_string())
                    })
                    .collect::<Result<_, _>>()?,
                Some(_) => return Err("'keys' must be an array of strings".to_string()),
                None => vec![arg_str(args, "key")?.to_string()],
            };
            if specs.is_empty() {
                return Err("'keys' is empty; there is nothing to press".to_string());
            }
            // Parse them all before pressing any, so a typo in the third key
            // does not leave the app half way through the sequence.
            let strokes = specs
                .iter()
                .map(|s| parse_key(s))
                .collect::<Result<Vec<_>, _>>()?;
            for stroke in strokes {
                press(app, stroke, window, cx);
            }
            Done::ok(state(app, window))
        }

        // Select a chart, as pressing its card does (press then release, with
        // no travel in between — the release ends the move the press armed).
        "select-chart" => {
            let idx = arg_usize(args, "index")?;
            sheet(app)?;
            let n = app.chart_count();
            if idx >= n {
                return Err(match n {
                    0 => "this sheet has no charts".to_string(),
                    _ => format!("no chart {idx}; this sheet has {n} (0..{})", n - 1),
                });
            }
            app.chart_press(idx, (0, 0), (0.0, 0.0), cx);
            app.grid_release(cx);
            Done::ok(state(app, window))
        }

        // Give a reference field the keyboard, as clicking it does.
        "focus-field" => {
            let target = parse_field(arg_str(args, "field")?)?;
            if target.is_bar() && app.bar_field != Some(target) {
                return Err(format!(
                    "'{}' belongs to an entry bar that is not open; \
                     open it from the ribbon first",
                    field_name(target)
                ));
            }
            let seed = app.ref_field_seed(target).ok_or_else(|| {
                format!(
                    "'{}' is not on screen (no chart is in the panel, or it has no such series)",
                    field_name(target)
                )
            })?;
            app.ref_field_focus(target, seed, cx);
            Done::ok(state(app, window))
        }

        // Read one cell: what it shows, and what re-editing it would put in the
        // editor (the formula, or the unformatted literal).
        "cell" => {
            let (r, c) = cell_arg(args, "cell")?;
            if let Some(crate::Surface::Project(v)) = app.tabs.get(app.active).map(|t| &t.surface) {
                let task =
                    v.ed.project()
                        .tasks
                        .get(r as usize)
                        .ok_or("No task at this row")?;
                let values = crate::project_row(&v.ed, task);
                let text = values
                    .get(c as usize)
                    .ok_or("No Project column at this index")?;
                return Done::ok(Json::obj(vec![
                    ("cell", Json::Str(a1((r, c)))),
                    ("row", Json::Num(r as f64)),
                    ("col", Json::Num(c as f64)),
                    ("text", Json::Str(text.clone())),
                    ("value", Json::Str(text.clone())),
                    ("empty", Json::Bool(text.is_empty())),
                ]));
            }
            let v = sheet(app)?;
            let raw = v.edit_string(r, c);
            Done::ok(Json::obj(vec![
                ("cell", Json::Str(a1((r, c)))),
                ("row", Json::Num(r as f64)),
                ("col", Json::Num(c as f64)),
                ("text", Json::Str(v.cell_text(r, c))),
                ("value", Json::Str(raw.clone())),
                ("empty", Json::Bool(raw.is_empty())),
            ]))
        }

        // How many frames the app has drawn, and a request for one more.
        //
        // The primitive a driver waits on before it measures or photographs
        // anything: a verb only marks the view dirty, so when its reply goes
        // out the frame that shows what it did has not been laid out, and the
        // probe geometry `rect` answers from is a frame older still. Asking
        // for a region straight after a verb would report — or refuse — from
        // before the change. See `Driver::settle`.
        "frame" => {
            cx.notify();
            Done::ok(Json::obj(vec![("frame", Json::Num(app.frame as f64))]))
        }

        // Where a named region is, on the desktop, in physical pixels — so the
        // harness can crop a window capture to it. The app answers because the
        // layout is the only thing that knows; see [`Region`].
        //
        // `frame` comes back with it, and it is not decoration: a verb only
        // marks the view dirty, so the frame that SHOWS what it did has not
        // been laid out when the reply goes out. A driver reads `frame`, sends
        // its verbs, then waits for `rect` to report a higher one before
        // capturing — otherwise it would measure and photograph the frame
        // before the change.
        "rect" => {
            let region = parse_region(arg_str(args, "region")?)?;
            let b = app.region_bounds(region, window)?;
            let win = window.bounds();
            let r = screen_rect(
                (
                    f32::from(b.origin.x),
                    f32::from(b.origin.y),
                    f32::from(b.size.width),
                    f32::from(b.size.height),
                ),
                (f32::from(win.origin.x), f32::from(win.origin.y)),
                window.scale_factor(),
            );
            // Ask for a frame, so a driver that only ever calls `rect` still
            // makes progress: an idle app draws nothing, and the count it is
            // waiting on would never move.
            cx.notify();
            let mut out = vec![("region", Json::Str(region_name(region)))];
            out.extend(r.json());
            out.extend([
                ("scale", Json::Num(window.scale_factor() as f64)),
                ("frame", Json::Num(app.frame as f64)),
            ]);
            Done::ok(Json::obj(out))
        }

        // State assertions include tab metadata for every surface; spreadsheet
        // selection fields are added by state() only when a sheet is active.
        "selection" => Done::ok(state(app, window)),

        // Persist and go. The reply is written first (see the pump).
        "quit" => {
            crate::close::commit_pending_for_exit(&mut app.tabs);
            app.persist();
            Ok(Done {
                result: Json::obj(vec![("quitting", Json::Bool(true))]),
                quit: true,
            })
        }

        other => match app.dispatch_project(other, args, window, cx) {
            Some(result) => result.and_then(Done::ok),
            None => Err(format!("unknown verb '{other}'")),
        },
    }
}

/// A keystroke as the platform would hand it to the window.
fn key_event(keystroke: Keystroke) -> KeyDownEvent {
    KeyDownEvent {
        keystroke,
        is_held: false,
        prefer_character_input: false,
    }
}

/// The keys the app binds to actions rather than reading in `on_key`, and the
/// modifiers each binding carries.
///
/// gpui reserves Tab and Shift-Tab for focus traversal: it matches key
/// BINDINGS before it delivers a key-down event, so these two never reach
/// `on_key_down` and the app binds them as actions instead (the `cx.bind_keys`
/// call in `main`). A harness that pushed them into
/// `on_key` anyway would exercise a path the platform never takes — on a sheet
/// it is a silent no-op, where a real Tab commits the edit and advances a
/// cell. That is the "verb bypasses the handler under test" failure this
/// harness exists to prevent, so [`press`] re-routes them.
///
/// ⚠️ This must list exactly what `cx.bind_keys` registers; `action_bound_keys_match_the_bindings`
/// fails if the two drift.
const ACTION_KEYS: &[(&str, bool)] = &[("tab", false), ("tab", true)];

/// Whether `stroke` is one of [`ACTION_KEYS`] — i.e. gpui would dispatch it as
/// an action instead of delivering it to `on_key`.
fn is_action_key(stroke: &Keystroke) -> bool {
    let m = &stroke.modifiers;
    !m.control
        && !m.alt
        && !m.platform
        && ACTION_KEYS
            .iter()
            .any(|(key, shift)| *key == stroke.key && *shift == m.shift)
}

/// Deliver one keystroke the way the platform would: through the bound action
/// if gpui would match a binding first, otherwise through `on_key`.
fn press(
    app: &mut crate::Docxy,
    stroke: Keystroke,
    window: &mut Window,
    cx: &mut Context<crate::Docxy>,
) {
    if is_action_key(&stroke) {
        // The only bindings are Tab and Shift-Tab; `is_action_key` has already
        // ruled out every other modifier combination.
        if stroke.modifiers.shift {
            app.shift_tab_key(window, cx);
        } else {
            app.tab_key(window, cx);
        }
        return;
    }
    app.on_key(&key_event(stroke), window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    use docxcore::model::{Block, Document, ParProps, Paragraph, Run, RunProps, Spacing};

    fn editor(props: ParProps, run: RunProps) -> Editor {
        Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph {
                props,
                content: vec![docxcore::model::Inline::Run(Run {
                    text: "abc".into(),
                    props: run,
                })],
            })],
        })
    }

    #[test]
    fn theme_reply_resolves_preference_without_waiting_for_render() {
        use gpui::WindowAppearance;
        use gpui_component::ThemeMode;
        assert_eq!(
            crate::ThemePref::Dark.resolve(WindowAppearance::Light),
            ThemeMode::Dark
        );
        assert_eq!(
            crate::ThemePref::Light.resolve(WindowAppearance::Dark),
            ThemeMode::Light
        );
        assert_eq!(
            crate::ThemePref::Auto.resolve(WindowAppearance::Dark),
            ThemeMode::Dark
        );
        assert_eq!(
            crate::ThemePref::Auto.resolve(WindowAppearance::Light),
            ThemeMode::Light
        );
    }

    #[test]
    fn document_fields_read_editor_and_view_values() {
        let props = ParProps {
            style_id: Some("Heading2".into()),
            align: Align::Center,
            indent: 720,
            indent_right: 240,
            first_line: -120,
            num_id: Some(17),
            ilvl: 2,
            spacing: Spacing {
                before: Some(100),
                after: Some(200),
                line: Some(360),
                line_rule: Some("auto".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let run = RunProps {
            bold: true,
            italic: true,
            underline: true,
            strike: true,
            font: Some("Aptos".into()),
            size_half_pts: Some(26),
            color: Some("AABBCC".into()),
            highlight: Some("yellow".into()),
            vert_align: VertAlign::Superscript,
            ..Default::default()
        };
        let flags = ViewFlags {
            hf_edit: false,
            page: false,
            marks: true,
            ruler: true,
            navigation: true,
            comments: true,
            notes: true,
            zoom: 1.5,
            dark: true,
        };
        let state = doc_state(&editor(props, run), &flags);
        assert_eq!(state.get_str("text"), Some("abc\n"));
        assert_eq!(state.get("hf_edit"), Some(&Json::Bool(false)));
        let para = state.get("para").unwrap();
        assert_eq!(para.get_usize("index"), Some(0));
        assert_eq!(para.get_str("style"), Some("Heading2"));
        assert_eq!(para.get_str("alignment"), Some("center"));
        assert_eq!(
            para.get("ind").unwrap().get("left"),
            Some(&Json::Num(720.0))
        );
        assert_eq!(
            para.get("ind").unwrap().get("right"),
            Some(&Json::Num(240.0))
        );
        assert_eq!(
            para.get("ind").unwrap().get("first_line"),
            Some(&Json::Num(-120.0))
        );
        assert_eq!(
            para.get("spacing").unwrap().get("before"),
            Some(&Json::Num(100.0))
        );
        assert_eq!(
            para.get("spacing").unwrap().get("after"),
            Some(&Json::Num(200.0))
        );
        assert_eq!(
            para.get("spacing").unwrap().get("line"),
            Some(&Json::Num(360.0))
        );
        assert_eq!(para.get("spacing").unwrap().get_str("rule"), Some("auto"));
        assert_eq!(
            para.get("list").unwrap().get("num_id"),
            Some(&Json::Num(17.0))
        );
        assert_eq!(
            para.get("list").unwrap().get("level"),
            Some(&Json::Num(2.0))
        );
        let run = state.get("run").unwrap();
        for name in ["bold", "italic", "underline", "strike"] {
            assert_eq!(run.get(name), Some(&Json::Bool(true)), "{name}");
        }
        assert_eq!(run.get_str("font"), Some("Aptos"));
        assert_eq!(run.get("size_half_pts"), Some(&Json::Num(26.0)));
        assert_eq!(run.get_str("color"), Some("AABBCC"));
        assert_eq!(run.get_str("highlight"), Some("yellow"));
        assert_eq!(run.get_str("vert_align"), Some("superscript"));
        let view = state.get("view").unwrap();
        assert_eq!(view.get_str("layout"), Some("web"));
        for name in [
            "marks",
            "ruler",
            "navigation",
            "comments_pane",
            "notes_pane",
        ] {
            assert_eq!(view.get(name), Some(&Json::Bool(true)), "{name}");
        }
        assert_eq!(view.get("zoom"), Some(&Json::Num(1.5)));
        assert_eq!(view.get_str("theme"), Some("dark"));

        let defaults = doc_state(
            &editor(ParProps::default(), RunProps::default()),
            &ViewFlags {
                hf_edit: false,
                page: true,
                marks: false,
                ruler: false,
                navigation: false,
                comments: false,
                notes: false,
                zoom: 1.0,
                dark: false,
            },
        );
        assert_ne!(state.get("para"), defaults.get("para"));
        assert_ne!(state.get("run"), defaults.get("run"));
        assert_ne!(state.get("view"), defaults.get("view"));
        let at = |value: &Json, path: &[&str]| -> Json {
            path.iter()
                .fold(value, |v, key| v.get(key).unwrap())
                .clone()
        };
        for path in [
            &["para", "style"][..],
            &["para", "alignment"],
            &["para", "ind", "left"],
            &["para", "ind", "right"],
            &["para", "ind", "first_line"],
            &["para", "spacing", "before"],
            &["para", "spacing", "after"],
            &["para", "spacing", "line"],
            &["para", "spacing", "rule"],
            &["para", "list"],
            &["run", "bold"],
            &["run", "italic"],
            &["run", "underline"],
            &["run", "strike"],
            &["run", "font"],
            &["run", "size_half_pts"],
            &["run", "color"],
            &["run", "highlight"],
            &["run", "vert_align"],
            &["view", "layout"],
            &["view", "marks"],
            &["view", "ruler"],
            &["view", "navigation"],
            &["view", "comments_pane"],
            &["view", "notes_pane"],
            &["view", "zoom"],
            &["view", "theme"],
        ] {
            assert_ne!(
                at(&state, path),
                at(&defaults, path),
                "{path:?} must reflect live values"
            );
        }
    }

    #[test]
    fn selection_set_validates_before_mutating_and_preserves_direction() {
        let mut ed = editor(ParProps::default(), RunProps::default());
        select_offsets(&mut ed, 3, 1).unwrap();
        assert_eq!(ed.anchor.as_ref().unwrap().offset, 3);
        assert_eq!(ed.caret.offset, 1);
        let before = (ed.anchor.clone(), ed.caret.clone());
        assert!(select_offsets(&mut ed, 1, 4).is_err());
        assert_eq!((ed.anchor, ed.caret), before);
    }

    #[test]
    fn document_state_preserves_cross_story_endpoints() {
        let mut ed = Editor::new(Document {
            body: vec![Block::Paragraph(Paragraph {
                props: ParProps::default(),
                content: vec![
                    docxcore::model::Inline::Run(Run {
                        text: "host".into(),
                        props: RunProps::default(),
                    }),
                    docxcore::model::Inline::TextBox {
                        raw: String::new(),
                        blocks: vec![Block::Paragraph(Paragraph {
                            props: ParProps::default(),
                            content: vec![docxcore::model::Inline::Run(Run {
                                text: "box".into(),
                                props: RunProps::default(),
                            })],
                        })],
                    },
                ],
            })],
        });
        ed.set_caret(docxcore::editor::Caret::top(0, 4));
        ed.extend_selection(true);
        ed.move_right();
        let state = doc_state(
            &ed,
            &ViewFlags {
                hf_edit: false,
                page: true,
                marks: false,
                ruler: false,
                navigation: false,
                comments: false,
                notes: false,
                zoom: 1.0,
                dark: false,
            },
        );
        assert_eq!(state.get("cross_story"), Some(&Json::Bool(true)));
        assert_eq!(state.get("sel"), Some(&Json::Null));
        assert_eq!(state.get("anchor").unwrap().get_str("story"), Some("main"));
        assert_eq!(state.get("anchor").unwrap().get_usize("offset"), Some(4));
        assert_eq!(
            state.get("caret").unwrap().get_str("story"),
            Some("textbox:0/1")
        );
        assert_eq!(state.get("caret").unwrap().get_usize("offset"), Some(0));
        assert_eq!(state.get_str("text"), Some("host\n"));
        assert_eq!(
            state.get("textboxes").unwrap().as_array().unwrap()[0].get_str("text"),
            Some("box\n")
        );
    }

    #[test]
    fn ribbon_reflects_definition_and_checked_state() {
        let off = ribbon_json_for(crate::Kind::Docx, false, |_| false);
        let on = ribbon_json_for(crate::Kind::Docx, true, |c| {
            matches!(c.act, crate::Act::Bold)
        });
        let tabs = off.get("tabs").unwrap().as_array().unwrap();
        assert_eq!(tabs[0].get_str("name"), Some("File"));
        assert_eq!(tabs[0].get_str("kind"), Some("backstage"));
        assert_eq!(
            tabs.iter()
                .map(|t| t.get_str("name").unwrap())
                .collect::<Vec<_>>(),
            vec!["File", "Home", "Insert", "Review", "View"]
        );
        let tabs_on = on.get("tabs").unwrap().as_array().unwrap();
        assert_eq!(tabs_on.last().unwrap().get_str("name"), Some("Table"));
        let groups = tabs_on[1].get("groups").unwrap().as_array().unwrap();
        let commands: Vec<&Json> = groups
            .iter()
            .flat_map(|g| g.get("commands").unwrap().as_array().unwrap())
            .collect();
        let bold = commands
            .iter()
            .find(|c| c.get_str("id") == Some("b"))
            .unwrap();
        assert_eq!(bold.get_str("label"), Some("Bold"));
        assert_eq!(bold.get("checked"), Some(&Json::Bool(true)));
        assert!(bold.get("tip").unwrap().get_str("title").is_some());
        assert!(bold.get("key_tip").is_some());
        assert!(!on.get("qat").unwrap().as_array().unwrap().is_empty());
        assert!(
            groups
                .iter()
                .any(|g| !g.get("galleries").unwrap().as_array().unwrap().is_empty())
        );
    }

    #[test]
    fn style_gallery_checked_matches_heading_preview() {
        let mut props = ParProps::default();
        props.style_id = Some("Heading1".into());
        let editor = editor(props, RunProps::default());
        let ribbon = ribbon_json_for(crate::Kind::Docx, false, |c| {
            c.gallery && crate::gallery_style_selected(&editor.caret_para_props(), c.act)
        });
        let tabs = ribbon.get("tabs").unwrap().as_array().unwrap();
        let commands: Vec<&Json> = tabs[1]
            .get("groups")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g.get("commands").unwrap().as_array().unwrap())
            .collect();
        let checked = |label| {
            commands
                .iter()
                .find(|c| c.get_str("label") == Some(label))
                .unwrap()
                .get("checked")
        };
        assert_eq!(checked("Heading 1"), Some(&Json::Bool(true)));
        assert_eq!(checked("Normal"), Some(&Json::Bool(false)));
    }

    #[test]
    fn normal_paragraph_selects_only_normal_gallery_item() {
        let mut editor = editor(ParProps::default(), RunProps::default());
        let ribbon = ribbon_json_for(crate::Kind::Docx, false, |c| {
            c.gallery && crate::gallery_style_selected(&editor.caret_para_props(), c.act)
        });
        let commands = ribbon.get("tabs").unwrap().as_array().unwrap()[1]
            .get("groups")
            .unwrap()
            .as_array()
            .unwrap()[3]
            .get("commands")
            .unwrap()
            .as_array()
            .unwrap();
        let selected: Vec<&str> = commands
            .iter()
            .filter(|c| c.get("checked") == Some(&Json::Bool(true)))
            .filter_map(|c| c.get_str("label"))
            .collect();
        assert_eq!(selected, ["Normal"]);
        editor.set_space_before(Some(0));
        editor.set_space_after(Some(0));
        editor.set_line_spacing(240, "auto");
        assert!(crate::gallery_style_selected(
            &editor.caret_para_props(),
            crate::Act::NoSpacing
        ));
        assert!(!crate::gallery_style_selected(
            &editor.caret_para_props(),
            crate::Act::Normal
        ));
    }

    #[test]
    fn ribbon_resolver_rejects_ambiguous_labels_and_status_is_live_text() {
        let commands = vec![
            RibbonCommand {
                id: "one".into(),
                label: "Same".into(),
                tip_title: String::new(),
                tip_body: String::new(),
                key_tip: String::new(),
                act: crate::Act::Bold,
                gallery: false,
            },
            RibbonCommand {
                id: "two".into(),
                label: "Same".into(),
                tip_title: String::new(),
                tip_body: String::new(),
                key_tip: String::new(),
                act: crate::Act::Italic,
                gallery: false,
            },
        ];
        let err = resolve_commands(&commands, "Home", "Same").err().unwrap();
        assert!(err.contains("one, two"));
        assert!(resolve_commands(&commands, "Home", "one").is_ok());
        assert_eq!(
            status_json("saved")
                .get("items")
                .unwrap()
                .as_array()
                .unwrap()[0]
                .get_str("text"),
            Some("saved")
        );
        assert_eq!(
            status_json("changed")
                .get("items")
                .unwrap()
                .as_array()
                .unwrap()[0]
                .get_str("text"),
            Some("changed")
        );
    }

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    // ---- flag parsing ----

    /// The ordinary launch: files only, and no harness.
    #[test]
    fn plain_files_do_not_enable_the_harness() {
        let cli = parse_args(args(&[r"C:\docs\a.docx", "b.xlsx"]));
        assert!(!cli.harness);
        assert_eq!(
            cli.files,
            vec![PathBuf::from(r"C:\docs\a.docx"), PathBuf::from("b.xlsx")]
        );
        assert!(cli.unknown_flags.is_empty());
    }

    /// The flag is recognized wherever it appears, and never counted as a file.
    #[test]
    fn harness_flag_is_recognized_and_is_not_a_file() {
        let cli = parse_args(args(&["a.xlsx", HARNESS_FLAG, "b.xlsx"]));
        assert!(cli.harness);
        assert_eq!(
            cli.files,
            vec![PathBuf::from("a.xlsx"), PathBuf::from("b.xlsx")]
        );
    }

    /// No arguments at all is the double-click case: nothing on, nothing to open.
    #[test]
    fn empty_command_line_is_a_plain_launch() {
        assert_eq!(parse_args(args(&[])), Cli::default());
    }

    /// A near-miss must be reported, not swallowed. If `--harnes` were treated
    /// as a file it would vanish (no such file) and the app would come up
    /// looking normal while the test waited forever for a socket.
    #[test]
    fn unknown_flags_are_reported_rather_than_treated_as_files() {
        let cli = parse_args(args(&["--harnes", "--verbose"]));
        assert!(!cli.harness);
        assert!(cli.files.is_empty());
        assert_eq!(cli.unknown_flags, vec!["--harnes", "--verbose"]);
    }

    /// After `--`, a file that happens to look like our flag is still a file.
    #[test]
    fn double_dash_ends_flag_parsing() {
        let cli = parse_args(args(&["--", HARNESS_FLAG]));
        assert!(!cli.harness);
        assert_eq!(cli.files, vec![PathBuf::from(HARNESS_FLAG)]);
    }

    // ---- the env alternative ----

    #[test]
    fn env_flag_reads_on_and_off_values() {
        assert!(!env_flag(None));
        assert!(!env_flag(Some(OsString::from(""))));
        assert!(!env_flag(Some(OsString::from("0"))));
        assert!(!env_flag(Some(OsString::from("false"))));
        assert!(!env_flag(Some(OsString::from(" OFF "))));
        assert!(env_flag(Some(OsString::from("1"))));
        assert!(env_flag(Some(OsString::from("true"))));
        assert!(env_flag(Some(OsString::from("yes"))));
    }

    // ---- the isolation gate ----

    /// The happy path: an override that is somewhere else entirely.
    #[test]
    fn gate_accepts_a_sandbox_root() {
        let os = PathBuf::from(r"C:\Users\someone\AppData\Roaming");
        let root = gate(Some(OsStr::new(r"D:\runs\harness-42")), Some(&os))
            .expect("a sandbox root is accepted");
        assert_eq!(root, PathBuf::from(r"D:\runs\harness-42"));
    }

    /// No override: refuse, and say why. This is the case the whole gate exists
    /// for — starting here would write into the user's real profile.
    #[test]
    fn gate_refuses_without_an_override() {
        let err = gate(None, Some(Path::new(r"C:\Users\someone\AppData\Roaming")))
            .expect_err("no override must be refused");
        assert!(
            err.contains(CONFIG_DIR_ENV),
            "message names the variable: {err}"
        );
    }

    /// An exported-but-blank variable is a shell accident, and `config_root`
    /// already treats it as unset — so the gate must refuse it too, rather than
    /// approve a sandbox that is really the config directory.
    #[test]
    fn gate_refuses_a_blank_override() {
        let err = gate(
            Some(OsStr::new("")),
            Some(Path::new(r"C:\Users\someone\AppData\Roaming")),
        )
        .expect_err("a blank override must be refused");
        assert!(err.contains(CONFIG_DIR_ENV));
    }

    /// Pointing the override at the real config directory is the mistyped
    /// invocation that would drive the user's own instance. Refuse it, and
    /// refuse the trailing-separator and wrong-case spellings of it too.
    #[test]
    fn gate_refuses_the_real_config_directory() {
        let os = PathBuf::from(r"C:\Users\someone\AppData\Roaming");
        for spelling in [
            r"C:\Users\someone\AppData\Roaming",
            r"C:\Users\someone\AppData\Roaming\",
            r"C:/Users/someone/AppData/Roaming",
        ] {
            let err = gate(Some(OsStr::new(spelling)), Some(&os))
                .expect_err(&format!("{spelling} must be refused"));
            assert!(
                err.contains("real config directory"),
                "message says what is wrong: {err}"
            );
        }
    }

    /// With no OS config directory to compare against (an odd profile), an
    /// override is still enough — there is nothing it could collide with.
    #[test]
    fn gate_accepts_when_the_os_config_dir_is_unknown() {
        assert_eq!(
            gate(Some(OsStr::new("target/harness-cfg")), None),
            Ok(PathBuf::from("target/harness-cfg"))
        );
    }

    #[test]
    fn same_dir_ignores_separator_style_trailing_slash_and_case() {
        assert!(same_dir(
            Path::new(r"C:\Users\a\AppData\Roaming"),
            Path::new(r"C:\Users\a\AppData\Roaming\")
        ));
        assert!(same_dir(
            Path::new(r"C:\Users\a\AppData\Roaming"),
            Path::new("C:/Users/a/AppData/Roaming")
        ));
        assert!(!same_dir(
            Path::new(r"C:\Users\a\AppData\Roaming"),
            Path::new(r"C:\Users\a\AppData\Roaming2")
        ));
        if cfg!(windows) {
            assert!(same_dir(
                Path::new(r"C:\Users\a\AppData\Roaming"),
                Path::new(r"c:\users\a\appdata\roaming")
            ));
        }
    }

    /// A directory that is certainly there, for the tests that need to
    /// canonicalize something real.
    ///
    /// ⚠️ Not `std::env::temp_dir()`, which reads TMP/TEMP. Reading the
    /// environment here would race `session_and_hot_both_follow_the_override`
    /// in main.rs — same test binary, and cargo runs tests on several threads
    /// — and a getenv concurrent with that test's setenv is the undefined
    /// behaviour those calls are `unsafe` for. `CARGO_MANIFEST_DIR` is
    /// substituted at compile time, so nothing is read at run time at all.
    fn an_existing_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// Both the lexical and filesystem-aware comparisons see through a spelling
    /// that walks out and back in.
    #[test]
    fn resolves_same_sees_through_a_dot_dot_spelling() {
        let real = an_existing_dir();
        let Some(leaf) = real.file_name().map(std::ffi::OsString::from) else {
            return; // a root directory; there is nothing to walk out of
        };
        let round_trip = real.join("..").join(leaf);
        assert!(same_dir(&round_trip, &real), "dot-dot is normalized");
        assert!(resolves_same(&round_trip, &real));
    }

    #[test]
    fn gate_refuses_a_nonexistent_component_followed_by_dot_dot() {
        let real = an_existing_dir();
        let alias = real.join("no-such-harness-dir-9f3a").join("..");
        let err = gate(Some(alias.as_os_str()), Some(&real))
            .expect_err("the unresolved spelling still names the real config root");
        assert!(err.contains("real config directory"), "{err}");
    }

    #[test]
    fn nearest_ancestor_resolution_handles_a_junction_target_with_a_missing_tail() {
        let real = PathBuf::from("real-config");
        let alias = PathBuf::from("run")
            .join("config-link")
            .join("missing")
            .join("..");
        // Model the contract supplied by `canonicalize`: the junction itself
        // resolves to the real directory, while descendants below the missing
        // component do not exist. Injecting it keeps this regression test
        // deterministic on Windows machines where creating symlinks needs an
        // elevated token or Developer Mode.
        let mut canonicalize = |path: &Path| {
            if path == Path::new("run").join("config-link") || path == real {
                Ok(real.clone())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "synthetic missing path",
                ))
            }
        };
        assert!(
            resolves_same_with(&alias, &real, &mut canonicalize),
            "a missing tail beneath a junction still resolves to the real root"
        );
    }

    /// Distinct unresolved tails must remain unequal even though their nearest
    /// existing ancestors can be resolved.
    #[test]
    fn resolves_same_keeps_distinct_unresolved_tails_unequal() {
        let real = an_existing_dir();
        assert!(!resolves_same(
            &real.join("no-such-harness-dir-9f3a"),
            &real
        ));
        assert!(!resolves_same(
            Path::new("/no-such-harness-dir-9f3a"),
            Path::new("/no-such-harness-dir-9f3b")
        ));
    }

    // ---- discovery placement ----

    /// The discovery file must sit under the sandbox, not wherever
    /// `ctlcore::config_ctl_dir` would put it — that reads APPDATA and could
    /// name a different sandbox from the one holding session.json.
    #[test]
    fn control_dir_is_under_the_sandbox_root() {
        let root = Path::new(r"D:\runs\harness-42");
        let dir = control_dir(root);
        assert_eq!(dir, root.join("suite").join("ctl"));
        assert!(dir.starts_with(root));
    }

    // ---- verb arguments (pure) ----

    fn obj(pairs: &[(&str, Json)]) -> Json {
        Json::Obj(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    }

    fn s(v: &str) -> Json {
        Json::Str(v.to_string())
    }

    /// A cell reference names one cell, in any of the spellings a user types.
    #[test]
    fn parse_cell_takes_the_spellings_a1_notation_has() {
        assert_eq!(parse_cell("A1"), Ok((0, 0)));
        assert_eq!(parse_cell("b3"), Ok((2, 1)));
        assert_eq!(parse_cell(" $C$4 "), Ok((3, 2)));
        assert_eq!(parse_cell("AA100"), Ok((99, 26)));
    }

    /// A cell that does not exist is a refusal, not a clamp: a test driven at
    /// the wrong cell would still pass, which is worse than not running at all.
    #[test]
    fn parse_cell_refuses_what_is_not_a_cell() {
        for bad in ["A0", "0", "banana", "", "A1:B2", "1A", "XFE1", "A1048577"] {
            let err = parse_cell(bad).expect_err(&format!("{bad:?} must be refused"));
            assert!(
                err.contains("not a cell reference"),
                "message says what is wrong: {err}"
            );
        }
    }

    /// The `open` verb's whole guard against a green step about a document
    /// that never loaded. The loaders' success and failure wordings are the
    /// contract, so they are pinned here rather than left to a reader to
    /// notice: `Loaded::empty` hands back a real, editable tab titled with the
    /// file's own name, and the status is the only place the failure shows.
    #[test]
    fn a_status_that_does_not_start_with_loaded_is_a_failed_open() {
        for ok in [
            "loaded",
            "loaded (markdown)",
            "loaded \u{2014} 1 sheet",
            "loaded \u{2014} 3 sheets",
        ] {
            assert!(!load_failed(ok), "{ok}");
        }
        for bad in [
            "read error: The system cannot find the file specified. (os error 2)",
            "load error: NotZip",
            "xlsx load error: NotZip",
            "new",
        ] {
            assert!(load_failed(bad), "{bad}");
        }
    }

    #[test]
    fn cell_arg_reports_a_missing_or_mistyped_argument() {
        let args = obj(&[("cell", s("B2"))]);
        assert_eq!(cell_arg(&args, "cell"), Ok((1, 1)));
        assert_eq!(
            cell_arg(&Json::Null, "cell"),
            Err("missing argument 'cell'".to_string())
        );
        assert_eq!(
            cell_arg(&obj(&[("cell", Json::Num(3.0))]), "cell"),
            Err("'cell' must be a string".to_string())
        );
    }

    #[test]
    fn arg_usize_and_flag_report_their_own_shapes() {
        let args = obj(&[
            ("index", Json::Num(2.0)),
            ("shift", Json::Bool(true)),
            ("bad", Json::Num(-1.0)),
        ]);
        assert_eq!(arg_usize(&args, "index"), Ok(2));
        assert!(arg_usize(&args, "bad").is_err());
        assert!(arg_usize(&args, "nope").is_err());
        assert_eq!(arg_flag(&args, "shift"), Ok(true));
        assert_eq!(arg_flag(&args, "absent"), Ok(false)); // an omitted flag is off
        assert!(arg_flag(&obj(&[("shift", s("yes"))]), "shift").is_err());
    }

    /// Both spellings of a drag reach the same pair of cells.
    #[test]
    fn drag_args_accepts_from_to_and_a_range() {
        assert_eq!(
            drag_args(&obj(&[("from", s("A1")), ("to", s("C5"))])),
            Ok(((0, 0), (4, 2)))
        );
        assert_eq!(
            drag_args(&obj(&[("range", s("A1:C5"))])),
            Ok(((0, 0), (4, 2)))
        );
        // A one-cell range is a drag that goes nowhere, not an error.
        assert_eq!(drag_args(&obj(&[("range", s("B2"))])), Ok(((1, 1), (1, 1))));
    }

    /// A malformed range, and a half-given pair.
    #[test]
    fn drag_args_refuses_a_malformed_range() {
        for bad in ["A1:", ":C5", "A1:C0", "A1-C5", "everything"] {
            let err = drag_args(&obj(&[("range", s(bad))]))
                .expect_err(&format!("{bad:?} must be refused"));
            assert!(err.contains("is not a range"), "{err}");
        }
        assert_eq!(
            drag_args(&obj(&[("range", Json::Num(1.0))])),
            Err("'range' must be a string".to_string())
        );
        // `to` without `from`, and a `from` that is not a cell.
        assert_eq!(
            drag_args(&obj(&[("to", s("C5"))])),
            Err("missing argument 'from'".to_string())
        );
        assert!(drag_args(&obj(&[("from", s("A0")), ("to", s("C5"))])).is_err());
    }

    // ---- the drag path ----

    /// A drag that never leaves its cell still delivers the one move that
    /// plants the selection.
    #[test]
    fn drag_path_of_a_still_drag_is_its_own_cell() {
        assert_eq!(drag_path((2, 2), (2, 2)), vec![(2, 2)]);
    }

    /// Straight drags cross every cell on the way, starting where they began.
    #[test]
    fn drag_path_crosses_every_cell_in_a_straight_run() {
        assert_eq!(
            drag_path((0, 0), (0, 3)),
            vec![(0, 0), (0, 1), (0, 2), (0, 3)]
        );
        assert_eq!(
            drag_path((5, 1), (2, 1)),
            vec![(5, 1), (4, 1), (3, 1), (2, 1)]
        );
    }

    /// A diagonal moves both ways at once and lands exactly on its target.
    #[test]
    fn drag_path_runs_the_diagonal_to_its_end() {
        let path = drag_path((0, 0), (4, 2));
        assert_eq!(path.first(), Some(&(0, 0)));
        assert_eq!(path.last(), Some(&(4, 2)));
        assert_eq!(path.len(), 5); // one step per row, the longer axis
        // Monotone in both axes: a pointer never doubles back.
        for w in path.windows(2) {
            assert!(w[1].0 >= w[0].0 && w[1].1 >= w[0].1, "{path:?}");
        }
    }

    /// A drag down a thousand rows is sampled rather than walked cell by cell,
    /// but still starts and ends where it was told to.
    #[test]
    fn drag_path_is_capped_but_keeps_both_ends() {
        let path = drag_path((0, 0), (5000, 0));
        assert_eq!(path.first(), Some(&(0, 0)));
        assert_eq!(path.last(), Some(&(5000, 0)));
        assert!(path.len() <= MAX_DRAG_STEPS + 1, "{} cells", path.len());
    }

    // ---- keys ----

    fn stroke(spec: &str) -> Keystroke {
        parse_key(spec).unwrap_or_else(|e| panic!("{spec}: {e}"))
    }

    /// A named key carries no character: the platform filters the control
    /// character it would have produced, and the grid's handlers rely on that.
    #[test]
    fn parse_key_reads_the_named_keys() {
        let k = stroke("enter");
        assert_eq!(k.key, "enter");
        assert_eq!(k.key_char, None);
        assert!(!k.modifiers.modified());
        assert_eq!(stroke("Escape").key, "escape"); // case is not significant
        assert_eq!(stroke("f2").key, "f2");
        assert_eq!(stroke("pagedown").key, "pagedown");
        assert_eq!(stroke("alt").key, "alt");
        assert_eq!(stroke("alt").key_char, None);
        // Space is the named key that does type something.
        assert_eq!(stroke("space").key_char.as_deref(), Some(" "));
    }

    #[test]
    fn parse_key_reads_modifiers() {
        let k = stroke("ctrl+c");
        assert!(k.modifiers.control && !k.modifiers.shift);
        assert_eq!(k.key, "c");
        // A chord types nothing — or ctrl+c would put a "c" in the cell.
        assert_eq!(k.key_char, None);

        let k = stroke("shift+down");
        assert!(k.modifiers.shift);
        assert_eq!(k.key, "down");

        let k = stroke("ctrl+shift+z");
        assert!(k.modifiers.control && k.modifiers.shift);
        assert_eq!(k.key, "z");

        assert!(stroke("cmd+s").modifiers.platform);
        assert!(stroke("alt+f4").modifiers.alt);
    }

    /// An upper-case letter is the same key with shift held, which is how the
    /// platform reports it — `key` stays lower case, `key_char` is the capital.
    #[test]
    fn parse_key_reads_a_capital_as_shift_plus_the_key() {
        let k = stroke("A");
        assert_eq!(k.key, "a");
        assert!(k.modifiers.shift);
        assert_eq!(k.key_char.as_deref(), Some("A"));

        let k = stroke("a");
        assert_eq!(k.key_char.as_deref(), Some("a"));
        assert!(!k.modifiers.shift);
    }

    // ---- action-bound keys ----

    /// The list the harness re-routes must be exactly what `cx.bind_keys`
    /// registers. If a third binding is ever added and this list is not
    /// updated, the harness would push that key into `on_key` — where the app,
    /// by construction, does not read it — and every case using it would
    /// silently assert nothing. Reading main.rs's source is the only way to
    /// check a `cx.bind_keys` call from a test, and it is worth it here.
    #[test]
    fn action_bound_keys_match_the_bindings() {
        let src = include_str!("main.rs");
        let (_, after) = src
            .split_once("cx.bind_keys([")
            .expect("main.rs binds keys exactly once");
        let (block, _) = after.split_once("]);").expect("the bind_keys call closes");
        let bound: Vec<String> = block
            .lines()
            .filter_map(|l| l.split_once("KeyBinding::new(\""))
            .filter_map(|(_, rest)| rest.split_once('"'))
            .map(|(name, _)| name.to_ascii_lowercase())
            .collect();
        assert!(!bound.is_empty(), "found no bindings to compare against");

        let routed: Vec<String> = ACTION_KEYS
            .iter()
            .map(|(key, shift)| {
                if *shift {
                    format!("shift-{key}")
                } else {
                    key.to_string()
                }
            })
            .collect();
        assert_eq!(
            bound, routed,
            "the keys main.rs binds as actions and the keys `press` re-routes have drifted"
        );
        // `split_once` reads the FIRST call; a second one elsewhere would bind
        // keys this test never sees. main.rs is the only file that binds any.
        assert_eq!(
            src.matches("cx.bind_keys([").count(),
            1,
            "main.rs binds keys in more than one place; this test reads only the first"
        );
    }

    /// The routing decision itself: Tab and Shift-Tab go to the action, and a
    /// chord on the same key does not (gpui matches no binding for it, so the
    /// platform would deliver it as an ordinary key-down).
    #[test]
    fn only_the_bare_tab_chords_route_to_an_action() {
        assert!(is_action_key(&stroke("tab")));
        assert!(is_action_key(&stroke("shift+tab")));

        assert!(!is_action_key(&stroke("ctrl+tab")));
        assert!(!is_action_key(&stroke("ctrl+shift+tab")));
        assert!(!is_action_key(&stroke("alt+tab")));
        assert!(!is_action_key(&stroke("enter")));
        assert!(!is_action_key(&stroke("a")));
    }

    /// Typing a literal tab is the same press, so it must route the same way —
    /// `typed_keys` produces the very keystroke `parse_key("tab")` does.
    #[test]
    fn a_typed_tab_routes_to_the_action_too() {
        let keys = typed_keys("	").expect("a tab is typable");
        assert_eq!(keys.len(), 1);
        assert!(is_action_key(&keys[0]));
    }

    /// The separator is also a key.
    #[test]
    fn parse_key_reads_a_trailing_plus_as_the_plus_key() {
        assert_eq!(stroke("+").key, "+");
        let k = stroke("ctrl++");
        assert_eq!(k.key, "+");
        assert!(k.modifiers.control);
    }

    #[test]
    fn parse_key_refuses_what_it_cannot_press() {
        assert!(parse_key("").is_err());
        assert!(parse_key("   ").is_err());
        let err = parse_key("hyper+a").expect_err("unknown modifier");
        assert!(err.contains("unknown modifier"), "{err}");
        let err = parse_key("banana").expect_err("unknown key");
        assert!(err.contains("unknown key"), "{err}");
        assert!(parse_key("f25").is_err()); // there is no F25
    }

    /// Typing is one keystroke per character, with capitals shifted.
    #[test]
    fn typed_keys_delivers_one_keystroke_per_character() {
        let keys = typed_keys("Hi!").expect("typable");
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0].key, "h");
        assert!(keys[0].modifiers.shift);
        assert_eq!(keys[0].key_char.as_deref(), Some("H"));
        assert_eq!(keys[1].key_char.as_deref(), Some("i"));
        assert_eq!(keys[2].key, "!");
        assert_eq!(keys[2].key_char.as_deref(), Some("!"));
    }

    /// The whitespace a script is likely to hold.
    #[test]
    fn typed_keys_maps_whitespace_to_its_keys() {
        let keys = typed_keys("a b\tc\r\n").expect("typable");
        let names: Vec<&str> = keys.iter().map(|k| k.key.as_str()).collect();
        // The \r of a CRLF is dropped: it types one Enter, not two.
        assert_eq!(names, ["a", "space", "b", "tab", "c", "enter"]);
        assert_eq!(keys[5].key_char, None);
    }

    #[test]
    fn typed_keys_refuses_empty_text_and_control_characters() {
        assert!(typed_keys("").is_err());
        let err = typed_keys("a\u{7}b").expect_err("a bell is not typing");
        assert!(err.contains("control character"), "{err}");
    }

    // ---- fields ----

    #[test]
    fn parse_field_names_every_reference_field() {
        assert_eq!(parse_field("chart-range"), Ok(RefTarget::ChartRange));
        assert_eq!(parse_field("Chart-Title"), Ok(RefTarget::ChartTitle));
        assert_eq!(parse_field("categories"), Ok(RefTarget::Categories));
        assert_eq!(parse_field("series-name:0"), Ok(RefTarget::SeriesName(0)));
        assert_eq!(
            parse_field(" series-values:2 "),
            Ok(RefTarget::SeriesValues(2))
        );
        assert_eq!(parse_field("sort"), Ok(RefTarget::Sort));
    }

    #[test]
    fn parse_field_refuses_unknown_and_unnumbered_fields() {
        let err = parse_field("chart-colour").expect_err("no such field");
        assert!(err.contains("unknown field"), "{err}");
        let err = parse_field("series-name").expect_err("which series?");
        assert!(err.contains("series number"), "{err}");
        let err = parse_field("series-values:last").expect_err("not a number");
        assert!(err.contains("not a series number"), "{err}");
    }

    /// Every name a reply can print is a name the next verb accepts.
    #[test]
    fn field_names_round_trip() {
        for t in [
            RefTarget::ChartRange,
            RefTarget::ChartTitle,
            RefTarget::Categories,
            RefTarget::SeriesName(0),
            RefTarget::SeriesValues(3),
            RefTarget::CondFormat,
            RefTarget::Validation,
            RefTarget::Sort,
            RefTarget::TextToColumns,
        ] {
            assert_eq!(parse_field(&field_name(t)), Ok(t));
        }
    }

    // ---- reply shapes ----

    #[test]
    fn a1_names_cells_and_ranges_the_way_a_test_writes_them() {
        assert_eq!(a1((0, 0)), "A1");
        assert_eq!(a1_range((0, 0, 4, 2)), "A1:C5");
        // A one-cell range prints as the cell, not as "B2:B2".
        assert_eq!(a1_range((1, 1, 1, 1)), "B2");
    }

    // ---- region names ----

    #[test]
    fn the_plain_regions_parse() {
        assert_eq!(parse_region("window"), Ok(Region::Window));
        assert_eq!(parse_region("grid"), Ok(Region::Grid));
        assert_eq!(parse_region("chart-panel"), Ok(Region::ChartPanel));
        // Case and surrounding space are noise, as everywhere else here.
        assert_eq!(parse_region("  Chart-Panel "), Ok(Region::ChartPanel));
    }

    #[test]
    fn a_cell_region_takes_one_cell_or_a_range() {
        assert_eq!(parse_region("cell:B3"), Ok(Region::Cells(2, 1, 2, 1)));
        assert_eq!(parse_region("cell:A1:C5"), Ok(Region::Cells(0, 0, 4, 2)));
        // `cells:` reads better for a range and means the same thing.
        assert_eq!(parse_region("cells:A1:C5"), Ok(Region::Cells(0, 0, 4, 2)));
        assert_eq!(parse_region("cell:b3"), Ok(Region::Cells(2, 1, 2, 1)));
    }

    #[test]
    fn a_chart_region_takes_an_index() {
        assert_eq!(parse_region("chart:0"), Ok(Region::Chart(0)));
        assert_eq!(parse_region("chart:12"), Ok(Region::Chart(12)));
    }

    /// An unknown region must name itself in the refusal and list the ones
    /// there are. A test that mistyped `chart_panel` would otherwise get a
    /// rectangle-shaped silence and no idea why.
    #[test]
    fn an_unknown_region_is_refused_by_name() {
        let e = parse_region("chart_panel").unwrap_err();
        assert!(e.contains("chart_panel"), "{e}");
        assert!(e.contains("chart-panel"), "{e}");
        assert!(parse_region("").is_err());
    }

    #[test]
    fn a_region_argument_that_names_nothing_is_refused() {
        // A prefix with nothing after it.
        let e = parse_region("cell:").unwrap_err();
        assert!(e.contains("cell:B3"), "{e}");
        assert!(parse_region("cell").unwrap_err().contains("needs a cell"));
        // Not a cell, and not a range either.
        assert!(parse_region("cell:banana").is_err());
        // Rows count from 1, so there is no row 0 — the same strictness
        // `parse_cell` has, for the same reason.
        assert!(parse_region("cell:A0").is_err());
        assert!(parse_region("cell:A1:").is_err());
        // A chart index has to be one.
        let e = parse_region("chart:last").unwrap_err();
        assert!(e.contains("last"), "{e}");
        assert!(parse_region("chart:-1").is_err());
        assert!(parse_region("chart").unwrap_err().contains("chart:0"));
        // Regions with no argument never accept a suffix. Interactive commands
        // use this parser directly, so they must be as strict as scripts.
        for name in ["window:1", "grid:typo", "chart-panel:"] {
            let e = parse_region(name).expect_err("an argument-less region rejects a suffix");
            assert!(e.contains("does not take an argument"), "{name}: {e}");
        }
    }

    /// Every name a reply prints is a name the next request accepts.
    #[test]
    fn region_names_round_trip() {
        for r in [
            Region::Window,
            Region::Grid,
            Region::ChartPanel,
            Region::Cells(2, 1, 2, 1),
            Region::Cells(0, 0, 4, 2),
            Region::Chart(3),
            Region::Gantt,
            Region::Bar(3),
            Region::ProjectHbarTable,
            Region::ProjectHbarChart,
            Region::ProjectVbar,
            Region::ProjectTimeline,
        ] {
            assert_eq!(parse_region(&region_name(r)), Ok(r));
        }
        // A one-cell region prints as the cell, not as a degenerate range.
        assert_eq!(region_name(Region::Cells(2, 1, 2, 1)), "cell:B3");
        assert_eq!(region_name(Region::Cells(0, 0, 4, 2)), "cell:A1:C5");
        assert!(parse_region("gantt:1").is_err());
        assert!(parse_region("project-vbar:1").is_err());
        assert!(parse_region("project-timeline:1").is_err());
        assert!(parse_region("bar:abc").is_err());
        assert!(parse_region("bar:").is_err());
    }

    // ---- logical rect -> physical screen pixels ----

    #[test]
    fn an_unscaled_rect_is_offset_by_the_window_origin() {
        let r = screen_rect((10.0, 20.0, 100.0, 50.0), (300.0, 200.0), 1.0);
        assert_eq!(
            r,
            ScreenRect {
                x: 310,
                y: 220,
                w: 100,
                h: 50
            }
        );
    }

    #[test]
    fn a_scaled_rect_scales_both_the_offset_and_the_size() {
        let r = screen_rect((10.0, 20.0, 100.0, 50.0), (300.0, 200.0), 2.0);
        assert_eq!(
            r,
            ScreenRect {
                x: 620,
                y: 440,
                w: 200,
                h: 100
            }
        );
    }

    /// Adjacent regions must still be adjacent after conversion. Rounding the
    /// origin and scaling the size separately leaves a seam at some scroll
    /// positions and an overlap at others, and a border probe would then sample
    /// the wrong side of the line.
    #[test]
    fn adjacent_rects_stay_adjacent_at_a_fractional_scale() {
        let scale = 1.5;
        let mut x = 0.0f32;
        let mut prev_right: Option<i32> = None;
        // Widths that do not land on whole physical pixels at 1.5x.
        for w in [37.0f32, 51.0, 40.0, 63.0, 19.0] {
            let r = screen_rect((x, 0.0, w, 10.0), (0.5, 0.25), scale);
            if let Some(p) = prev_right {
                assert_eq!(r.x, p, "a gap or an overlap opened at x={x}");
            }
            prev_right = Some(r.x + r.w as i32);
            x += w;
        }
    }

    /// A window on a monitor left of the primary one has a negative origin, and
    /// the rect has to survive it rather than wrapping through zero.
    #[test]
    fn a_negative_window_origin_is_kept() {
        let r = screen_rect((10.0, 10.0, 20.0, 20.0), (-1920.0, -100.0), 1.0);
        assert_eq!(r.x, -1910);
        assert_eq!(r.y, -90);
        assert_eq!((r.w, r.h), (20, 20));
    }

    /// A zero-size region is a legitimate answer (a collapsed panel); a
    /// negative one is not, and must not wrap round to an enormous width.
    #[test]
    fn degenerate_sizes_clamp_to_zero() {
        let r = screen_rect((10.0, 10.0, 0.0, 0.0), (0.0, 0.0), 1.0);
        assert_eq!((r.w, r.h), (0, 0));
        let r = screen_rect((10.0, 10.0, -50.0, -50.0), (0.0, 0.0), 1.0);
        assert_eq!((r.w, r.h), (0, 0));
    }

    /// A scale factor of zero would collapse every region onto a point, and a
    /// harness would crop nothing at all rather than fail loudly.
    #[test]
    fn a_nonsense_scale_falls_back_to_unscaled() {
        for bad in [0.0, -2.0, f32::NAN] {
            let r = screen_rect((10.0, 20.0, 30.0, 40.0), (0.0, 0.0), bad);
            assert_eq!(
                r,
                ScreenRect {
                    x: 10,
                    y: 20,
                    w: 30,
                    h: 40
                },
                "scale {bad}"
            );
        }
    }
}
