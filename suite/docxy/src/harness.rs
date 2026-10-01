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
use gpui::{App, Context, Entity, KeyDownEvent, Keystroke, Pixels, Point, Window, point, px, size};
use gridcore::sheet::{cell_name, parse_cell_name, parse_range_name};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

/// The command-line flag that turns the harness on.
pub const HARNESS_FLAG: &str = "--harness";

/// The environment variable that turns the harness on, for launchers that
/// cannot add an argument (equivalent to passing [`HARNESS_FLAG`]).
pub const HARNESS_ENV: &str = "DOCXY_HARNESS";

/// The command-line flag that opens every workbook on the line read-only
/// (#882). On Windows Excel's `/r` spelling works too; see [`is_read_only_flag`].
pub const READ_ONLY_FLAG: &str = "--read-only";

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
    /// `--read-only` (or `/r` on Windows) was passed: every workbook on the
    /// line opens read-only (#882). Documents and projects open as usual.
    pub read_only: bool,
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
            if is_read_only_flag(&arg, cfg!(windows)) {
                cli.read_only = true;
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

/// Whether `arg` asks for a read-only open: [`READ_ONLY_FLAG`] anywhere,
/// and Excel's `/r` (either case) on Windows only. Elsewhere `/r` is an
/// absolute path, so it stays a file. `windows` is a parameter so both
/// platforms' rules are tested on either.
fn is_read_only_flag(arg: &OsStr, windows: bool) -> bool {
    arg == READ_ONLY_FLAG || (windows && (arg == "/r" || arg == "/R"))
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

/// Where the `capture` verb leaves its pixels: inside the sandbox, beside the
/// control socket, one file that each capture overwrites. A driver reads it
/// straight after the reply, and a full window is several megabytes, so
/// keeping every capture would fill a run's directory for nothing.
#[cfg(any(test, feature = "harness-capture"))]
pub fn capture_path(root: &Path) -> PathBuf {
    root.join(CTL_APP).join("capture").join("last.rgba")
}

/// The desktop pixel the offscreen image's (0,0) corresponds to, computed by
/// the same [`screen_rect`] that `rect` answers with — so a region's rect minus
/// this origin is exactly where the region sits in the image, rounding and
/// all, and the driver's existing crop needs no second opinion about it.
#[cfg(any(test, feature = "harness-capture"))]
pub fn content_origin(win_origin: (f32, f32), scale: f32) -> (i32, i32) {
    let r = screen_rect((0.0, 0.0, 0.0, 0.0), win_origin, scale);
    (r.x, r.y)
}

/// What a build without offscreen capture answers `capture` with.
#[cfg(any(test, not(feature = "harness-capture")))]
pub const CAPTURE_UNAVAILABLE: &str = "this build cannot capture offscreen: rebuild the suite \
     with `--features harness-capture` (macOS only) to take pictures of a harness window";

/// This instance's control name. A normal instance is
/// `suite-<AGWINTERM_SESSION_ID|pid>`, the convention the terminal editors use,
/// so an agent can address the suite in its pane. A harness instance is always
/// `suite-<pid>` (#697): it inherits the pane id of whatever terminal launched
/// it, so every instance started from one pane would share one name, and a
/// launcher that knows only the pid it started could never find it.
pub fn instance_name(harness: bool) -> String {
    let session = std::env::var("AGWINTERM_SESSION_ID").ok();
    instance_name_for(harness, session.as_deref(), std::process::id())
}

/// [`instance_name`] with the environment passed in.
fn instance_name_for(harness: bool, session_id: Option<&str>, pid: u32) -> String {
    let session_id = if harness { None } else { session_id };
    ctlcore::instance_name_from(CTL_APP, session_id, pid)
}

/// Start the control server for a config root at `root`, named for the mode
/// it serves (see [`instance_name`]).
pub fn start(
    root: &Path,
    harness: bool,
) -> std::io::Result<(ctlcore::Server, Receiver<ctlcore::Request>)> {
    ctlcore::serve(&control_dir(root), &instance_name(harness))
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

/// What a document refuses to be saved as.
const DOC_SAVE_FORMATS: &str = "Documents can be saved as .docx, .md or .html";

/// The extension `save-as` gives a path that has none, for a `format`.
fn format_extension(format: &str) -> Option<&'static str> {
    Some(match format {
        "docx" => ".docx",
        "md" => ".md",
        "html" => ".docx.html",
        "xlsx" => ".xlsx",
        "xlsm" => ".xlsm",
        "xltx" => ".xltx",
        "xltm" => ".xltm",
        "yppx" => ".yppx",
        "xml" => ".xml",
        _ => return None,
    })
}

/// The format a document path saves as, by the app's own rules
/// (`is_markdown_path`, `htmlbundle::is_html_path`), or the refusal. Any other
/// extension is refused: `save_doc_tab` would write a Word package under it.
fn doc_save_format(path: &Path, html_ok: bool) -> Result<&'static str, String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    if crate::is_markdown_path(path) {
        Ok("md")
    } else if htmlbundle::is_html_path(&path.to_string_lossy()) {
        if html_ok {
            Ok("html")
        } else {
            Err("this build cannot write editable HTML (.html)".into())
        }
    } else if ext.as_deref() == Some("docx") {
        Ok("docx")
    } else {
        Err(DOC_SAVE_FORMATS.into())
    }
}

/// Resolve a `save-as` request to the file the dialog would have answered
/// with and the format it writes (#699). `raw` is the path as given: a
/// relative one resolves against `base`, the active file's directory, where
/// the dialog opens; a path with no extension takes `format`'s, or the tab
/// kind's default (the Save As dialog's first filter). The extension rules
/// are the app's own: `sheet_save_target`, `yppx::save_target`, and
/// [`doc_save_format`]. `format`, when given, must be one the tab kind saves
/// and must agree with an explicit extension.
fn save_as_target(
    kind: crate::Kind,
    base: Option<&Path>,
    raw: &str,
    format: Option<&str>,
    html_ok: bool,
) -> Result<(PathBuf, &'static str), String> {
    use crate::Kind;
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("save-as needs a non-empty 'path'".into());
    }
    // Formats are named like extensions, in any case.
    let format = format.map(|f| f.trim().to_ascii_lowercase());
    if format.as_deref() == Some("") {
        return Err("'format' must not be empty; leave it out to go by the extension".into());
    }
    let format = format.as_deref();
    let given = PathBuf::from(raw);
    let mut path = if given.is_absolute() {
        given
    } else {
        base.ok_or(
            "this tab has never been saved, so a relative 'path' has no folder: give an absolute path",
        )?
        .join(given)
    };
    let kind_formats: &[&str] = match kind {
        Kind::Docx => &["docx", "md", "html"],
        Kind::Xlsx => &crate::SHEET_EXTENSIONS,
        Kind::Project => &["yppx", "xml"],
        Kind::Look => return Err("this tab cannot be saved as a file".into()),
    };
    if let Some(f) = format {
        if !kind_formats.contains(&f) {
            // The kind's own refusal, in its own rule's words: the rule is
            // asked about an extension no kind saves, so it always refuses.
            let foreign = Path::new("x.not-a-format");
            return Err(match kind {
                Kind::Xlsx => crate::sheet_save_target(foreign).err(),
                Kind::Project => projcore::yppx::save_target(foreign).err(),
                _ => None,
            }
            .unwrap_or_else(|| DOC_SAVE_FORMATS.into()));
        }
    }
    if path.extension().is_none() {
        let ext = format_extension(format.unwrap_or(kind_formats[0])).unwrap_or_default();
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(ext);
        path.set_file_name(name);
    }
    let (path, written) = match kind {
        Kind::Xlsx => {
            let path = crate::sheet_save_target(&path)?;
            // The target keeps a workbook extension: its kind is the format.
            let written =
                gridcore::xlsx::SpreadsheetKind::from_path(&path).map_or("xlsx", |k| k.extension());
            (path, written)
        }
        Kind::Project => {
            let path = projcore::yppx::save_target(&path)?;
            let yppx = path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("yppx"));
            (path, if yppx { "yppx" } else { "xml" })
        }
        _ => {
            let written = doc_save_format(&path, html_ok)?;
            (path, written)
        }
    };
    if let Some(f) = format.filter(|f| *f != written) {
        return Err(format!(
            "'format' {f} does not match {}, which saves as {written}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    Ok((path, written))
}

/// A cell or range argument (`"A1"` or `"A1:C5"`), as its two corners.
pub fn range_arg(args: &Json, key: &str) -> Result<DragEnds, String> {
    let text = arg_str(args, key)?.trim();
    if let Some((r0, c0, r1, c1)) = parse_range_name(text) {
        return Ok(((r0, c0), (r1, c1)));
    }
    let cell = parse_cell(text)
        .map_err(|_| format!("'{text}' is not a cell or a range (expected A1 or A1:C5)"))?;
    Ok((cell, cell))
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

// ---- theme preference (pure) -----------------------------------------------

/// The `theme-set` verb's `theme`: `light`, `dark` or `auto` (follow the OS).
pub(crate) fn parse_theme_pref(name: &str) -> Result<crate::ThemePref, String> {
    match name.trim().to_ascii_lowercase().as_str() {
        "light" => Ok(crate::ThemePref::Light),
        "dark" => Ok(crate::ThemePref::Dark),
        "auto" => Ok(crate::ThemePref::Auto),
        other => Err(format!("unknown theme '{other}' (light, dark or auto)")),
    }
}

/// [`parse_theme_pref`]'s inverse.
pub(crate) fn theme_pref_name(pref: crate::ThemePref) -> &'static str {
    match pref {
        crate::ThemePref::Light => "light",
        crate::ThemePref::Dark => "dark",
        crate::ThemePref::Auto => "auto",
    }
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
    /// The bounded document tab strip and its overflow controls.
    TitleTabs,
    TabPrev,
    TabNext,
    TabMore,
    TabMoreItem(usize),
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
    /// The Project split bar between the entry table and the Gantt chart.
    ProjectSplit,
    /// The Home ribbon's Styles gallery: the well its tiles sit in.
    Gallery,
}

/// Parse a region name: `window`, `grid`, `chart-panel`, `cell:B3`,
/// `cell:A1:C5`, `chart:0`, `gantt`, `bar:3`, `project-hbar-table`,
/// `project-hbar-chart`, `project-vbar`, `project-timeline`, `project-split`,
/// `gallery`.
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
        "title-tabs" if arg.is_none() => Ok(Region::TitleTabs),
        "tab-prev" if arg.is_none() => Ok(Region::TabPrev),
        "tab-next" if arg.is_none() => Ok(Region::TabNext),
        "tab-more" if arg.is_none() => Ok(Region::TabMore),
        "tab-more-item" => {
            let i = arg
                .ok_or("'tab-more-item' needs an index")?
                .parse::<usize>()
                .map_err(|_| "'tab-more-item' needs a numeric index".to_string())?;
            Ok(Region::TabMoreItem(i))
        }
        "grid" if arg.is_none() => Ok(Region::Grid),
        "chart-panel" if arg.is_none() => Ok(Region::ChartPanel),
        "gantt" if arg.is_none() => Ok(Region::Gantt),
        "project-hbar-table" if arg.is_none() => Ok(Region::ProjectHbarTable),
        "project-hbar-chart" if arg.is_none() => Ok(Region::ProjectHbarChart),
        "project-vbar" if arg.is_none() => Ok(Region::ProjectVbar),
        "project-timeline" if arg.is_none() => Ok(Region::ProjectTimeline),
        "project-split" if arg.is_none() => Ok(Region::ProjectSplit),
        "gallery" if arg.is_none() => Ok(Region::Gallery),
        "window" | "grid" | "chart-panel" | "title-tabs" | "tab-prev" | "tab-next" | "tab-more"
        | "gantt" | "project-hbar-table" | "project-hbar-chart" | "project-vbar"
        | "project-timeline" | "project-split" | "gallery" => {
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
            "unknown region '{other}' (window, title-tabs, tab-prev, tab-next, tab-more, tab-more-item:0, grid, chart-panel, cell:B3, cell:A1:C5, chart:0, gantt, bar:3, project-hbar-table, project-hbar-chart, project-vbar, project-timeline, project-split, gallery)"
        )),
    }
}

/// The name a region reports itself under — [`parse_region`]'s inverse, so a
/// reply names the same thing the request did.
pub fn region_name(region: Region) -> String {
    match region {
        Region::Window => "window".into(),
        Region::TitleTabs => "title-tabs".into(),
        Region::TabPrev => "tab-prev".into(),
        Region::TabNext => "tab-next".into(),
        Region::TabMore => "tab-more".into(),
        Region::TabMoreItem(i) => format!("tab-more-item:{i}"),
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
        Region::ProjectSplit => "project-split".into(),
        Region::Gallery => "gallery".into(),
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
        other => Err(format!(
            "unknown field '{other}' (chart-range, chart-title, categories, \
             series-name:N, series-values:N, cond-format, validation, sort)"
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
    gridlines: bool,
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
            gridlines: app.view_gridlines,
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
        ("gridlines", Json::Bool(flags.gridlines)),
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
        ("table", table_state(editor)),
    ])
}

/// The caret's innermost table (#705), or null: its row count, the caret's
/// row and cell, its style and style options, the caret cell's shading and
/// text direction, the cell-range selection when it lies in this table, and
/// each cell's paragraphs as text (a tab shows as `⇥`, as the issues write
/// it, so a script can name it). Every field describes this one table, even
/// while a selection reaches out of it into an enclosing table.
fn table_state(editor: &Editor) -> Json {
    let Some(pos) = editor.table_at_caret() else {
        return Json::Null;
    };
    let Some(t) = editor.table(&pos.table) else {
        return Json::Null;
    };
    let cells = t
        .rows
        .iter()
        .map(|row| {
            Json::Arr(
                row.cells
                    .iter()
                    .map(|cell| {
                        Json::Arr(
                            cell.blocks
                                .iter()
                                .map(|b| Json::Str(b.plain_text().replace('\t', "⇥")))
                                .collect(),
                        )
                    })
                    .collect(),
            )
        })
        .collect();
    let props = docxcore::table::table_props(t);
    let look = props
        .get("w:tblLook")
        .map(docxcore::table_props::TblLook::parse)
        .unwrap_or_default();
    let range = editor.cell_range().filter(|r| r.table == pos.table);
    let range = range.map_or(Json::Null, |r| {
        Json::obj(vec![
            ("top", Json::Num(r.top as f64)),
            ("bottom", Json::Num(r.bottom as f64)),
            ("left", Json::Num(r.left as f64)),
            ("right", Json::Num(r.right as f64)),
        ])
    });
    let map = docxcore::table::GridMap::of(t);
    Json::obj(vec![
        ("rows", Json::Num(t.rows.len() as f64)),
        ("columns", Json::Num(map.width(t) as f64)),
        ("row", Json::Num(pos.row as f64)),
        ("cell", Json::Num(pos.cell as f64)),
        ("style", str_or_null(props.attr("w:tblStyle", "w:val"))),
        ("look", Json::Str(format!("{:04X}", look.bits()))),
        ("shading", str_or_null(editor.cell_shading())),
        ("text_direction", str_or_null(editor.cell_text_direction())),
        ("range", range),
        ("cells", Json::Arr(cells)),
    ])
}

/// Ruler coordinates come from the last painted frame, in logical pixels.
fn ruler_state(app: &crate::Docxy) -> Json {
    if !app.show_ruler {
        return Json::Null;
    }
    let probe = app.ruler_probe.borrow();
    let Some(g) = probe.painted.as_ref() else {
        return Json::Null;
    };
    let tenth = |n: f32| Json::Num(((n * 10.0).round() / 10.0) as f64);
    let mut fields = vec![
        ("frame", Json::Num(app.frame as f64)),
        ("first_offset", tenth(g.first_x - g.content_x)),
        ("left_offset", tenth(g.left_x - g.content_x)),
        ("right_offset", tenth(g.content_right - g.right_x)),
    ];
    if g.draft {
        fields.push(("column_inset", tenth(g.content_x - g.viewport.x)));
        fields.push(("text_inset", Json::Null));
        fields.push(("vtop_inset", Json::Null));
        fields.push(("vbottom_inset", Json::Null));
        fields.push(("tracked_page", Json::Null));
    } else {
        fields.push(("column_inset", Json::Null));
        fields.push(("text_inset", tenth(g.content_x - g.page_x)));
        fields.push(("vtop_inset", tenth(g.content_y - g.page_y)));
        fields.push(("vbottom_inset", tenth(g.page_bottom - g.content_bottom)));
        fields.push((
            "tracked_page",
            g.tracked_page
                .map(|i| Json::Num(i as f64))
                .unwrap_or(Json::Null),
        ));
    }
    // The tab stops the ruler draws, in twips from the text's left edge.
    let tabs = app
        .ruler_para()
        .1
        .iter()
        .map(|t| {
            Json::obj(vec![
                ("pos", Json::Num(t.pos as f64)),
                ("align", Json::Str(format!("{:?}", t.align).to_lowercase())),
            ])
        })
        .collect();
    fields.push(("tabs", Json::Arr(tabs)));
    Json::obj(fields)
}

fn live_doc_state(app: &crate::Docxy, window: &Window) -> Result<Json, String> {
    let mut doc = doc_state(active_doc(app)?, &ViewFlags::live(app, window));
    if let Json::Obj(fields) = &mut doc {
        fields.push(("ruler".into(), ruler_state(app)));
    }
    Ok(doc)
}

fn active_doc(app: &crate::Docxy) -> Result<&Editor, String> {
    match app.tabs.get(app.active).map(|t| &t.surface) {
        Some(crate::Surface::Doc(ed)) => Ok(ed),
        _ => Err("the active tab is not a document".into()),
    }
}

/// Which ribbon the active tab draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RibbonSurface {
    /// The declarative document/Project ribbon (`ribbon_for`).
    Model,
    /// The spreadsheet ribbon (`sheet_ribbon::SHEET_RIBBON`).
    Sheet,
}

/// Ribbon verbs address only a ribbon the window is drawing.
fn ribbon_surface(app: &crate::Docxy) -> Result<RibbonSurface, String> {
    if app.backstage {
        return Err("the ribbon is hidden while File (backstage) is open".into());
    }
    if app.ribbon_min {
        return Err("the ribbon is collapsed".into());
    }
    match app.tabs.get(app.active).map(|t| &t.surface) {
        Some(crate::Surface::Doc(_) | crate::Surface::Project(_)) => Ok(RibbonSurface::Model),
        Some(crate::Surface::Sheet(_)) => Ok(RibbonSurface::Sheet),
        _ => Err("the active tab has no ribbon".into()),
    }
}

/// One sheet ribbon command, in the document ribbon's reply shape. A sheet
/// button has no screentip or KeyTip, so the tip title is the label.
fn sheet_command_json(app: &crate::Docxy, cmd: &crate::sheet_ribbon::SheetCmd) -> Json {
    let label = cmd.label(app.sheet_act_toggled(cmd.act));
    Json::obj(vec![
        ("id", Json::Str(cmd.id.into())),
        ("label", Json::Str(label.into())),
        (
            "tip",
            Json::obj(vec![
                ("title", Json::Str(label.into())),
                ("body", Json::Str(String::new())),
            ]),
        ),
        ("key_tip", Json::Str(String::new())),
        (
            "checked",
            Json::Bool(crate::sheet_ribbon::act_on(cmd.act, &app.active_xf())),
        ),
        ("enabled", Json::Bool(cmd.enabled())),
    ])
}

/// The spreadsheet ribbon as `ribbon-read` reports it: File, then every tab
/// the sheet strip offers with the groups `sheet_ribbon_body` draws for it.
fn sheet_ribbon_json(app: &crate::Docxy) -> Json {
    let kind = crate::Kind::Xlsx;
    let mut tabs = vec![file_tab_json(kind)];
    for (tab, name, key_tip) in crate::ribbon_tab_set(kind) {
        let Some(tab) = tab else { continue };
        let def = crate::sheet_ribbon::tab_def(*tab);
        let groups = def
            .groups
            .iter()
            .map(|g| {
                Json::obj(vec![
                    ("title", Json::Str(g.title.into())),
                    ("launcher", Json::Bool(g.launcher)),
                    (
                        "commands",
                        Json::Arr(
                            g.commands()
                                .into_iter()
                                .map(|c| sheet_command_json(app, c))
                                .collect(),
                        ),
                    ),
                    ("galleries", Json::Arr(Vec::new())),
                ])
            })
            .collect();
        tabs.push(Json::obj(vec![
            ("name", Json::Str((*name).into())),
            ("key_tip", Json::Str((*key_tip).into())),
            ("kind", Json::Str("ribbon".into())),
            ("groups", Json::Arr(groups)),
        ]));
    }
    ribbon_reply(tabs)
}

/// Find a command on the sheet ribbon tab `tab_name`, as drawn now.
fn resolve_sheet_command(
    app: &crate::Docxy,
    tab_name: &str,
    query: &str,
) -> Result<(crate::RibbonTab, crate::SheetAct), String> {
    if tab_name == "File" {
        return Err("File is backstage; use the backstage verb".into());
    }
    let tab = ribbon_tab_by_name(crate::Kind::Xlsx, tab_name)?;
    let commands = crate::sheet_ribbon::tab_def(tab).commands();
    let cmd =
        crate::sheet_ribbon::resolve(&commands, tab_name, query, |act| app.sheet_act_toggled(act))?;
    Ok((tab, cmd.act))
}

/// The live status-line items in the order the app draws them.
fn status_json(items: &[(&str, String)]) -> Json {
    Json::obj(vec![(
        "items",
        Json::Arr(
            items
                .iter()
                .map(|(id, text)| {
                    Json::obj(vec![
                        ("id", Json::Str((*id).into())),
                        ("text", Json::Str(text.clone())),
                    ])
                })
                .collect(),
        ),
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
    /// The id of the split button or drop-down whose menu holds it.
    menu: Option<String>,
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
            menu: None,
        }
    }
    fn in_menu(cmd: &crate::rs::Cmd<crate::Act>, owner: &crate::rs::Cmd<crate::Act>) -> Self {
        Self {
            menu: Some(owner.id.into()),
            ..Self::from_cmd(cmd)
        }
    }
    fn json(
        &self,
        checked: &impl Fn(&RibbonCommand) -> bool,
        enabled: &impl Fn(crate::Act) -> bool,
    ) -> Json {
        let mut fields = vec![
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
            // The predicate the button draws with (#397).
            ("enabled", Json::Bool(enabled(self.act))),
        ];
        if let Some(menu) = &self.menu {
            fields.push(("menu", Json::Str(menu.clone())));
        }
        Json::obj(fields)
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
            out.extend(menu.iter().map(|c| RibbonCommand::in_menu(c, primary)));
        }
        Control::Dropdown { cmd, items } => {
            out.push(RibbonCommand::from_cmd(cmd));
            out.extend(items.iter().map(|c| RibbonCommand::in_menu(c, cmd)));
        }
        Control::Gallery(g) => out.extend(g.items.iter().map(|item| RibbonCommand {
            id: format!("{}:{}", g.id, item.label),
            label: item.label.into(),
            tip_title: g.tip.title.into(),
            tip_body: g.tip.body.into(),
            key_tip: String::new(),
            act: item.act,
            gallery: true,
            menu: None,
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
fn tab_json(
    tab: &crate::rs::Tab<crate::Act>,
    checked: &impl Fn(&RibbonCommand) -> bool,
    enabled: &impl Fn(crate::Act) -> bool,
) -> Json {
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
                    Json::Arr(commands.iter().map(|c| c.json(checked, enabled)).collect()),
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

/// Pure ribbon snapshot for a tab kind and its table and Gantt contexts.
#[cfg(test)]
fn ribbon_json_for(
    kind: crate::Kind,
    in_table: bool,
    in_gantt: bool,
    in_hf: bool,
    checked: impl Fn(&RibbonCommand) -> bool,
) -> Json {
    ribbon_json_with(kind, in_table, in_gantt, in_hf, checked, crate::act_enabled)
}

/// [`ribbon_json_for`] with the enabled predicate the buttons draw with.
fn ribbon_json_with(
    kind: crate::Kind,
    in_table: bool,
    in_gantt: bool,
    in_hf: bool,
    checked: impl Fn(&RibbonCommand) -> bool,
    enabled: impl Fn(crate::Act) -> bool,
) -> Json {
    let ribbon = crate::ribbon_for(kind);
    let mut tabs = vec![file_tab_json(kind)];
    tabs.extend(ribbon.tabs.iter().map(|t| tab_json(t, &checked, &enabled)));
    if kind == crate::Kind::Docx && in_hf {
        tabs.push(tab_json(&crate::hf_tab::hf_tab(), &checked, &enabled));
    }
    if kind == crate::Kind::Docx && in_table {
        tabs.push(tab_json(
            &crate::table_tab::table_design_tab(),
            &checked,
            &enabled,
        ));
        tabs.push(tab_json(
            &crate::table_tab::table_layout_tab(),
            &checked,
            &enabled,
        ));
    }
    if kind == crate::Kind::Project && in_gantt {
        tabs.push(tab_json(&crate::gantt_format_tab(), &checked, &enabled));
    }
    ribbon_reply(tabs)
}

/// The File tab entry every ribbon reply starts with.
fn file_tab_json(kind: crate::Kind) -> Json {
    let (_, file_name, file_tip) = crate::ribbon_tab_set(kind)[0];
    Json::obj(vec![
        ("name", Json::Str(file_name.into())),
        ("key_tip", Json::Str(file_tip.into())),
        ("kind", Json::Str("backstage".into())),
        ("groups", Json::Arr(Vec::new())),
    ])
}

/// A ribbon reply: the tabs, their count and the Quick Access Toolbar.
fn ribbon_reply(tabs: Vec<Json>) -> Json {
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
    let (kind, in_table) = (app.ribbon_kind(), app.caret_in_table());
    let mut json = ribbon_json_with(
        kind,
        in_table,
        app.project_gantt_showing(),
        app.hf_active(),
        |command| {
            if command.gallery && !matches!(command.act, crate::Act::Table(_)) {
                app.gallery_item_selected(command.act)
            } else {
                app.act_active(command.act)
            }
        },
        |act| app.act_enabled_now(act),
    );
    add_combo_values(&mut json, app);
    json
}

/// Give the Header from Top / Footer from Bottom boxes the value they show
/// (inches, as drawn: `0.5"`).
fn add_combo_values(json: &mut Json, app: &crate::Docxy) {
    let Json::Obj(fields) = json else { return };
    let Some((_, Json::Arr(tabs))) = fields.iter_mut().find(|(k, _)| k == "tabs") else {
        return;
    };
    for tab in tabs {
        let Json::Obj(tab) = tab else { continue };
        let Some((_, Json::Arr(groups))) = tab.iter_mut().find(|(k, _)| k == "groups") else {
            continue;
        };
        for group in groups {
            let Json::Obj(group) = group else { continue };
            let Some((_, Json::Arr(commands))) = group.iter_mut().find(|(k, _)| k == "commands")
            else {
                continue;
            };
            for command in commands {
                let Json::Obj(command) = command else {
                    continue;
                };
                let id = command.iter().find_map(|(k, v)| match (k.as_str(), v) {
                    ("id", Json::Str(id)) => Some(id.clone()),
                    _ => None,
                });
                let Some(menu) = id.as_deref().and_then(crate::hf_tab::menu_of) else {
                    continue;
                };
                let is_header = match menu {
                    crate::hf_tab::HfMenu::HeaderFromTop => true,
                    crate::hf_tab::HfMenu::FooterFromBottom => false,
                    _ => continue,
                };
                let value = app
                    .tabs
                    .get(app.active)
                    .and_then(|t| crate::hf_tab::distance_text(t, is_header))
                    .map_or(Json::Null, Json::Str);
                command.push(("value".into(), value));
            }
        }
    }
}

/// Resolve a currently valid tab name before a synthetic ribbon click.
fn ribbon_tab_by_name(kind: crate::Kind, name: &str) -> Result<crate::RibbonTab, String> {
    crate::ribbon_tab_set(kind)
        .iter()
        .find_map(|(tab, label, _)| (*label == name).then_some(*tab).flatten())
        .or_else(|| match name {
            crate::table_tab::DESIGN_TAB if kind == crate::Kind::Docx => {
                Some(crate::RibbonTab::TableDesign)
            }
            crate::table_tab::LAYOUT_TAB if kind == crate::Kind::Docx => {
                Some(crate::RibbonTab::TableLayout)
            }
            _ => None,
        })
        .or_else(|| {
            (kind == crate::Kind::Docx
                && name == crate::ribbon_tab_name(crate::RibbonTab::HeaderFooter))
            .then_some(crate::RibbonTab::HeaderFooter)
        })
        .or_else(|| {
            (kind == crate::Kind::Project
                && name == crate::ribbon_tab_name(crate::RibbonTab::GanttFormat))
            .then_some(crate::RibbonTab::GanttFormat)
        })
        .ok_or_else(|| format!("'{name}' is not a ribbon tab for the active document"))
}

/// Find a command on the actual active-kind ribbon definition.
fn resolve_ribbon_command(
    app: &crate::Docxy,
    tab_name: &str,
    query: &str,
) -> Result<crate::Act, String> {
    if tab_name == "File" {
        return Err("File is backstage; use the backstage verb".into());
    }
    let tab = ribbon_tab_def(app, tab_name)?;
    let commands = tab_commands(&tab);
    resolve_commands(&commands, tab_name, query)
}

/// A ribbon tab's definition as the active document shows it, contextual
/// tabs included only while they show.
fn ribbon_tab_def(
    app: &crate::Docxy,
    tab_name: &str,
) -> Result<crate::rs::Tab<crate::Act>, String> {
    let kind = app.ribbon_kind();
    ribbon_tab_by_name(kind, tab_name)?;
    let in_table = app.caret_in_table();
    let tab = if tab_name == crate::table_tab::DESIGN_TAB
        || tab_name == crate::table_tab::LAYOUT_TAB
    {
        if !in_table {
            return Err(format!("{tab_name} tab is not active outside a table"));
        }
        if tab_name == crate::table_tab::DESIGN_TAB {
            crate::table_tab::table_design_tab()
        } else {
            crate::table_tab::table_layout_tab()
        }
    } else if tab_name == crate::ribbon_tab_name(crate::RibbonTab::HeaderFooter) {
        if !app.hf_active() {
            return Err("Header & Footer tab is not active outside a header or footer".into());
        }
        crate::hf_tab::hf_tab()
    } else if tab_name == crate::ribbon_tab_name(crate::RibbonTab::GanttFormat) {
        if !app.project_gantt_showing() {
            return Err("Gantt Chart Format tab is not active without a Gantt view".into());
        }
        crate::gantt_format_tab()
    } else {
        crate::ribbon_for(kind)
            .tabs
            .into_iter()
            .find(|t| t.name == tab_name)
            .ok_or_else(|| format!("'{tab_name}' is not a ribbon tab for the active document"))?
    };
    Ok(tab)
}

/// The split button a `menu-open {"ribbon": [tab, group, label]}` names:
/// the id of its primary command, found by the primary's label as drawn.
fn split_primary(
    tab: &crate::rs::Tab<crate::Act>,
    group: &str,
    label: &str,
) -> Result<&'static str, String> {
    let g = tab
        .groups
        .iter()
        .find(|g| g.title == group)
        .ok_or_else(|| format!("no group '{group}' on tab '{}'", tab.name))?;
    for control in &g.items {
        match control {
            crate::Control::Split { primary: cmd, .. } | crate::Control::Dropdown { cmd, .. }
                if cmd.label == label =>
            {
                return Ok(cmd.id);
            }
            _ => {}
        }
    }
    let mut commands = Vec::new();
    for control in &g.items {
        control_commands(control, &mut commands);
    }
    Err(if commands.iter().any(|c| c.label == label) {
        format!("'{label}' has no menu")
    } else {
        format!(
            "no command '{label}' in group '{group}' on tab '{}'",
            tab.name
        )
    })
}

/// Where `menu-open` opens a menu: on the target's own probe when the last
/// frame drew it (a cell's middle; a split button uses the pointer's own
/// `split_menu_anchor`), else the middle of the window. The menu keeps
/// itself inside the window.
fn menu_point(
    app: &crate::Docxy,
    window: &Window,
    probe: Option<&str>,
    at: impl Fn(gpui::Bounds<Pixels>) -> Point<Pixels>,
) -> Point<Pixels> {
    probe
        .and_then(|name| app.probes.borrow().get(name))
        .map(at)
        .unwrap_or_else(|| {
            let size = window.viewport_size();
            point(size.width / 2., size.height / 2.)
        })
}

/// `menu-open`: open the menu on `target` through the opener its pointer
/// gesture uses. Targets without a menu yet are refused by name, never
/// mapped to another menu.
fn menu_open(
    app: &mut crate::Docxy,
    target: &Json,
    window: &mut Window,
    cx: &mut Context<crate::Docxy>,
) -> Result<(), String> {
    match target {
        Json::Str(name) if name == "document" => {
            if app.active_is_project() {
                return Err(
                    r#"the document menu does not open on a Project tab; a task row's is {"row": uid}"#
                        .into(),
                );
            }
            let at = menu_point(app, window, None, |b| b.center());
            app.open_document_menu(at, cx);
            Ok(())
        }
        Json::Obj(fields) if fields.len() == 1 => match fields[0].0.as_str() {
            "row" => {
                let uid = match &fields[0].1 {
                    Json::Null => None,
                    other => Some(
                        other
                            .as_i64()
                            .ok_or("'row' must be a task uid, or null for the entry row")?,
                    ),
                };
                let Some(crate::Surface::Project(v)) = app.tabs.get(app.active).map(|t| &t.surface)
                else {
                    return Err("a row menu needs a Project tab".into());
                };
                let tasks = &v.ed.project().tasks;
                // The entry row below the last task: a right-click there
                // opens the same menu, for no task.
                let Some(uid) = uid else {
                    let probe = format!("project-cell:entry:{}", v.col);
                    let row = tasks.len();
                    let at = menu_point(app, window, Some(&probe), |b| b.center());
                    return app.open_row_menu(row, None, at, window, cx);
                };
                let row = tasks
                    .iter()
                    .position(|t| i64::from(t.uid) == uid)
                    .ok_or_else(|| format!("no task with uid {uid}"))?;
                if !v.ed.visible_rows().contains(&row) {
                    return Err(format!(
                        "task {uid} is hidden under a collapsed summary; there is no row to right-click"
                    ));
                }
                let probe = format!("project-cell:{}:{}", tasks[row].id, v.col);
                let at = menu_point(app, window, Some(&probe), |b| b.center());
                app.open_row_menu(row, None, at, window, cx)
            }
            "ribbon" => {
                let path: Vec<&str> = fields[0]
                    .1
                    .as_array()
                    .map(|a| a.iter().filter_map(Json::as_str).collect())
                    .unwrap_or_default();
                let [tab, group, label] = path.as_slice() else {
                    return Err("'ribbon' must be [tab, group, command]".into());
                };
                if ribbon_surface(app)? == RibbonSurface::Sheet {
                    return Err("the sheet ribbon has no split buttons".into());
                }
                let def = ribbon_tab_def(app, tab)?;
                let id = split_primary(&def, group, label)?;
                app.select_ribbon_tab(ribbon_tab_by_name(app.ribbon_kind(), tab)?, window, cx);
                // Where the pointer's press on the arrow opens it too.
                let anchor = crate::split_menu_anchor(&app.probes.borrow(), id);
                let at = anchor.unwrap_or_else(|| menu_point(app, window, None, |b| b.center()));
                app.open_split_menu(id, at, cx)
            }
            other => Err(format!(
                "menu target '{other}' is not supported yet (document, row and ribbon are)"
            )),
        },
        _ => Err(r#"'target' must be "document" or one key such as {"row": uid}"#.into()),
    }
}

/// Refuse a menu verb while the window draws no menu: the backstage covers
/// the tab, and the more-tabs list hides an open menu.
fn refuse_under_cover(app: &crate::Docxy) -> Result<(), String> {
    cover_refusal(app.backstage, app.tab_more_open)
}

fn cover_refusal(backstage: bool, tab_more_open: bool) -> Result<(), String> {
    if backstage {
        return Err("File (backstage) is open; no menu shows".into());
    }
    if tab_more_open {
        return Err("the more-tabs list is open; no menu shows".into());
    }
    Ok(())
}

/// Whether `verb` stands for a press outside an open menu, which closes it
/// (#397). Reads leave it open, `key` and `type` reach the menu's own key
/// gate, the `menu-*` verbs act on it, and a control-pipe verb closes it
/// when it changes the plan or the focus (`dispatch_project`).
fn closes_menu(verb: &str, args: &Json) -> bool {
    // Reading the backstage is a read; opening or closing it is a press.
    if verb == "backstage" {
        return args.get_str("action") != Some("read");
    }
    matches!(
        verb,
        "click-cell"
            | "drag"
            | "fill-drag"
            | "save-as"
            | "select-chart"
            | "focus-field"
            | "ribbon-click"
            | "title-tab"
            | "close-tab"
            | "selection-set"
            | "open"
            | "backstage-close"
            | "theme-set"
            | "ask-on-close"
            | "autorecover"
            | "dialog-set"
            | "dialog-tab"
            | "dialog-click"
            | "enable-editing"
    )
}

/// `menu-click`'s `{label}` or `{path}`, as the labels to walk.
fn menu_path(args: &Json) -> Result<Vec<&str>, String> {
    match (args.get("label"), args.get("path")) {
        (Some(label), None) => Ok(vec![label.as_str().ok_or("'label' must be a string")?]),
        (None, Some(path)) => path
            .as_array()
            .ok_or("'path' must be an array of labels")?
            .iter()
            .map(|l| {
                l.as_str()
                    .ok_or_else(|| "'path' must be an array of labels".to_string())
            })
            .collect(),
        _ => Err("menu-click takes 'label' or 'path'".into()),
    }
}

/// Resolve by id first, then by label, then by screentip title, each tier
/// tried only when the one before matched nothing and used only when unique.
/// The screentip tier lets instructions that name an icon-only command by its
/// screentip ("Indent Task") find it, as they would in Microsoft Project.
fn resolve_commands(
    commands: &[RibbonCommand],
    tab_name: &str,
    query: &str,
) -> Result<crate::Act, String> {
    let tiers: [fn(&RibbonCommand) -> &str; 3] = [|c| &c.id, |c| &c.label, |c| &c.tip_title];
    let matches: Vec<_> = tiers
        .iter()
        .map(|field| {
            commands
                .iter()
                .filter(|c| field(c) == query)
                .collect::<Vec<_>>()
        })
        .find(|m| !m.is_empty())
        .unwrap_or_default();
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

/// What the active tab's paste would take besides the clipboard's text: the
/// document's rich clip or the sheet's grid clip, while the clipboard still
/// holds what that copy put there (`clip_still_ours`); otherwise `none`, and a
/// paste takes `text`.
fn clipboard_app_json(
    surface: Option<&crate::Surface>,
    doc: Option<&crate::DocClip>,
    grid: Option<&crate::GridClip>,
    now: &crate::ClipRead,
) -> Json {
    match surface {
        Some(crate::Surface::Doc(_)) => {
            if let Some(clip) = doc.filter(|c| crate::clip_still_ours(&c.text, now)) {
                return Json::obj(vec![
                    ("kind", Json::Str("doc".into())),
                    ("text", Json::Str(clip.text.clone())),
                ]);
            }
        }
        Some(crate::Surface::Sheet(_)) => {
            if let Some(clip) = grid.filter(|c| crate::clip_still_ours(&c.text, now)) {
                return Json::obj(vec![
                    ("kind", Json::Str("grid".into())),
                    ("text", Json::Str(clip.text.clone())),
                    ("rows", Json::Num(clip.cells.len() as f64)),
                    (
                        "cols",
                        Json::Num(clip.cells.first().map_or(0, Vec::len) as f64),
                    ),
                ]);
            }
        }
        _ => {}
    }
    Json::obj(vec![("kind", Json::Str("none".into()))])
}

/// The `clipboard` reply: the clipboard's text (the harness's private one)
/// and what the active tab's paste would use.
fn clipboard_json(app: &crate::Docxy, cx: &App) -> Json {
    let now = app.clipboard_read(cx);
    let surface = app.tabs.get(app.active).map(|t| &t.surface);
    let used = clipboard_app_json(surface, app.clip.as_ref(), app.grid_clip.as_ref(), &now);
    Json::obj(vec![
        ("text", str_or_null(now.text().map(str::to_string))),
        ("app", used),
    ])
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
            "autorecover_minutes",
            Json::Num(f64::from(app.autorecover_minutes)),
        ),
        (
            "ribbon_tab",
            Json::Str(
                crate::ribbon_tab_name(crate::valid_ribbon_tab(
                    app.ribbon_kind(),
                    app.ribbon_tab,
                    app.caret_in_table(),
                    app.project_gantt_showing(),
                    app.hf_active(),
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
        // The active tab's caption and open mode (#610).
        (
            "caption",
            Json::Str(
                app.tabs
                    .get(app.active)
                    .map(|t| t.caption())
                    .unwrap_or_default(),
            ),
        ),
        (
            "read_only",
            Json::Bool(app.tabs.get(app.active).is_some_and(|t| t.access.read_only)),
        ),
        (
            "protected",
            Json::Bool(app.tabs.get(app.active).is_some_and(|t| t.access.protected)),
        ),
        (
            "repaired",
            Json::Bool(app.tabs.get(app.active).is_some_and(|t| t.access.repaired)),
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
        // The Project status bar's Ready / Edit / Busy; null off a Project.
        (
            "app_state",
            app.tabs
                .get(app.active)
                .and_then(crate::tab_app_state)
                .map_or(Json::Null, |s| Json::Str(s.label().into())),
        ),
        // The open menu's target, or null; `menu-read` has its items.
        (
            "menu",
            app.menu.as_ref().map_or(Json::Null, |m| {
                Json::obj(vec![("target", m.target.to_json())])
            }),
        ),
        // The active tab's top dialog's id, or `none`; `dialog-read` has the rest.
        (
            "dialog",
            Json::Str(
                app.tabs
                    .get(app.active)
                    .map_or("none", |t| t.dialogs.top_id())
                    .into(),
            ),
        ),
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
    if active_doc(app).is_ok() {
        if let Ok(Json::Obj(fields)) = live_doc_state(app, window) {
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

/// The kind a tab is, as `tab-list` names it: the Backstage › New cards'.
fn kind_name(kind: crate::Kind) -> &'static str {
    match kind {
        crate::Kind::Docx => "docx",
        crate::Kind::Xlsx => "xlsx",
        crate::Kind::Project => "project",
        crate::Kind::Look => "mail",
    }
}

/// `tab-list`: every open tab, in strip order, and which one is active.
fn tab_list(tabs: &[crate::DocTab], active: usize) -> Json {
    let tabs = tabs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            Json::obj(vec![
                ("index", Json::Num(i as f64)),
                ("title", Json::Str(t.title.to_string())),
                ("kind", Json::Str(kind_name(t.kind).into())),
                (
                    "path",
                    str_or_null(t.path.as_ref().map(|p| p.to_string_lossy().into_owned())),
                ),
                ("dirty", Json::Bool(t.dirty)),
                ("imported", Json::Bool(crate::is_imported(t))),
                ("caption", Json::Str(t.caption())),
                ("read_only", Json::Bool(t.access.read_only)),
                ("protected", Json::Bool(t.access.protected)),
                ("repaired", Json::Bool(t.access.repaired)),
            ])
        })
        .collect();
    Json::obj(vec![
        ("active", Json::Num(active as f64)),
        ("tabs", Json::Arr(tabs)),
    ])
}

/// The active tab's dialogs, for a verb that drives one.
fn open_dialogs(app: &mut crate::Docxy) -> Result<&mut crate::dialog::DialogStack, String> {
    app.tabs
        .get_mut(app.active)
        .map(|t| &mut t.dialogs)
        .filter(|d| d.is_open())
        .ok_or_else(|| crate::dialog::NONE_OPEN.into())
}

// ---- the verb table -------------------------------------------------------

/// Route one harness verb against the live app.
///
/// Every driving verb goes through the same entry point the pointer or the
/// keyboard would: `click-cell` is the cell's own click handler, `drag` is a
/// press plus one move per cell crossed plus the release, ordinary `key`/`type`
/// input uses [`crate::Docxy::on_key`], action-bound Tab variants use their
/// `tab_key`/`shift_tab_key` handlers, and `select-chart` is the press on a chart
/// card. `title-tab` invokes the same tab arrow/dropdown handler methods as the
/// title-bar elements. `window-size` and `window-zoom` are window setup verbs
/// that call GPUI's window APIs directly.
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
    // A levelling pass asked for by an earlier verb runs before this one
    // reads or changes anything, so every reply sees a settled plan whether
    // or not a frame ran in between. The verb that asks for one replies
    // before it runs, with `app_state` "Busy".
    app.flush_project_passes(cx);
    // A verb that stands for a press outside an open menu closes it first,
    // as that press would (the backdrop closes it, then the press goes on).
    if closes_menu(verb, args) && app.close_menu() {
        cx.notify();
    }
    match verb {
        "window-zoom" => {
            window.zoom_window();
            cx.notify();
            Done::ok(Json::obj(vec![(
                "maximized",
                Json::Bool(window.is_maximized()),
            )]))
        }
        "window-size" => {
            let w = arg_usize(args, "w")?;
            let h = arg_usize(args, "h")?;
            if !(300..=4096).contains(&w) || !(200..=4096).contains(&h) {
                return Err("window size must be 300..4096 by 200..4096 logical pixels".into());
            }
            window.resize(size(px(w as f32), px(h as f32)));
            cx.notify();
            Done::ok(Json::obj(vec![
                ("w", Json::Num(w as f64)),
                ("h", Json::Num(h as f64)),
            ]))
        }
        "title-bar" => {
            let probes = app.probes.borrow();
            let read = |name: &str| {
                probes
                    .get(name)
                    .ok_or_else(|| format!("{name} has not been laid out yet"))
            };
            let content = read("title-content")?;
            let strip = read("title-tabs")?;
            let drag = read("title-drag")?;
            let theme = read("title-theme")?;
            // The root probe measures the actual inner box after Root's CSD
            // shadow and border. This oracle is independent of the renderer's
            // title-width arithmetic.
            let root = read("suite-root")?;
            let caption_w = if cfg!(any(target_os = "macos", target_family = "wasm")) {
                0.0
            } else {
                3.0 * f32::from(gpui_component::TITLE_BAR_HEIGHT)
            };
            let caption_left = f32::from(root.origin.x + root.size.width) - caption_w;
            let content_right = f32::from(content.origin.x + content.size.width);
            let strip_right = f32::from(strip.origin.x + strip.size.width);
            let content_left = f32::from(content.origin.x);
            let drag_left = f32::from(drag.origin.x);
            let drag_right = f32::from(drag.origin.x + drag.size.width);
            let drag_w = (drag_right.min(content_right) - drag_left.max(content_left)).max(0.0);
            let theme_left = f32::from(theme.origin.x);
            let theme_right = f32::from(theme.origin.x + theme.size.width);
            let theme_visible =
                theme_left >= content_left - 0.5 && theme_right <= content_right + 0.5;
            let layout = app.tab_layout;
            let active_visible = layout.active_visible(app.active)
                && probes.get("title-active-chip").is_some_and(|chip| {
                    let left = f32::from(chip.origin.x);
                    let right = f32::from(chip.origin.x + chip.size.width);
                    left >= f32::from(strip.origin.x) - 0.5
                        && right <= strip_right + 0.5
                        && right <= content_right + 0.5
                });
            let active_dirty_visible = app.tabs.get(app.active).is_some_and(|tab| tab.dirty)
                && probes.get("title-active-chip").is_some_and(|chip| {
                    probes.get("title-active-dirty").is_some_and(|mark| {
                        let chip_left = f32::from(chip.origin.x);
                        let chip_right = f32::from(chip.origin.x + chip.size.width);
                        let mark_left = f32::from(mark.origin.x);
                        let mark_right = f32::from(mark.origin.x + mark.size.width);
                        mark_left >= chip_left - 0.5 && mark_right <= chip_right + 0.5
                    })
                });
            Done::ok(Json::obj(vec![
                ("tabs", Json::Num(app.tabs.len() as f64)),
                ("active", Json::Num(app.active as f64)),
                ("first", Json::Num(layout.first as f64)),
                ("visible", Json::Num((layout.end - layout.first) as f64)),
                ("mode", Json::Str(layout.mode.name().into())),
                ("overflow", Json::Bool(layout.more)),
                ("active_visible", Json::Bool(active_visible)),
                ("active_dirty_visible", Json::Bool(active_dirty_visible)),
                ("theme_visible", Json::Bool(theme_visible)),
                ("strip_right", Json::Num(strip_right as f64)),
                ("content_right", Json::Num(content_right as f64)),
                ("caption_left", Json::Num(caption_left as f64)),
                (
                    "controls_clear",
                    Json::Bool(content_right <= caption_left + 0.5),
                ),
                ("drag_w", Json::Num(drag_w as f64)),
                (
                    "drag_ok",
                    Json::Bool(drag_w + 0.5 >= crate::tabstrip::DRAG_MIN_W),
                ),
            ]))
        }
        "title-tab" => {
            app.refuse_under_dialog()?;
            match arg_str(args, "action")? {
                "prev" => app.tab_prev(window, cx),
                "next" => app.tab_next(window, cx),
                "more" => app.tab_more_toggle(cx),
                "pick" => {
                    let i = arg_usize(args, "index")?;
                    if i >= app.tabs.len() {
                        return Err(format!("no tab {i}"));
                    }
                    if !app.tab_more_open {
                        return Err("the more-tabs list is closed".into());
                    }
                    app.tab_more_pick(i, window, cx);
                }
                _ => return Err("'action' must be prev, next, more or pick".into()),
            }
            Done::ok(state(app, window))
        }
        "tab-list" => Done::ok(tab_list(&app.tabs, app.active)),
        // The tab chip's click handler, by index or title/path substring.
        "tab-select" => {
            let tab = args.get("tab").ok_or("tab-select needs 'tab'")?;
            app.refuse_under_dialog()?;
            let i = crate::control::match_tab(&app.tabs, tab, false)?;
            app.select_tab(i, window, cx);
            Done::ok(state(app, window))
        }
        "doc" => Done::ok(live_doc_state(app, window)?),
        // Header/footer editing state (#641): which area of which section and
        // variant, its labels, and the contextual tab's Options and Position.
        "hf-state" => Done::ok(crate::hf_tab::hf_state(app.tabs.get(app.active))),
        // A pointer double-click on a print-layout page's header, footer or
        // body area (0-based `page`), through the handler the page draws.
        "page-double-click" => {
            app.refuse_under_dialog()?;
            app.close_menu();
            let page = arg_usize(args, "page")?;
            let area = match arg_str(args, "area")? {
                "header" => crate::PageArea::Header,
                "footer" => crate::PageArea::Footer,
                "body" => crate::PageArea::Body,
                _ => return Err("'area' must be header, footer or body".into()),
            };
            app.page_double_click(page, area, window, cx)?;
            Done::ok(crate::hf_tab::hf_state(app.tabs.get(app.active)))
        }
        "selection-set" => {
            app.refuse_under_dialog()?;
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
        // Dialogs (#393). There is no `dialog-open`: a dialog opens through
        // the verb a person would use (`key`, `ribbon-click`, `click-cell`).
        "dialog-read" => Done::ok(app.tabs.get(app.active).map_or_else(
            || crate::dialog::DialogStack::default().to_json(),
            |t| t.dialogs.to_json(),
        )),
        // The control's input handler, the one the overlay's editable widgets
        // call too (#649).
        "dialog-set" => {
            let control = arg_str(args, "control")?.to_string();
            let dialogs = open_dialogs(app)?;
            dialogs.set(&control, args)?;
            let reply = dialogs.to_json();
            cx.notify();
            Done::ok(reply)
        }
        "dialog-tab" => {
            let tab = arg_str(args, "tab")?.to_string();
            let dialogs = open_dialogs(app)?;
            dialogs.select_tab(&tab)?;
            let reply = dialogs.to_json();
            cx.notify();
            Done::ok(reply)
        }
        // The same press as the drawn button, Enter or Escape. The reply is
        // the state after the button's handler, with whatever dialog is on
        // top now (a child it opened, the parent, or `{open: false}`).
        "dialog-click" => {
            let button = arg_str(args, "button")?.to_string();
            open_dialogs(app)?;
            app.dialog_press(&button)?;
            app.refocus(window, cx);
            let Json::Obj(mut out) = state(app, window) else {
                unreachable!("state is an object")
            };
            let dialog = app.tabs[app.active].dialogs.to_json();
            match out.iter_mut().find(|(k, _)| k == "dialog") {
                Some((_, v)) => *v = dialog,
                None => out.push(("dialog".into(), dialog)),
            }
            Done::ok(Json::Obj(out))
        }
        "status-read" => {
            let tab = app.tabs.get(app.active).ok_or("there is no active tab")?;
            Done::ok(status_json(&crate::status_items(tab)))
        }
        "backstage" => {
            match arg_str(args, "action")? {
                "open" => {
                    app.refuse_under_dialog()?;
                    app.open_backstage(cx)
                }
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
        "theme-set" => {
            let pref = parse_theme_pref(arg_str(args, "theme")?)?;
            app.set_theme_pref(pref, window, cx);
            Done::ok(Json::obj(vec![
                ("theme", Json::Str(theme_pref_name(pref).into())),
                (
                    "resolved",
                    Json::Str(
                        if pref.resolve(window.appearance()) == gpui_component::ThemeMode::Dark {
                            "dark"
                        } else {
                            "light"
                        }
                        .into(),
                    ),
                ),
            ]))
        }
        "ribbon-read" => match ribbon_surface(app)? {
            RibbonSurface::Model => Done::ok(ribbon_json(app)),
            RibbonSurface::Sheet => Done::ok(sheet_ribbon_json(app)),
        },
        "ribbon-click" => {
            app.refuse_under_dialog()?;
            let surface = ribbon_surface(app)?;
            let tab = arg_str(args, "tab")?.to_string();
            let command = arg_str(args, "command")?.to_string();
            if surface == RibbonSurface::Sheet {
                // The button's own click: select its tab, then run its act.
                let (ribbon_tab, act) = resolve_sheet_command(app, &tab, &command)?;
                app.select_ribbon_tab(ribbon_tab, window, cx);
                app.run_sheet_act(act, window, cx);
                return Done::ok(state(app, window));
            }
            let act = resolve_ribbon_command(app, &tab, &command)?;
            app.select_ribbon_tab(ribbon_tab_by_name(app.ribbon_kind(), &tab)?, window, cx);
            app.dispatch(act, window, cx);
            Done::ok(state(app, window))
        }
        // Menus (#397): opened through the opener the right-click or the
        // split button's arrow calls, clicked through the item's own handler.
        "menu-open" => {
            app.refuse_under_dialog()?;
            refuse_under_cover(app)?;
            let target = args.get("target").ok_or("menu-open needs a 'target'")?;
            menu_open(app, target, window, cx)?;
            Done::ok(crate::menu::read_json(app.menu.as_ref()))
        }
        "menu-read" => Done::ok(crate::menu::read_json(app.menu.as_ref())),
        // Insert > Table's hover grid (#646): `{cols, rows}` moves the pointer
        // over that cell (`{}` off the grid), `click: true` clicks it. The
        // same handlers the drawn cells call; the reply carries the header.
        "table-grid" => {
            app.refuse_under_dialog()?;
            let at = match (args.get("cols"), args.get("rows")) {
                (None, None) => None,
                _ => Some((arg_usize(args, "cols")?, arg_usize(args, "rows")?)),
            };
            let click = matches!(args.get("click"), Some(Json::Bool(true)));
            let header = crate::table_tab::grid_header(at);
            match (at, click) {
                (Some((c, r)), true) => app.table_grid_click(c, r, window, cx)?,
                (None, true) => return Err("a click names 'cols' and 'rows'".into()),
                (at, false) => app.table_grid_hover(at, cx)?,
            }
            Done::ok(Json::obj(vec![
                ("header", Json::Str(header)),
                ("state", state(app, window)),
            ]))
        }
        "menu-click" => {
            app.refuse_under_dialog()?;
            refuse_under_cover(app)?;
            let labels = menu_path(args)?;
            let menu = app.menu.as_ref().ok_or("no menu is open")?;
            let path = crate::menu::resolve(&menu.items, &labels)?;
            app.menu_activate(&path, window, cx)?;
            Done::ok(state(app, window))
        }
        "menu-close" => {
            app.refuse_under_dialog()?;
            if !app.close_menu() {
                return Err("no menu is open".into());
            }
            cx.notify();
            Done::ok(state(app, window))
        }
        "close-tab" => {
            app.refuse_under_dialog()?;
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
            app.refuse_under_dialog()?;
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
        // The Settings AutoRecover interval, in minutes; 0 turns it off.
        "autorecover" => {
            let minutes = u32::try_from(arg_usize(args, "minutes")?)
                .map_err(|_| "'minutes' is too large".to_string())?;
            app.set_autorecover_minutes(minutes, cx);
            Done::ok(state(app, window))
        }
        // One AutoRecover tick now, as the timer would run it once the
        // interval is up, so a test need not wait minutes. `wrote` says
        // whether anything was unsaved and so written. Runs even when the
        // setting is off: it is the tick, not the schedule.
        "autorecover-now" => {
            let wrote = app.autorecover_tick();
            cx.notify();
            Done::ok(Json::obj(vec![("wrote", Json::Bool(wrote))]))
        }
        "ping" => Done::ok(Json::obj(vec![
            ("instance", Json::Str(instance_name(true))),
            ("pid", Json::Num(std::process::id() as f64)),
            (
                "config_root",
                Json::Str(crate::config_root().display().to_string()),
            ),
            ("tabs", Json::Num(app.tabs.len() as f64)),
        ])),

        // Open a file, exactly as a path on the command line does, through
        // the one `open_path` every open takes (#610). `mode` is a
        // workbook's open mode (`normal`, `read-only`, `copy`, `repair`).
        // A file already open is reloaded without asking, because `open` is
        // a case's setup; `reopen: "ask"` asks as a person's open does, and
        // the question is a dialog the `dialog-*` verbs answer. A relative
        // `path` resolves against the active tab's folder, as `save-as`'s
        // does, so a case can name the copy `open copy:` made.
        "open" => {
            let raw = arg_str(args, "path")?;
            let path = if Path::new(raw).is_relative() {
                // A relative path has no folder of its own; never the CWD.
                let base = app
                    .tabs
                    .get(app.active)
                    .and_then(|t| t.path.as_deref())
                    .and_then(Path::parent)
                    .ok_or(
                        "the active tab has never been saved, so a relative 'path' has no folder: give an absolute path",
                    )?;
                base.join(raw)
            } else {
                PathBuf::from(raw)
            };
            if !path.is_file() {
                return Err(format!("no such file: {raw}"));
            }
            let mode = match args.get("mode") {
                None => crate::open_mode::OpenMode::Normal,
                Some(Json::Str(m)) => crate::open_mode::OpenMode::parse(m)
                    .ok_or("'mode' must be normal, read-only, copy or repair")?,
                Some(_) => return Err("'mode' must be a string".into()),
            };
            let reopen = match args.get("reopen") {
                None => crate::open_mode::Reopen::Always,
                Some(Json::Str(r)) if r == "always" => crate::open_mode::Reopen::Always,
                Some(Json::Str(r)) if r == "ask" => crate::open_mode::Reopen::Ask,
                Some(_) => return Err("'reopen' must be \"always\" or \"ask\"".into()),
            };
            let loaded = app.open_path(&path, mode, reopen)?;
            app.backstage = false;
            app.drop_grid_state();
            app.persist();
            cx.notify();
            // An open tab was only focused, or asked about: nothing was
            // loaded, so its status says something else and is not judged.
            if !loaded {
                return Done::ok(state(app, window));
            }
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

        // The Protected View message bar's Enable Editing button (#610),
        // which the harness has no generic way to press.
        "enable-editing" => {
            app.refuse_under_dialog()?;
            if !app.tabs.get(app.active).is_some_and(|t| t.access.protected) {
                return Err("the active tab is not in Protected View".into());
            }
            app.enable_editing(cx);
            Done::ok(state(app, window))
        }

        // A click on a cell: press, click, release — the three events the
        // pointer delivers, in that order.
        "click-cell" => {
            app.refuse_under_dialog()?;
            let (shift, dbl) = (arg_flag(args, "shift")?, arg_flag(args, "double")?);
            if app.active_is_project() {
                let tab = &mut app.tabs[app.active];
                let crate::Surface::Project(v) = &tab.surface else {
                    return Err("Project is not loaded".into());
                };
                // Rows are drawn rows; the one just below the last is the entry row.
                let target = crate::project_cell_target(args, &v.ed)?;
                if target.col >= crate::COLUMN_COUNT {
                    return Err("Project cell is outside the entry table".into());
                }
                if target.row.is_none() {
                    return Err(
                        "that task is hidden under a collapsed summary; expand it to click it"
                            .into(),
                    );
                }
                match target.at {
                    Some(crate::ShownRow::Task(i)) => {
                        crate::project_cell_click(tab, i, Some(target.col), dbl)
                    }
                    Some(crate::ShownRow::Entry) => {
                        crate::project_entry_click(tab, Some(target.col), dbl)
                    }
                    None => return Err("Project cell is outside the entry table".into()),
                }
                app.refocus(window, cx);
                return Done::ok(state(app, window));
            }
            let cell = cell_arg(args, "cell")?;
            sheet(app)?;
            click_cell(app, cell, shift, dbl, window, cx);
            Done::ok(state(app, window))
        }

        // Save As without the native dialog (#699), which a harness instance
        // must never open (its modal loop stops the control pump). The target
        // the dialog would have answered with goes to the same save functions
        // its answer feeds; success is what they return.
        "save-as" => {
            app.refuse_under_dialog()?;
            let raw = match args.get("path") {
                Some(Json::Str(path)) => path.as_str(),
                Some(_) => return Err("'path' must be a string".into()),
                None => return Err("save-as needs a 'path'".into()),
            };
            let format = match args.get("format") {
                None => None,
                Some(Json::Str(f)) => Some(f.as_str()),
                Some(_) => return Err("'format' must be a string".into()),
            };
            let overwrite = arg_flag(args, "overwrite")?;
            let tab = app.tabs.get(app.active).ok_or("no tab is open")?;
            let base = tab.path.as_deref().and_then(Path::parent);
            let (target, written) = save_as_target(
                tab.kind,
                base,
                raw,
                format,
                crate::doc_html_save_allowed(tab),
            )?;
            // The dialog asks before replacing a file; a case says so up front.
            if target.exists() && !overwrite {
                return Err(format!(
                    "{} already exists; pass \"overwrite\": true to replace it",
                    target.display()
                ));
            }
            let saved = match tab.kind {
                crate::Kind::Xlsx => app.save_sheet_as(&target, window, cx),
                crate::Kind::Project => app.save_project_to(&target, window, cx).is_ok(),
                _ => app.save_doc_to(Some(target.clone()), window, cx),
            };
            let tab = &app.tabs[app.active];
            if !saved {
                return Err(tab.status.to_string());
            }
            Done::ok(Json::obj(vec![
                (
                    "path",
                    str_or_null(tab.path.as_ref().map(|p| p.display().to_string())),
                ),
                ("format", Json::Str(written.into())),
                ("title", Json::Str(tab.title.to_string())),
                ("dirty", Json::Bool(tab.dirty)),
                ("status", Json::Str(tab.status.to_string())),
            ]))
        }

        // The fill handle (#699): the handle's own press, one move per cell
        // crossed, the release — what a pointer dragging the handle does. The
        // `drag` verb presses the grid instead, so it sweeps a selection.
        "fill-drag" => {
            app.refuse_under_dialog()?;
            if args.get("option").is_some() {
                return Err("AutoFill Options are not implemented in this app".into());
            }
            let to = cell_arg(args, "to")?;
            let from = match args.get("from") {
                Some(_) => Some(range_arg(args, "from")?),
                None => None,
            };
            sheet(app)?;
            // Refuse before `from` moves anything: a click would land in an
            // open edit or a pointing reference rather than select cells.
            fill_press_refusal(
                app.backstage,
                app.tab_more_open,
                app.fill_handle_hidden_reason(),
                from.is_some(),
            )?;
            if let Some((start, end)) = from {
                click_cell(app, start, false, false, window, cx);
                if end != start {
                    click_cell(app, end, true, false, window, cx);
                }
            }
            fill_press_refusal(
                app.backstage,
                app.tab_more_open,
                app.fill_handle_hidden_reason(),
                false,
            )?;
            let src = sheet(app)?.range();
            app.sheet_fill_start(cx);
            if app.sheet_fill.is_none() {
                return Err(if app.protected_view() {
                    format!(
                        "the fill did not arm: {}",
                        crate::open_mode::PROTECTED_STATUS
                    )
                } else if app.sheet_protected() {
                    "the fill did not arm: the sheet is protected".into()
                } else {
                    "the fill did not arm: another gesture is in flight".into()
                });
            }
            for (r, c) in drag_path((src.2, src.3), to) {
                app.grid_drag_over(r, c, cx);
            }
            app.grid_release(cx);
            let after = sheet(app)?.range();
            let mut reply = state(app, window);
            if let Json::Obj(fields) = &mut reply {
                let filled = (after != src).then(|| a1_range(after));
                fields.push(("filled".into(), str_or_null(filled)));
            }
            Done::ok(reply)
        }

        // The clipboard (#699). A harness instance has a private one (it starts
        // empty and never touches the OS clipboard); `write` puts text on it
        // as another app's copy would. Copy, cut and paste are the app's own
        // keys and buttons (`key ctrl+c`, `ribbon-click`), not a second route.
        "clipboard" => {
            match arg_str(args, "action")? {
                "read" => {}
                "write" => {
                    let text = arg_str(args, "text")?.to_string();
                    app.clipboard_write(text, cx);
                }
                "paste-special" => {
                    return Err("paste special is not implemented in this app".into());
                }
                action @ ("copy" | "cut" | "paste") => {
                    return Err(format!(
                        "clipboard does not {action}: press the app's own key (key ctrl+c, ctrl+x, ctrl+v) or ribbon-click its button"
                    ));
                }
                other => {
                    return Err(format!(
                        "unknown clipboard action '{other}' (read, write, paste-special)"
                    ));
                }
            }
            Done::ok(clipboard_json(app, cx))
        }

        // A drag: the press plants the anchor, each cell crossed is a move, the
        // release commits whatever the moves armed.
        "drag" => {
            app.refuse_under_dialog()?;
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
            app.refuse_under_dialog()?;
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
            app.refuse_under_dialog()?;
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
            if let Some(crate::Surface::Project(v)) = app.tabs.get(app.active).map(|t| &t.surface) {
                // Rows are drawn rows; `{uid, column}` reaches a hidden task too.
                let target = crate::project_cell_target(args, &v.ed)?;
                let c = target.col;
                if c >= crate::COLUMN_COUNT {
                    return Err("No Project column at this index".into());
                }
                let task = match target.at.ok_or("No task at this row")? {
                    crate::ShownRow::Task(i) => Some(&v.ed.project().tasks[i]),
                    crate::ShownRow::Entry => None,
                };
                let text = task
                    .map(|t| crate::project_row(&v.ed, t)[c].clone())
                    .unwrap_or_default();
                // A task a collapsed summary hides has no drawn row to name.
                let row = target.row;
                let mut out = vec![
                    (
                        "cell",
                        row.map_or(Json::Null, |r| Json::Str(a1((r as u32, c as u32)))),
                    ),
                    ("row", row.map_or(Json::Null, |r| Json::Num(r as f64))),
                    ("col", Json::Num(c as f64)),
                    ("text", Json::Str(text.clone())),
                    ("value", Json::Str(text.clone())),
                    ("empty", Json::Bool(text.is_empty())),
                    ("entry", Json::Bool(task.is_none())),
                ];
                if let Some(t) = task {
                    out.push(("id", Json::Num(f64::from(t.id))));
                    out.push(("uid", Json::Num(f64::from(t.uid))));
                }
                return Done::ok(Json::obj(out));
            }
            let (r, c) = cell_arg(args, "cell")?;
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
            Done::ok_drawn(Json::obj(vec![("frame", Json::Num(app.frame as f64))]))
        }

        // The window's pixels, rendered offscreen by the app itself. macOS only:
        // there a harness window is never on screen and reading another
        // process's pixels needs Screen Recording, so the driver cannot take
        // the picture from outside the way `PrintWindow` does on Windows.
        "capture" => capture(app, window),

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

        // The rows a Project's entry table draws, top to bottom, without the
        // entry row. Reads only: a `tab` other than the active one stays behind.
        "rows" => {
            let i = crate::control::resolve_project_tab(&app.tabs, app.active, args.get("tab"))?;
            let crate::Surface::Project(v) = &app.tabs[i].surface else {
                return Err("Project is not loaded".into());
            };
            Done::ok(crate::rows_json(&v.ed))
        }

        // Persist and go. The reply is written first (see the pump).
        "quit" => {
            crate::close::commit_pending_for_exit(&mut app.tabs);
            // Not through `on_window_should_close`: clear the run marker here
            // too, or every harness relaunch would look like a crash (#632).
            app.clean_exit();
            Ok(Done {
                result: Json::obj(vec![("quitting", Json::Bool(true))]),
                quit: true,
                draw: false,
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

/// Why `fill-drag` cannot press the fill handle, or `Ok` when it can. The
/// handle is not there to press while File (backstage) covers the sheet or the
/// more-tabs list covers the window, or while the grid does not draw it
/// (`hidden`). With `selecting_from`, the verb is about to click `from`, so a
/// reason such a click clears (a selected chart) is left to the check after
/// it; the others refuse first, so a refused verb has changed nothing.
fn fill_press_refusal(
    backstage: bool,
    tab_more_open: bool,
    hidden: Option<crate::HandleHidden>,
    selecting_from: bool,
) -> Result<(), String> {
    if backstage {
        return Err("the fill handle is not shown: File (backstage) is open".into());
    }
    if tab_more_open {
        return Err("the fill handle is covered: the more-tabs list is open".into());
    }
    match hidden {
        Some(h) if !(selecting_from && h.cleared_by_a_click()) => {
            Err(format!("the fill handle is not shown: {}", h.why()))
        }
        _ => Ok(()),
    }
}

/// A click on a sheet cell, as the pointer makes it: press, the cell's click
/// handler, release.
fn click_cell(
    app: &mut crate::Docxy,
    cell: (u32, u32),
    shift: bool,
    double: bool,
    window: &mut Window,
    cx: &mut Context<crate::Docxy>,
) {
    app.grid_press_cell(cell, cx);
    app.cell_click(cell.0, cell.1, shift, double, window, cx);
    app.grid_release(cx);
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

/// Render the last drawn frame to an offscreen texture and leave the RGBA in
/// the sandbox; reply with where it is and how to map a desktop rect onto it.
///
/// ⚠️ This renders the scene gpui last *drew*, not what a compositor showed:
/// nothing here is ever on screen. That is what lets it work for a window that
/// was never shown, and it is why a caller must settle first — the harness's
/// `frame` verb is what draws on macOS, so a capture without one would render
/// a stale scene.
#[cfg(feature = "harness-capture")]
fn capture(app: &crate::Docxy, window: &mut Window) -> Result<Done, String> {
    let img = window
        .render_to_image()
        .map_err(|e| format!("the offscreen render failed: {e}"))?;
    let path = capture_path(&crate::config_root());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, img.as_raw()).map_err(|e| format!("{}: {e}", path.display()))?;
    let win = window.bounds();
    let scale = window.scale_factor();
    let (ox, oy) = content_origin((f32::from(win.origin.x), f32::from(win.origin.y)), scale);
    Done::ok(Json::obj(vec![
        ("path", Json::Str(path.display().to_string())),
        ("width", Json::Num(img.width() as f64)),
        ("height", Json::Num(img.height() as f64)),
        ("scale", Json::Num(scale as f64)),
        ("frame", Json::Num(app.frame as f64)),
        (
            "content_origin",
            Json::obj(vec![
                ("x", Json::Num(ox as f64)),
                ("y", Json::Num(oy as f64)),
            ]),
        ),
    ]))
}

#[cfg(not(feature = "harness-capture"))]
fn capture(_app: &crate::Docxy, _window: &mut Window) -> Result<Done, String> {
    Err(CAPTURE_UNAVAILABLE.to_string())
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

    /// #699, #755: `clipboard` reports what the active tab's paste would take:
    /// the document clip on a document and the grid clip on a sheet, each only
    /// while the clipboard still holds its text, and nothing on other surfaces.
    #[test]
    fn clipboard_reports_the_clip_the_next_paste_would_use() {
        use crate::ClipRead;
        let doc = crate::Surface::Doc(editor(ParProps::default(), RunProps::default()));
        let sheet = crate::new_sheet_surface();
        let clip = crate::DocClip {
            clip: docxcore::editor::Clip::from_text("one\ntwo"),
            text: "one\ntwo".into(),
        };
        let grid = crate::GridClip {
            cells: vec![vec![Default::default(); 3]; 2],
            text: "a\tb\tc\nd\te\tf\n".into(),
        };
        let kind = |j: Json| j.get_str("kind").unwrap().to_string();
        let nothing = ClipRead::Nothing;
        let text = |t: &str| ClipRead::Text(t.into());

        let on_doc = clipboard_app_json(Some(&doc), Some(&clip), Some(&grid), &nothing);
        assert_eq!(kind(on_doc.clone()), "doc");
        assert_eq!(on_doc.get_str("text"), Some("one\ntwo"));
        let bare = clipboard_app_json(Some(&doc), None, Some(&grid), &nothing);
        assert_eq!(kind(bare), "none");
        let doc_echoed = clipboard_app_json(Some(&doc), Some(&clip), None, &text("one\r\ntwo"));
        assert_eq!(kind(doc_echoed), "doc");
        // Another app's copy, text or image, is newer than the document clip.
        let doc_replaced = clipboard_app_json(Some(&doc), Some(&clip), None, &text("other"));
        assert_eq!(kind(doc_replaced), "none");
        let doc_image = clipboard_app_json(Some(&doc), Some(&clip), None, &ClipRead::NotText);
        assert_eq!(kind(doc_image), "none");

        let ours = clipboard_app_json(Some(&sheet), Some(&clip), Some(&grid), &text(&grid.text));
        assert_eq!(kind(ours.clone()), "grid");
        assert_eq!(ours.get("rows"), Some(&Json::Num(2.)));
        assert_eq!(ours.get("cols"), Some(&Json::Num(3.)));
        let crlf = text(&grid.text.replace('\n', "\r\n"));
        let echoed = clipboard_app_json(Some(&sheet), None, Some(&grid), &crlf);
        assert_eq!(kind(echoed), "grid");
        let replaced = clipboard_app_json(Some(&sheet), None, Some(&grid), &text("x\ty"));
        assert_eq!(kind(replaced), "none");
        // An image copied since is newer than the grid clip; a clipboard with
        // no item at all cannot say it changed.
        let image = clipboard_app_json(Some(&sheet), None, Some(&grid), &ClipRead::NotText);
        assert_eq!(kind(image), "none");
        let unread = clipboard_app_json(Some(&sheet), None, Some(&grid), &nothing);
        assert_eq!(kind(unread), "grid");

        let placeholder = crate::Surface::Placeholder;
        assert_eq!(
            kind(clipboard_app_json(
                Some(&placeholder),
                Some(&clip),
                Some(&grid),
                &nothing
            )),
            "none"
        );
        assert_eq!(kind(clipboard_app_json(None, None, None, &nothing)), "none");
    }

    /// #697: a harness instance ignores the pane it was launched from and is
    /// named by its pid; a normal instance keeps the pane id agents address.
    #[test]
    fn a_harness_instance_is_named_by_pid_whatever_pane_launched_it() {
        assert_eq!(instance_name_for(true, Some("pane-1"), 42), "suite-42");
        assert_eq!(instance_name_for(true, None, 42), "suite-42");
        assert_eq!(instance_name_for(false, Some("pane-1"), 42), "suite-pane-1");
        assert_eq!(instance_name_for(false, None, 42), "suite-42");
    }

    /// `tab-list` over one tab of every kind: a blank plan has no path, an
    /// .mpp is imported, and `active` and `dirty` are each tab's own.
    #[test]
    fn tab_list_reports_every_tab_of_every_kind() {
        let doc = |kind, title: &str| crate::DocTab {
            kind,
            title: title.to_owned().into(),
            path: None,
            surface: crate::Surface::Placeholder,
            dirty: false,
            status: "".into(),
            comments: vec![],
            pkg: None,
            notes: vec![],
            markdown: false,
            hf_edit: None,
            bundle_html: None,
            load_failed: false,
            dialogs: crate::dialog::DialogStack::default(),
            access: crate::open_mode::Access::default(),
        };
        let mut word = doc(crate::Kind::Docx, "a.docx");
        word.path = Some("C:/work/a.docx".into());
        word.dirty = true;
        let book = doc(crate::Kind::Xlsx, "Untitled.xlsx");
        let blank = crate::new_project_tab();
        let mut mpp = doc(crate::Kind::Project, "plan.mpp");
        mpp.path = Some("C:/work/plan.mpp".into());
        let inbox = doc(crate::Kind::Look, "Inbox");
        let list = tab_list(&[word, book, blank, mpp, inbox], 2);
        assert_eq!(list.get("active"), Some(&Json::Num(2.)));
        let tabs = list.get("tabs").and_then(Json::as_array).unwrap();
        let row = |i: usize| {
            let t = &tabs[i];
            (
                t.get("index").cloned(),
                t.get_str("title").unwrap().to_string(),
                t.get_str("kind").unwrap().to_string(),
                t.get("path").cloned(),
                t.get("dirty").cloned(),
                t.get("imported").cloned(),
            )
        };
        let expect = |i: usize, title: &str, kind: &str, path: Option<&str>, dirty, imported| {
            (
                Some(Json::Num(i as f64)),
                title.to_string(),
                kind.to_string(),
                Some(path.map_or(Json::Null, |p| Json::Str(p.into()))),
                Some(Json::Bool(dirty)),
                Some(Json::Bool(imported)),
            )
        };
        assert_eq!(tabs.len(), 5);
        assert_eq!(
            row(0),
            expect(0, "a.docx", "docx", Some("C:/work/a.docx"), true, false)
        );
        assert_eq!(
            row(1),
            expect(1, "Untitled.xlsx", "xlsx", None, false, false)
        );
        assert_eq!(
            row(2),
            expect(2, "Untitled.yppx", "project", None, false, false)
        );
        assert_eq!(
            row(3),
            expect(
                3,
                "plan.mpp",
                "project",
                Some("C:/work/plan.mpp"),
                false,
                true
            )
        );
        assert_eq!(row(4), expect(4, "Inbox", "mail", None, false, false));
    }

    #[test]
    fn theme_set_names_parse_and_round_trip() {
        for pref in [
            crate::ThemePref::Light,
            crate::ThemePref::Dark,
            crate::ThemePref::Auto,
        ] {
            assert_eq!(parse_theme_pref(theme_pref_name(pref)), Ok(pref));
        }
        assert_eq!(parse_theme_pref(" Dark "), Ok(crate::ThemePref::Dark));
        let e = parse_theme_pref("dim").unwrap_err();
        assert!(
            e.contains("dim") && e.contains("light, dark or auto"),
            "{e}"
        );
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
            gridlines: true,
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
                gridlines: true,
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
                gridlines: true,
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

    /// The contextual Header & Footer tab is in `ribbon-read` only while a
    /// header or footer is being edited (#641).
    #[test]
    fn ribbon_read_lists_the_header_and_footer_tab_only_while_editing() {
        let names = |in_hf: bool| -> Vec<String> {
            let json = ribbon_json_for(crate::Kind::Docx, false, false, in_hf, |_| false);
            json.get("tabs")
                .and_then(Json::as_array)
                .unwrap()
                .iter()
                .map(|t| t.get("name").and_then(Json::as_str).unwrap().to_string())
                .collect()
        };
        assert!(names(true).contains(&"Header & Footer".to_string()));
        assert!(!names(false).contains(&"Header & Footer".to_string()));
        assert!(
            ribbon_tab_by_name(crate::Kind::Docx, "Header & Footer")
                == Ok(crate::RibbonTab::HeaderFooter)
        );
    }

    #[test]
    fn ribbon_reflects_definition_and_checked_state() {
        let off = ribbon_json_for(crate::Kind::Docx, false, false, false, |_| false);
        let on = ribbon_json_for(crate::Kind::Docx, true, false, false, |c| {
            matches!(c.act, crate::Act::Bold)
        });
        let tabs = off.get("tabs").unwrap().as_array().unwrap();
        assert_eq!(tabs[0].get_str("name"), Some("File"));
        assert_eq!(tabs[0].get_str("kind"), Some("backstage"));
        assert_eq!(
            tabs.iter()
                .map(|t| t.get_str("name").unwrap())
                .collect::<Vec<_>>(),
            vec!["File", "Home", "Insert", "Layout", "Review", "View"]
        );
        let tabs_on = on.get("tabs").unwrap().as_array().unwrap();
        let n = tabs_on.len();
        assert_eq!(tabs_on[n - 2].get_str("name"), Some("Table Design"));
        assert_eq!(tabs_on[n - 1].get_str("name"), Some("Table Layout"));
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
    fn ribbon_lists_gantt_chart_format_only_with_a_project_gantt() {
        let names = |ribbon: &Json| {
            ribbon
                .get("tabs")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.get_str("name").unwrap().to_string())
                .collect::<Vec<_>>()
        };
        let on = ribbon_json_for(crate::Kind::Project, false, true, false, |c| {
            matches!(c.act, crate::Act::Project(crate::ProjectAct::CriticalTasks))
        });
        assert_eq!(
            names(&on),
            [
                "File",
                "Task",
                "Resource",
                "Report",
                "Project",
                "View",
                "Gantt Chart Format"
            ]
        );
        let tab = on.get("tabs").unwrap().as_array().unwrap().last().unwrap();
        assert_eq!(tab.get_str("key_tip"), Some("O"));
        let group = &tab.get("groups").unwrap().as_array().unwrap()[0];
        assert_eq!(group.get_str("title"), Some("Bar Styles"));
        let checked = group
            .get("commands")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c.get_str("label").unwrap(), c.get("checked").cloned()))
            .collect::<Vec<_>>();
        assert_eq!(
            checked,
            [
                ("Critical Tasks", Some(Json::Bool(true))),
                ("Baseline", Some(Json::Bool(false)))
            ]
        );
        let off = ribbon_json_for(crate::Kind::Project, false, false, false, |_| false);
        assert!(!names(&off).iter().any(|n| n == "Gantt Chart Format"));
        let docx = ribbon_json_for(crate::Kind::Docx, false, true, false, |_| false);
        assert!(!names(&docx).iter().any(|n| n == "Gantt Chart Format"));
        assert!(ribbon_tab_by_name(crate::Kind::Project, "Gantt Chart Format").is_ok());
        assert!(ribbon_tab_by_name(crate::Kind::Docx, "Gantt Chart Format").is_err());
    }

    #[test]
    fn style_gallery_checked_matches_heading_preview() {
        let mut props = ParProps::default();
        props.style_id = Some("Heading1".into());
        let editor = editor(props, RunProps::default());
        let ribbon = ribbon_json_for(crate::Kind::Docx, false, false, false, |c| {
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
        let ribbon = ribbon_json_for(crate::Kind::Docx, false, false, false, |c| {
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
    fn ribbon_resolver_falls_back_to_the_screentip_after_id_and_label() {
        use crate::{Act, ProjectAct};
        let task = crate::project_ribbon()
            .tabs
            .into_iter()
            .find(|t| t.name == "Task")
            .unwrap();
        let commands = tab_commands(&task);
        for query in ["pr-indent", "Indent", "Indent Task"] {
            assert!(
                matches!(
                    resolve_commands(&commands, "Task", query),
                    Ok(Act::Project(ProjectAct::Indent))
                ),
                "{query}"
            );
        }
        assert!(matches!(
            resolve_commands(&commands, "Task", "Link the Selected Tasks"),
            Ok(Act::Project(ProjectAct::AddLink))
        ));
        // A label beats another command's screentip, and screentip matches
        // are refused when ambiguous, as labels are.
        let cmd = |id: &str, label: &str, tip: &str, act| RibbonCommand {
            id: id.into(),
            label: label.into(),
            tip_title: tip.into(),
            tip_body: String::new(),
            key_tip: String::new(),
            act,
            gallery: false,
            menu: None,
        };
        let commands = vec![
            cmd("one", "Move", "Move Task", Act::Bold),
            cmd("two", "Other", "Move", Act::Italic),
            cmd("three", "Third", "Shared", Act::Underline),
            cmd("four", "Fourth", "Shared", Act::Bold),
        ];
        assert!(matches!(
            resolve_commands(&commands, "Task", "Move"),
            Ok(Act::Bold)
        ));
        assert!(matches!(
            resolve_commands(&commands, "Task", "Move Task"),
            Ok(Act::Bold)
        ));
        let err = resolve_commands(&commands, "Task", "Shared").unwrap_err();
        assert!(err.contains("three, four"), "{err}");
    }

    #[test]
    fn project_report_tab_is_listed_with_no_groups() {
        let json = ribbon_json_for(crate::Kind::Project, false, false, false, |_| false);
        let report = json
            .get("tabs")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t.get_str("name") == Some("Report"))
            .unwrap();
        assert_eq!(report.get_str("kind"), Some("ribbon"));
        assert_eq!(report.get_str("key_tip"), Some("R"));
        assert_eq!(report.get("groups"), Some(&Json::Arr(Vec::new())));
        assert!(ribbon_tab_by_name(crate::Kind::Project, "Report").is_ok());
    }

    /// #397: every command reports `enabled` from the predicate its button
    /// draws with, and a split's menu items name the split they sit in.
    #[test]
    fn ribbon_read_reports_enabled_and_the_menu_a_command_sits_in() {
        for kind in [crate::Kind::Docx, crate::Kind::Project] {
            let json = ribbon_json_for(kind, true, true, false, |_| false);
            let ribbon = crate::ribbon_for(kind);
            let mut defs: Vec<RibbonCommand> = ribbon.tabs.iter().flat_map(tab_commands).collect();
            if kind == crate::Kind::Docx {
                defs.extend(tab_commands(&crate::table_tab::table_design_tab()));
                defs.extend(tab_commands(&crate::table_tab::table_layout_tab()));
            } else {
                defs.extend(tab_commands(&crate::gantt_format_tab()));
            }
            let read: Vec<&Json> = json
                .get("tabs")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|t| t.get("groups").unwrap().as_array().unwrap())
                .flat_map(|g| g.get("commands").unwrap().as_array().unwrap())
                .collect();
            assert_eq!(read.len(), defs.len());
            for (c, def) in read.iter().zip(&defs) {
                assert_eq!(c.get_str("id"), Some(def.id.as_str()));
                assert_eq!(
                    c.get("enabled"),
                    Some(&Json::Bool(crate::act_enabled(def.act))),
                    "{}",
                    def.id
                );
            }
        }
        let json = ribbon_json_for(crate::Kind::Project, false, false, false, |_| false);
        let schedule = json
            .get("tabs")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t.get_str("name") == Some("Project"))
            .unwrap()
            .get("groups")
            .unwrap()
            .as_array()
            .unwrap()[0]
            .get("commands")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|c| (c.get_str("label").unwrap(), c.get_str("menu")))
            .collect::<Vec<_>>();
        assert_eq!(
            schedule,
            [
                ("Calculate Project", None),
                ("Set Baseline", None),
                ("Set Baseline...", Some("pr-baseline")),
                ("Clear Baseline...", Some("pr-baseline")),
            ]
        );
    }

    /// #397: `ribbon-click` still finds the commands that moved into Set
    /// Baseline's menu by the names a test already uses.
    #[test]
    fn ribbon_click_names_still_reach_set_and_clear_baseline() {
        let ribbon = crate::ribbon_for(crate::Kind::Project);
        let project = ribbon.tabs.iter().find(|t| t.name == "Project").unwrap();
        let commands = tab_commands(project);
        let act = |q| resolve_commands(&commands, "Project", q);
        assert!(matches!(
            act("Set Baseline"),
            Ok(crate::Act::Project(crate::ProjectAct::Baseline))
        ));
        for q in ["Clear Baseline", "Clear Baseline...", "pr-baseline-clear"] {
            assert!(
                matches!(
                    act(q),
                    Ok(crate::Act::Project(crate::ProjectAct::ClearBaseline))
                ),
                "{q}"
            );
        }
    }

    /// #397: `menu-open {"ribbon": [...]}` finds a split button by its
    /// primary's label, and says why when the named command has no menu.
    #[test]
    fn a_ribbon_menu_target_names_a_split_button() {
        let ribbon = crate::ribbon_for(crate::Kind::Project);
        let project = ribbon.tabs.iter().find(|t| t.name == "Project").unwrap();
        assert_eq!(
            split_primary(project, "Schedule", "Set Baseline"),
            Ok("pr-baseline")
        );
        assert_eq!(
            split_primary(project, "Schedule", "Calculate Project"),
            Err("'Calculate Project' has no menu".into())
        );
        assert_eq!(
            split_primary(project, "Schedule", "Nope"),
            Err("no command 'Nope' in group 'Schedule' on tab 'Project'".into())
        );
        assert_eq!(
            split_primary(project, "Nope", "Set Baseline"),
            Err("no group 'Nope' on tab 'Project'".into())
        );
    }

    /// #397: the verbs that stand for a press close an open menu; reads,
    /// keys and the menu verbs do not.
    #[test]
    fn pointer_verbs_close_an_open_menu_and_reads_do_not() {
        for verb in [
            "click-cell",
            "drag",
            "fill-drag",
            "save-as",
            "ribbon-click",
            "title-tab",
            "close-tab",
            "selection-set",
            "open",
            "select-chart",
            "focus-field",
        ] {
            assert!(closes_menu(verb, &Json::obj(vec![])), "{verb}");
        }
        for verb in [
            "menu-open",
            "menu-read",
            "menu-click",
            "menu-close",
            "key",
            "type",
            "ribbon-read",
            "clipboard",
            "dialog-read",
            "status-read",
            "doc",
            "rows",
            "cell",
            "selection",
            "capture",
            "rect",
            "ping",
            "task.set",
        ] {
            assert!(!closes_menu(verb, &Json::obj(vec![])), "{verb}");
        }
        let action = |a: &str| Json::obj(vec![("action", Json::Str(a.into()))]);
        assert!(closes_menu("backstage", &action("open")));
        assert!(closes_menu("backstage", &action("close")));
        assert!(!closes_menu("backstage", &action("read")), "a read");
    }

    /// #397: no menu opens or runs while File or the more-tabs list covers
    /// the tab, since the window draws none then.
    #[test]
    fn menus_are_refused_under_the_backstage_and_the_more_tabs_list() {
        assert_eq!(cover_refusal(false, false), Ok(()));
        assert!(
            cover_refusal(true, false)
                .unwrap_err()
                .contains("backstage")
        );
        assert!(
            cover_refusal(false, true)
                .unwrap_err()
                .contains("more-tabs list")
        );
    }

    /// #397: `menu-click` takes a label or a path of labels, not both.
    #[test]
    fn menu_click_takes_a_label_or_a_path() {
        let parse = |text: &str| Json::parse(text).unwrap();
        assert_eq!(
            menu_path(&parse(r#"{"label":"Insert Task"}"#)),
            Ok(vec!["Insert Task"])
        );
        assert_eq!(
            menu_path(&parse(r#"{"path":["Insert","Insert Task"]}"#)),
            Ok(vec!["Insert", "Insert Task"])
        );
        for bad in [
            r#"{}"#,
            r#"{"label":"A","path":["A"]}"#,
            r#"{"label":3}"#,
            r#"{"path":"A"}"#,
            r#"{"path":["A",1]}"#,
        ] {
            assert!(menu_path(&parse(bad)).is_err(), "{bad}");
        }
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
                menu: None,
            },
            RibbonCommand {
                id: "two".into(),
                label: "Same".into(),
                tip_title: String::new(),
                tip_body: String::new(),
                key_tip: String::new(),
                act: crate::Act::Italic,
                gallery: false,
                menu: None,
            },
        ];
        let err = resolve_commands(&commands, "Home", "Same").err().unwrap();
        assert!(err.contains("one, two"));
        assert!(resolve_commands(&commands, "Home", "one").is_ok());
        let json = status_json(&[("state", "Ready".into()), ("message", "saved".into())]);
        let items = json.get("items").unwrap().as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].get_str("id"), Some("state"));
        assert_eq!(items[0].get_str("text"), Some("Ready"));
        assert_eq!(items[1].get_str("id"), Some("message"));
        assert_eq!(items[1].get_str("text"), Some("saved"));
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

    /// `--read-only` turns read-only on for the whole line and is not a file.
    #[test]
    fn read_only_flag_is_recognized_and_is_not_a_file() {
        let cli = parse_args(args(&["a.xlsx", READ_ONLY_FLAG, "b.docx"]));
        assert!(cli.read_only);
        assert!(!cli.harness);
        assert!(cli.unknown_flags.is_empty());
        assert_eq!(
            cli.files,
            vec![PathBuf::from("a.xlsx"), PathBuf::from("b.docx")]
        );
        assert!(!parse_args(args(&["a.xlsx"])).read_only);
    }

    /// Excel's `/r` and `/R` are the flag on Windows only; elsewhere they are
    /// absolute paths. Nothing else that merely starts with `/r` is a flag.
    #[test]
    fn slash_r_is_a_flag_on_windows_only() {
        for spelling in ["/r", "/R"] {
            assert!(is_read_only_flag(OsStr::new(spelling), true), "{spelling}");
            assert!(!is_read_only_flag(OsStr::new(spelling), false), "{spelling}");
        }
        assert!(is_read_only_flag(OsStr::new(READ_ONLY_FLAG), false));
        for other in ["/ro", "/read-only", "-r", "/x", "--READ-ONLY"] {
            assert!(!is_read_only_flag(OsStr::new(other), true), "{other}");
        }
        let cli = parse_args(args(&["/r", "a.xlsx"]));
        assert_eq!(cli.read_only, cfg!(windows));
        let files: Vec<PathBuf> = if cfg!(windows) {
            vec![PathBuf::from("a.xlsx")]
        } else {
            vec![PathBuf::from("/r"), PathBuf::from("a.xlsx")]
        };
        assert_eq!(cli.files, files);
    }

    /// After `--`, the read-only spellings are files.
    #[test]
    fn double_dash_makes_read_only_spellings_files() {
        let cli = parse_args(args(&["--", READ_ONLY_FLAG, "/r"]));
        assert!(!cli.read_only);
        assert_eq!(
            cli.files,
            vec![PathBuf::from(READ_ONLY_FLAG), PathBuf::from("/r")]
        );
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
    fn a_capture_is_written_inside_the_sandbox_beside_the_control_socket() {
        let root = Path::new("/runs/harness-42");
        let p = capture_path(root);
        assert_eq!(p, root.join("suite").join("capture").join("last.rgba"));
        assert!(
            p.starts_with(root),
            "a capture must never leave the sandbox"
        );
    }

    /// The contract the driver's crop depends on: a region's desktop rect,
    /// minus the capture's content origin, is where that region sits in the
    /// offscreen image. Checked against the very function `rect` answers with,
    /// on fractional origins, a Retina scale, and a display left of the main one.
    #[test]
    fn a_region_rect_minus_the_content_origin_is_where_it_sits_in_the_image() {
        let cases = [
            ((370.0, 140.0), 1.0),
            ((370.5, 140.25), 2.0),
            ((-1920.0, -100.0), 1.0),
            ((0.5, 0.25), 1.5),
        ];
        for (origin, scale) in cases {
            let o = content_origin(origin, scale);
            let corner = screen_rect((0.0, 0.0, 1.0, 1.0), origin, scale);
            assert_eq!(
                (corner.x - o.0, corner.y - o.1),
                (0, 0),
                "the window's own top-left must be image pixel (0,0) at {origin:?} x{scale}"
            );
            for (x, y) in [(10.0_f32, 20.0_f32), (333.3, 77.7)] {
                let r = screen_rect((x, y, 5.0, 5.0), origin, scale);
                let (ix, iy) = ((r.x - o.0) as f32, (r.y - o.1) as f32);
                assert!(
                    (ix - x * scale).abs() <= 1.0 && (iy - y * scale).abs() <= 1.0,
                    "region at ({x},{y}) lands at ({ix},{iy}), not ~({},{}), at {origin:?} x{scale}",
                    x * scale,
                    y * scale
                );
            }
        }
    }

    #[test]
    fn a_build_without_capture_names_the_feature_to_build_with() {
        assert!(
            CAPTURE_UNAVAILABLE.contains("harness-capture"),
            "{CAPTURE_UNAVAILABLE}"
        );
    }

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

    /// #610: Open and Repair's status names what it emptied or dropped, and
    /// `open mode: repair` must still read it as a load that worked; a
    /// repair that had to give up is a failed open.
    #[test]
    fn repair_status_is_not_a_load_failure() {
        let dir = crate::open_mode_tests::Scratch::new();
        let src = crate::open_mode_tests::damaged_book(&dir, "book.xlsx");
        let tab = crate::tab_from_path_mode(&src, crate::open_mode::OpenMode::Repair).unwrap();
        assert!(
            tab.status.contains("emptied xl/styles.xml"),
            "{}",
            tab.status
        );
        assert!(!load_failed(&tab.status), "{}", tab.status);
        assert!(load_failed(
            "xlsx load error: could not repair: xl/charts/chart1.xml is damaged"
        ));
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

    /// #699: `save-as` resolves the path the dialog would have answered with,
    /// by the app's own extension rules, and refuses what the dialog would
    /// not produce.
    #[test]
    fn save_as_resolves_the_target_and_format_by_the_apps_rules() {
        use crate::Kind;
        let base = Path::new("/sandbox/case");
        let ok = |kind, raw: &str, format: Option<&str>| {
            save_as_target(kind, Some(base), raw, format, true)
        };
        let at = |name: &str| base.join(name);

        assert_eq!(
            ok(Kind::Docx, "out.docx", None),
            Ok((at("out.docx"), "docx"))
        );
        assert_eq!(ok(Kind::Docx, "out.md", None), Ok((at("out.md"), "md")));
        assert_eq!(
            ok(Kind::Docx, "out.markdown", None),
            Ok((at("out.markdown"), "md"))
        );
        assert_eq!(
            ok(Kind::Docx, "out.html", None),
            Ok((at("out.html"), "html"))
        );
        assert_eq!(ok(Kind::Docx, "out", None), Ok((at("out.docx"), "docx")));
        assert_eq!(ok(Kind::Docx, "out", Some("md")), Ok((at("out.md"), "md")));
        assert_eq!(
            ok(Kind::Docx, "out", Some("html")),
            Ok((at("out.docx.html"), "html"))
        );
        assert_eq!(
            ok(Kind::Docx, "out.txt", None),
            Err(DOC_SAVE_FORMATS.into())
        );
        assert_eq!(
            ok(Kind::Docx, "out.xlsx", None),
            Err(DOC_SAVE_FORMATS.into())
        );
        assert_eq!(
            ok(Kind::Docx, "out", Some("pdf")),
            Err(DOC_SAVE_FORMATS.into())
        );
        assert_eq!(
            ok(Kind::Docx, "out.md", Some("docx")),
            Err("'format' docx does not match out.md, which saves as md".into())
        );
        assert_eq!(
            save_as_target(Kind::Docx, Some(base), "out.html", None, false),
            Err("this build cannot write editable HTML (.html)".into())
        );

        assert_eq!(
            ok(Kind::Xlsx, "book.xlsx", None),
            Ok((at("book.xlsx"), "xlsx"))
        );
        assert_eq!(ok(Kind::Xlsx, "book", None), Ok((at("book.xlsx"), "xlsx")));
        // #727: every workbook type, by extension or by format.
        assert_eq!(
            ok(Kind::Xlsx, "book.XLSM", None),
            Ok((at("book.XLSM"), "xlsm"))
        );
        assert_eq!(
            ok(Kind::Xlsx, "book", Some("xltx")),
            Ok((at("book.xltx"), "xltx"))
        );
        assert_eq!(
            ok(Kind::Xlsx, "book.xltm", Some("xlsx")),
            Err("'format' xlsx does not match book.xltm, which saves as xltm".into())
        );
        assert_eq!(
            ok(Kind::Xlsx, "book.csv", None),
            Err(crate::SHEET_SAVE_FORMATS.into())
        );
        assert_eq!(
            ok(Kind::Xlsx, "book", Some("docx")),
            Err(crate::SHEET_SAVE_FORMATS.into())
        );
        // A format is named in any case; an empty one is refused, not taken
        // as a document format on a workbook.
        assert_eq!(
            ok(Kind::Xlsx, "book", Some("XLSX")),
            Ok((at("book.xlsx"), "xlsx"))
        );
        assert_eq!(
            ok(Kind::Docx, "out", Some(" Md ")),
            Ok((at("out.md"), "md"))
        );
        for kind in [Kind::Xlsx, Kind::Docx, Kind::Project] {
            assert!(
                ok(kind, "book", Some(""))
                    .unwrap_err()
                    .contains("'format' must not be empty")
            );
        }
        assert!(
            ok(Kind::Project, "plan", Some("xlsx"))
                .unwrap_err()
                .contains("can only be saved as .yppx or .xml")
        );

        assert_eq!(
            ok(Kind::Project, "plan", None),
            Ok((at("plan.yppx"), "yppx"))
        );
        assert_eq!(
            ok(Kind::Project, "plan.xml", None),
            Ok((at("plan.xml"), "xml"))
        );
        assert_eq!(
            ok(Kind::Project, "plan", Some("xml")),
            Ok((at("plan.xml"), "xml"))
        );
        assert!(
            ok(Kind::Project, "plan.mpp", None)
                .unwrap_err()
                .contains("can only be saved as .yppx or .xml")
        );

        let abs = if cfg!(windows) {
            "C:/elsewhere/x.docx"
        } else {
            "/elsewhere/x.docx"
        };
        assert_eq!(
            save_as_target(Kind::Docx, None, abs, None, true),
            Ok((PathBuf::from(abs), "docx"))
        );
        assert!(
            save_as_target(Kind::Docx, None, "x.docx", None, true)
                .unwrap_err()
                .contains("never been saved")
        );
        assert_eq!(
            ok(Kind::Docx, "  ", None),
            Err("save-as needs a non-empty 'path'".into())
        );
        assert_eq!(
            ok(Kind::Look, "x.docx", None),
            Err("this tab cannot be saved as a file".into())
        );
    }

    /// #699: `fill-drag` refuses before `from` is clicked when a click could
    /// not bring the handle back (an edit, a pointing reference, a cover), so
    /// a refusal changes nothing; a selected chart is left for the click to
    /// clear and refused only if the handle is still hidden after it.
    #[test]
    fn fill_drag_refuses_before_selecting_when_a_click_cannot_help() {
        use crate::HandleHidden::*;
        assert_eq!(fill_press_refusal(false, false, None, true), Ok(()));
        assert_eq!(fill_press_refusal(false, false, None, false), Ok(()));
        for selecting in [true, false] {
            assert_eq!(
                fill_press_refusal(false, false, Some(Editing), selecting),
                Err("the fill handle is not shown: a cell is being edited".into())
            );
            assert_eq!(
                fill_press_refusal(false, false, Some(Pointing), selecting),
                Err("the fill handle is not shown: a reference is being pointed at".into())
            );
            assert_eq!(
                fill_press_refusal(false, true, None, selecting),
                Err("the fill handle is covered: the more-tabs list is open".into())
            );
            assert_eq!(
                fill_press_refusal(true, false, None, selecting),
                Err("the fill handle is not shown: File (backstage) is open".into())
            );
        }
        assert_eq!(
            fill_press_refusal(false, false, Some(ChartSelected), true),
            Ok(())
        );
        assert_eq!(
            fill_press_refusal(false, false, Some(ChartSelected), false),
            Err("the fill handle is not shown: a chart is selected".into())
        );
    }

    /// #699: `fill-drag`'s `from` takes a cell or a range.
    #[test]
    fn range_arg_takes_a_cell_or_a_range() {
        let a = |text: &str| obj(&[("from", s(text))]);
        assert_eq!(range_arg(&a("B4:B5"), "from"), Ok(((3, 1), (4, 1))));
        assert_eq!(range_arg(&a(" C2 "), "from"), Ok(((1, 2), (1, 2))));
        assert!(
            range_arg(&a("B4:"), "from")
                .unwrap_err()
                .contains("is not a cell or a range")
        );
        assert!(range_arg(&obj(&[]), "from").is_err());
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
        assert_eq!(parse_region("gallery"), Ok(Region::Gallery));
        assert!(
            parse_region("gallery:1")
                .unwrap_err()
                .contains("does not take an argument")
        );
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
            Region::TitleTabs,
            Region::TabPrev,
            Region::TabNext,
            Region::TabMore,
            Region::TabMoreItem(19),
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
            Region::ProjectSplit,
            Region::Gallery,
        ] {
            assert_eq!(parse_region(&region_name(r)), Ok(r));
        }
        // A one-cell region prints as the cell, not as a degenerate range.
        assert_eq!(region_name(Region::Cells(2, 1, 2, 1)), "cell:B3");
        assert_eq!(region_name(Region::Cells(0, 0, 4, 2)), "cell:A1:C5");
        assert!(parse_region("gantt:1").is_err());
        assert!(parse_region("project-vbar:1").is_err());
        assert!(parse_region("project-timeline:1").is_err());
        assert!(parse_region("project-split:1").is_err());
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
