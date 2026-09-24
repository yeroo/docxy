//! `yppxy` — a terminal viewer/**editor** for project schedules.
//!
//! The project-management sibling of `xlsxy`/`docxy`: where those sit on
//! `gridcore`/`docxcore`, this is the TUI shell over the pure `projcore` engine
//! — a task outline on the left, a live terminal Gantt chart on the right, and a
//! Critical Path Method reschedule after every edit.
//!
//! It has the same ribbon + File backstage UX as docxy/xlsxy.
//!
//! Usage:
//!   yppxy                              start a new schedule
//!   yppxy <file.(xml|yppx|mpp)>        open MSPDI XML, a .yppx package, or a
//!                                      legacy .mpp (metadata only for now)
//!   yppxy <in> --gantt-md <out.md>     headless: export a Markdown Gantt chart
//!   yppxy <in> --save <out.(yppx|xml)> headless: convert/save and exit

use opccore::fsio::{export_atomic, write_atomic};
use std::path::Path;

use std::io;
use std::process::ExitCode;

mod backstage;
mod control;
mod mcp;
mod ribbon;
mod skill;

use backstage::Backstage;
// Brings `extensions()`/`default_save_name()`/`accent()` etc. into scope
// for the `impl backstage::BackstageHost for App` call site below.
use backstage::BackstageHost as _;
use ribbon::{Act, Ribbon};

use mppread::project::project_from_mpp;
use projcore::datetime::DateTime;
use projcore::editor::{AssignOutcome, Editor, FindOutcome, constraint_hint, parse_duration};
#[cfg(test)]
use projcore::model::Predecessor;
use projcore::model::{LinkType, Project, Task};
use projcore::schedule::{Schedule, schedule};
use projcore::{gantt, mspdi, yppx};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use unicode_width::UnicodeWidthStr;

const APP: &str = "yppxy";

// Schedule colors, shared by the chart and the table's critical marker.
const CRIT: Color = Color::Rgb(217, 100, 44); // amber — the critical path
const ONTRACK: Color = Color::Rgb(58, 170, 154); // teal — has float
const MILESTONE: Color = Color::Rgb(180, 130, 220);
const SUMMARY: Color = Color::Rgb(150, 160, 172); // rollup bars
const WEEKEND: Color = Color::Rgb(90, 100, 110);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--mcp` runs the headless MCP stdio bridge (a client of a running yppxy),
    // not the editor, so handle it before the file-oriented argument parsing.
    if args.iter().any(|a| a == "--mcp") {
        return match mcp::run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("mcp: {e}");
                ExitCode::FAILURE
            }
        };
    }
    // `yppxy install skill` writes the agent SKILL.md and exits.
    if args.first().map(String::as_str) == Some("install")
        && args.get(1).map(String::as_str) == Some("skill")
    {
        return match skill::install() {
            Ok(msg) => {
                println!("{msg}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("install skill: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let parsed = match parse_args(&args) {
        Ok(p) => p,
        Err(m) => {
            eprintln!("{m}");
            eprintln!("usage: yppxy [file.(xml|yppx|mpp)] [--gantt-md <out>] [--save <out>]");
            return ExitCode::from(2);
        }
    };
    if parsed.help {
        println!("usage: yppxy [file.(xml|yppx|mpp)] [--gantt-md <out>] [--save <out>]");
        return ExitCode::SUCCESS;
    }

    // Load the project (or start a fresh one).
    let proj = match &parsed.input {
        Some(path) => match load(path) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{path}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => new_project(),
    };

    // Headless modes: do the job and exit, no TUI.
    if let Some(out) = &parsed.gantt_md {
        let s = schedule(&proj);
        if let Err(e) = write_gantt_md(&proj, &s, parsed.input.as_deref(), out) {
            eprintln!("{out}: {e}");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }
    if let Some(out) = &parsed.save {
        if let Err(e) = save_to(&proj, out) {
            eprintln!("{out}: {e}");
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }

    match run_tui(proj, parsed.input, parsed.vim) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("yppxy: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Args {
    input: Option<String>,
    gantt_md: Option<String>,
    save: Option<String>,
    help: bool,
    vim: bool,
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args {
        input: None,
        gantt_md: None,
        save: None,
        help: false,
        vim: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => out.help = true,
            "--vim" => out.vim = true,
            "--gantt-md" => {
                i += 1;
                out.gantt_md = Some(args.get(i).ok_or("--gantt-md needs a path")?.clone());
            }
            "--save" => {
                i += 1;
                out.save = Some(args.get(i).ok_or("--save needs a path")?.clone());
            }
            s if s.starts_with('-') => return Err(format!("unknown flag: {s}")),
            s => {
                if out.input.is_some() {
                    return Err("only one input file is supported".into());
                }
                out.input = Some(s.to_string());
            }
        }
        i += 1;
    }
    Ok(out)
}

fn load(path: &str) -> Result<Project, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".yppx") {
        yppx::read_yppx(&bytes)
    } else if lower.ends_with(".mpp") {
        project_from_mpp(&bytes)
    } else {
        let xml = String::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
        mspdi::read_mspdi(&xml)
    }
}

fn write_gantt_md(
    proj: &Project,
    sched: &Schedule,
    source: Option<&str>,
    out: &str,
) -> std::io::Result<()> {
    export_atomic(
        source.map(Path::new),
        Path::new(out),
        gantt::to_markdown(proj, sched).as_bytes(),
    )
}

const SAVE_FORMAT_ERROR: &str = "Project schedules can only be saved as .yppx or .xml (MSPDI)";

fn save_to(proj: &Project, path: &str) -> Result<(), String> {
    let extension = Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");
    if extension.eq_ignore_ascii_case("yppx") {
        write_atomic(Path::new(path), &yppx::write_yppx(proj)).map_err(|e| e.to_string())
    } else if extension.eq_ignore_ascii_case("xml") {
        write_atomic(Path::new(path), mspdi::write_mspdi(proj).as_bytes())
            .map_err(|e| e.to_string())
    } else {
        Err(SAVE_FORMAT_ERROR.into())
    }
}

/// A fresh schedule: one task so the chart has something to show.
fn new_project() -> Project {
    let mut p = projcore::editor::untitled_project();
    p.tasks.push(Task {
        uid: 1,
        id: 1,
        name: "New task".into(),
        outline_level: 1,
        duration_min: 480,
        ..Task::default()
    });
    p
}

/// Path to the view-prefs file: `$XDG_CONFIG_HOME/yppxy/prefs` (or `~/.config`).
fn prefs_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("yppxy").join("prefs"))
}

/// Load the persisted theme preference (light/dark); defaults to dark.
fn load_theme_pref() -> bool {
    let Some(p) = prefs_path() else { return false };
    let Ok(text) = std::fs::read_to_string(p) else {
        return false;
    };
    text.lines()
        .find_map(|l| l.strip_prefix("theme="))
        .map(|v| v.trim() == "light")
        .unwrap_or(false)
}

/// Persist the theme preference. Best-effort — failures are ignored.
fn save_theme_pref(light: bool) {
    let Some(p) = prefs_path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        p,
        format!("theme={}\n", if light { "light" } else { "dark" }),
    );
}

fn window_title(path: &Option<String>, dirty: bool) -> String {
    let name = path
        .as_deref()
        .map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p).to_string())
        .unwrap_or_else(|| "untitled".into());
    format!("{}{APP} - {name}", if dirty { "* " } else { "" })
}

// ---- app state --------------------------------------------------------------

enum PromptKind {
    Rename,
    Duration,
    AddPredecessor,
    SaveAs,
    Find,
    VimCommand,
    Constraint,
    Assign,
}

/// What a confirmed (Yes) modal should do.
#[derive(Clone, PartialEq, Eq, Debug)]
enum ConfirmAction {
    Exit,
}

// The Yes/No modal itself lives in `backstage::Confirm<ConfirmAction>` (shared
// across all apps); yppxy only supplies the action carried on Yes.

struct Prompt {
    kind: PromptKind,
    label: String,
    buf: String,
}

struct App {
    ed: Editor,
    path: Option<String>,
    top: usize,   // first visible task row
    hscroll: i64, // gantt horizontal scroll in days from the earliest displayed start
    prompt: Option<Prompt>,
    status: String,
    quit: bool,
    /// The shared Yes/No exit confirmation modal, open while asking whether
    /// to quit (Ctrl+Q / File ▸ Exit).
    confirm: Option<backstage::Confirm<ConfirmAction>>,
    // ribbon + backstage + chrome
    ribbon: Ribbon,
    rfocus: ribbon::Focus,
    backstage: Option<Backstage>,
    light: bool,
    /// When launched with no file, a welcome/start screen overlays everything
    /// until the user picks New/Open/Quit.
    start_screen: bool,
    /// The shared centered start card (item list, selection, click rects).
    start: backstage::Start,
    // vim mode
    vim: bool,
    // geometry recorded during draw for mouse hit-testing
    list_y0: u16,     // absolute y of the first task row
    list_left_w: u16, // width of the task pane (left of the gantt)
    gantt_x0: u16,    // absolute x where the gantt inner area begins
}

const RIBBON_H: u16 = 7; // tab strip (1) + body (6: border, 2 rows, separator, titles, border)

impl App {
    fn new(proj: Project, path: Option<String>, vim: bool) -> App {
        let start_screen = path.is_none();
        App {
            ed: Editor::new(proj),
            path,
            top: 0,
            hscroll: 0,
            prompt: None,
            status: String::new(),
            quit: false,
            confirm: None,
            ribbon: Ribbon::new(),
            rfocus: ribbon::Focus::None,
            backstage: None,
            light: load_theme_pref(),
            start_screen,
            start: backstage::Start::new(
                "yppxy",
                vec![
                    backstage::StartItem {
                        label: "New schedule".to_string(),
                        desc: Some("Start a fresh blank schedule".to_string()),
                    },
                    backstage::StartItem {
                        label: "Open a project…".to_string(),
                        desc: Some("Browse for .xml · .yppx · .mpp".to_string()),
                    },
                    backstage::StartItem {
                        label: "Quit".to_string(),
                        desc: Some("Exit yppxy".to_string()),
                    },
                ],
                Color::Yellow,
            ),
            list_y0: 0,
            list_left_w: 0,
            gantt_x0: 0,
            vim,
        }
    }

    fn undo(&mut self) {
        self.status = if self.ed.undo() {
            "Undo"
        } else {
            "Nothing to undo"
        }
        .into();
    }

    fn redo(&mut self) {
        self.status = if self.ed.redo() {
            "Redo"
        } else {
            "Nothing to redo"
        }
        .into();
    }

    fn open_backstage(&mut self) {
        let dir = self
            .path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).parent().map(|d| d.to_path_buf()))
            .filter(|d| !d.as_os_str().is_empty())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        self.backstage = Some(Backstage::open(dir, self.extensions()));
        self.rfocus = ribbon::Focus::None;
    }

    /// Act on a chosen welcome-screen item. Returns true to quit.
    fn start_choose(&mut self, idx: usize) -> bool {
        self.start_screen = false;
        match idx {
            0 => self.new_schedule(),
            1 => self.open_backstage(),
            _ => return true, // Quit
        }
        false
    }

    /// Open the Exit confirmation modal (used by Ctrl+Q and File ▸ Exit).
    fn request_exit(&mut self) {
        self.backstage = None;
        let prompt = if self.ed.dirty() {
            "Exit yppxy? Unsaved changes will be lost."
        } else {
            "Exit yppxy?"
        };
        self.confirm = Some(backstage::Confirm::new(
            prompt,
            ConfirmAction::Exit,
            Color::Yellow,
        ));
    }

    /// Act on the shared dialog's outcome.
    fn apply_confirm(&mut self, outcome: backstage::ConfirmOutcome<ConfirmAction>) {
        match outcome {
            backstage::ConfirmOutcome::Pending => {}
            backstage::ConfirmOutcome::Cancelled => self.confirm = None,
            backstage::ConfirmOutcome::Confirmed(action) => {
                self.confirm = None;
                match action {
                    ConfirmAction::Exit => self.quit = true,
                }
            }
        }
    }

    /// Route a key to the Yes/No modal.
    fn confirm_key(&mut self, key: KeyEvent) {
        let Some(c) = self.confirm.as_mut() else {
            return;
        };
        let outcome = c.key(key);
        self.apply_confirm(outcome);
    }

    /// Route a click to the Yes/No modal.
    fn confirm_mouse(&mut self, x: u16, y: u16) {
        let Some(c) = self.confirm.as_mut() else {
            return;
        };
        let outcome = c.mouse(x, y);
        self.apply_confirm(outcome);
    }

    fn toggle_milestone(&mut self) {
        if let Some(uid) = self.ed.selected_uid() {
            if let Err(message) = self.ed.toggle_milestone(uid) {
                self.status = message;
            }
        }
    }

    fn theme_toggle(&mut self) {
        self.light = !self.light;
        save_theme_pref(self.light);
    }

    /// Find the next task whose name contains `query` (case-insensitive),
    /// searching from just after the current selection and wrapping around.
    fn find(&mut self, query: &str) {
        match self.ed.find(query) {
            FindOutcome::Inactive => {}
            FindOutcome::Found(_) => {
                self.status = format!("Found '{}'  (F3 next)", self.ed.find_query())
            }
            FindOutcome::NotFound => {
                self.status = format!("No task matching '{}'", self.ed.find_query())
            }
        }
    }

    /// Set a date constraint on the selected task from text like
    /// `SNET 2026-03-05`, `MSO 2026-03-05`, or `none` / `asap`.
    fn set_constraint(&mut self, text: &str) {
        if let Some(uid) = self.ed.selected_uid() {
            self.status = match self.ed.set_constraint(uid, text) {
                Ok(()) => format!(
                    "Constraint set: {}",
                    text.split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase()
                ),
                Err(message) => message,
            };
        }
    }

    /// Assign a resource (by name, created on first use) to the selected task.
    /// An empty name clears the task's assignments.
    fn assign_resource(&mut self, name: &str) {
        let Some(uid) = self.ed.selected_uid() else {
            return;
        };
        let name = name.trim();
        match self.ed.assign_resource(uid, name) {
            Ok(AssignOutcome::Assigned) => self.status = format!("Assigned {name}"),
            Ok(AssignOutcome::Cleared) => self.status = "Cleared the task's resources".into(),
            Ok(AssignOutcome::AlreadyAssigned) => {
                self.status = format!("{name} is already assigned")
            }
            Ok(AssignOutcome::NothingToClear) => {}
            Err(message) => self.status = message,
        }
    }

    /// Snapshot the current computed schedule as the baseline (the saved plan).
    fn set_baseline(&mut self) {
        self.ed.set_baseline();
        self.status = "Baseline set — variance now shows in the header".into();
    }

    /// Run a vim `:` command line (`w`, `q`, `wq`/`x`, `q!`, `e <path>`).
    fn vim_run(&mut self, cmd: &str) {
        match cmd.trim() {
            "w" => self.save(),
            "q" => {
                if self.ed.dirty() {
                    self.status = "Unsaved changes — :q! to force, or :wq to save".into();
                } else {
                    self.quit = true;
                }
            }
            "q!" => self.quit = true,
            "wq" | "x" => {
                self.save();
                if !self.ed.dirty() {
                    self.quit = true;
                }
            }
            other if other.starts_with("e ") => {
                let path = other[2..].trim().to_string();
                if !path.is_empty() {
                    self.open_file(&path);
                }
            }
            "" => {}
            other => self.status = format!("Not a command: :{other}"),
        }
    }

    /// Background for the selected row, theme-aware.
    fn sel_bg(&self) -> Color {
        if self.light {
            Color::Rgb(208, 218, 230)
        } else {
            Color::Rgb(38, 48, 58)
        }
    }

    /// Run a ribbon command.
    fn apply_act(&mut self, act: Act) {
        self.status.clear();
        match act {
            Act::AddTask => self.add_task(),
            Act::DeleteTask => self.delete_task(),
            Act::Milestone => self.toggle_milestone(),
            Act::Indent => self.indent(1),
            Act::Outdent => self.indent(-1),
            Act::Rename => {
                if let Some(t) = self.ed.project().tasks.get(self.ed.sel()) {
                    self.prompt = Some(Prompt {
                        kind: PromptKind::Rename,
                        label: "Rename".into(),
                        buf: t.name.clone(),
                    });
                }
            }
            Act::Duration => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::Duration,
                    label: "Duration".into(),
                    buf: String::new(),
                });
            }
            Act::AddLink => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::AddPredecessor,
                    label: "Predecessor ID".into(),
                    buf: String::new(),
                });
            }
            Act::Constraint => {
                let cur = self
                    .ed
                    .project()
                    .tasks
                    .get(self.ed.sel())
                    .map(constraint_hint)
                    .unwrap_or_default();
                self.prompt = Some(Prompt {
                    kind: PromptKind::Constraint,
                    label: "Constraint".into(),
                    buf: cur,
                });
            }
            Act::Baseline => self.set_baseline(),
            Act::Assign => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::Assign,
                    label: "Assign resource".into(),
                    buf: String::new(),
                });
            }
            Act::ClearResources => self.assign_resource(""),
            Act::ExportGantt => self.export_md(),
            Act::ScrollLeft => self.hscroll -= 1,
            Act::ScrollRight => self.hscroll += 1,
            Act::GoToStart => self.hscroll = 0,
            Act::ThemeToggle => self.theme_toggle(),
            Act::Level => self.toggle_level(),
            Act::Save => self.save(),
            Act::SaveAs => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::SaveAs,
                    label: "Save as".into(),
                    buf: self.path.clone().unwrap_or_default(),
                });
            }
            Act::Todo(name) => self.status = format!("{name} — not implemented yet"),
        }
    }

    fn toggle_level(&mut self) {
        self.ed.toggle_level();
        self.status = if self.ed.leveled() {
            "Resource leveling ON — bars delayed to fit resource capacity"
        } else {
            "Resource leveling OFF"
        }
        .into();
    }

    /// Displayed start of a task: leveled if leveling is on, else CPM early start.
    fn disp_start(&self, uid: i32) -> Option<DateTime> {
        self.ed.disp_start(uid)
    }

    fn disp_finish(&self, uid: i32) -> Option<DateTime> {
        self.ed.disp_finish(uid)
    }

    // ---- edits ----

    fn add_task(&mut self) {
        match self.ed.add_task(self.ed.selected_uid(), "New task", 480) {
            Ok(at) => self.ed.select(at),
            Err(message) => self.status = message,
        }
    }

    fn delete_task(&mut self) {
        if let Some(uid) = self.ed.selected_uid() {
            if let Err(message) = self.ed.delete_task(uid) {
                self.status = message;
            }
        }
    }

    fn indent(&mut self, delta: i32) {
        if let Some(uid) = self.ed.selected_uid() {
            if let Err(message) = self.ed.indent(uid, delta) {
                self.status = message;
            }
        }
    }

    fn set_duration(&mut self, text: &str) {
        // Keep invalid-input feedback even when there is no selected row.
        if let Some(uid) = self.ed.selected_uid() {
            if let Err(message) = self.ed.set_duration(uid, text) {
                self.status = message;
            }
        } else if parse_duration(text, self.ed.project()).is_none() {
            self.status = format!("Couldn't read duration '{text}' (try 3d, 4h, 2w)");
        }
    }

    fn rename(&mut self, text: &str) {
        if let Some(uid) = self.ed.selected_uid() {
            if let Err(message) = self.ed.rename(uid, text) {
                self.status = message;
            }
        }
    }

    fn add_predecessor(&mut self, text: &str) {
        let Ok(pred) = text.trim().parse::<i32>() else {
            self.status = "Predecessor must be a task ID (number)".into();
            return;
        };
        let Some(uid) = self.ed.selected_uid() else {
            self.status = "No task selected".into();
            return;
        };
        if let Err(message) = self.ed.add_predecessor(uid, pred, LinkType::FinishStart, 0) {
            self.status = message;
        }
    }

    fn save(&mut self) {
        match self.path.clone() {
            Some(p) => match save_to(self.ed.project(), &p) {
                Ok(()) => {
                    self.ed.mark_saved();
                    self.status = format!("Saved {p}");
                }
                Err(e) => self.status = format!("Save failed: {e}"),
            },
            None => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::SaveAs,
                    label: "Save as".into(),
                    buf: String::new(),
                });
            }
        }
    }

    fn export_md(&mut self) {
        let out = self
            .path
            .as_deref()
            .map(|p| format!("{}.md", p.rsplit_once('.').map(|(a, _)| a).unwrap_or(p)))
            .unwrap_or_else(|| "schedule.md".into());
        match write_gantt_md(
            self.ed.project(),
            self.ed.schedule(),
            self.path.as_deref(),
            &out,
        ) {
            Ok(()) => self.status = format!("Exported Gantt to {out}"),
            Err(e) => self.status = format!("Export failed: {e}"),
        }
    }

    /// Load a project from disk into the app, replacing the current one.
    fn open_file(&mut self, path: &str) {
        match load(path) {
            Ok(p) => {
                self.ed.replace_project(p);
                self.path = Some(path.to_string());
                self.top = 0;
                self.hscroll = 0;
                self.backstage = None;
                self.start_screen = false;
                let is_mpp = path.to_ascii_lowercase().ends_with(".mpp");
                self.status = if is_mpp && !self.ed.project().tasks.is_empty() {
                    format!(
                        "Opened {path} — {} task names decoded (.mpp dates/links pending)",
                        self.ed.project().tasks.len()
                    )
                } else if is_mpp {
                    format!("Opened {path} — .mpp metadata only (no task table found)")
                } else {
                    format!("Opened {path}")
                };
            }
            Err(e) => self.status = format!("Open failed: {e}"),
        }
    }

    /// Start a fresh blank schedule (one task), discarding any current file
    /// binding. Shared by the File ▸ New backstage item and the welcome
    /// screen's "New schedule" choice.
    fn new_schedule(&mut self) {
        self.ed.replace_project(new_project());
        self.path = None;
        self.top = 0;
        self.hscroll = 0;
        self.status = "New schedule".into();
    }

    /// Act on a [`backstage::BackstageEvent`] returned by the backstage's own
    /// `key`/`mouse` handlers. Shared by `backstage_key` and `bs_mouse`.
    fn apply_backstage_event(&mut self, ev: backstage::BackstageEvent) {
        use backstage::BackstageEvent;
        match ev {
            BackstageEvent::None => {}
            BackstageEvent::Close => self.backstage = None,
            BackstageEvent::New => {
                self.new_schedule();
                self.backstage = None;
            }
            BackstageEvent::Open(p) => self.open_file(&p.to_string_lossy()),
            BackstageEvent::Save => {
                self.backstage = None;
                self.save();
            }
            BackstageEvent::SaveAs { dir, name } => self.commit_save_as(dir, name),
            BackstageEvent::Export => {
                self.backstage = None;
                self.export_md();
            }
            BackstageEvent::Exit => self.request_exit(),
        }
    }

    /// Route a key to the backstage.
    fn backstage_key(&mut self, key: KeyEvent) {
        let mut bs = self.backstage.take();
        let ev = bs
            .as_mut()
            .map(|b| b.key(key, self))
            .unwrap_or(backstage::BackstageEvent::None);
        self.backstage = bs;
        self.apply_backstage_event(ev);
    }

    /// Route a left-click inside the File backstage. Row 0 is the ribbon tab
    /// strip (drawn over the backstage) and is handled here directly; every
    /// other row is delegated to `backstage::Backstage::mouse`.
    fn bs_mouse(&mut self, x: u16, y: u16) {
        if y == 0 {
            match self.ribbon.hit(x, 0, false) {
                ribbon::Hit::Tab(i) if !self.ribbon.tab_is_file(i) => {
                    self.backstage = None;
                    self.ribbon.set_active(i);
                    self.rfocus = ribbon::Focus::Tab(i);
                }
                _ => self.backstage = None,
            }
            return;
        }
        let mut bs = self.backstage.take();
        let ev = bs
            .as_mut()
            .map(|b| b.mouse(x, y, self))
            .unwrap_or(backstage::BackstageEvent::None);
        self.backstage = bs;
        self.apply_backstage_event(ev);
    }

    /// Write the project to `dir/name` (defaulting to `.yppx` when the typed
    /// name carries no extension), then make it the current
    /// file and close the backstage.
    fn commit_save_as(&mut self, dir: std::path::PathBuf, name: String) {
        let name = name.trim();
        if name.is_empty() {
            self.status = "Save As — type a file name first.".to_string();
            return;
        }
        let fname = if Path::new(name).extension().is_none() {
            format!("{name}.yppx")
        } else {
            name.to_string()
        };
        let path = dir.join(&fname).to_string_lossy().into_owned();
        self.backstage = None;
        match save_to(self.ed.project(), &path) {
            Ok(()) => {
                self.ed.mark_saved();
                self.status = format!("Saved {path}");
                self.path = Some(path);
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }
}

/// A few summary lines for the backstage preview / Info pane.
fn project_preview(proj: &Project, sched: &Schedule) -> Vec<String> {
    let fin = sched.project_finish.parts();
    let start = sched.project_start.parts();
    let leaves = proj.tasks.iter().filter(|t| !t.summary).count();
    let crit = proj
        .tasks
        .iter()
        .filter(|t| !t.summary && sched.get(t.uid).is_some_and(|r| r.critical))
        .count();
    let mut out = vec![
        format!(
            "Project: {}",
            if proj.name.is_empty() {
                "Untitled"
            } else {
                &proj.name
            }
        ),
        format!(
            "Start:   {:04}-{:02}-{:02}",
            start.year, start.month, start.day
        ),
        format!("Finish:  {:04}-{:02}-{:02}", fin.year, fin.month, fin.day),
        format!("Tasks:   {} ({crit} critical)", leaves),
        String::new(),
    ];
    for t in proj.tasks.iter().take(16) {
        let indent = "  ".repeat(t.outline_level.saturating_sub(1) as usize);
        let bullet = if t.summary {
            "▾"
        } else if t.is_milestone() {
            "◆"
        } else {
            "•"
        };
        out.push(format!("{indent}{bullet} {}", t.name));
    }
    if proj.tasks.len() > 16 {
        out.push(format!("  … {} more", proj.tasks.len() - 16));
    }
    out
}

/// Format-specific content the shared File backstage needs from yppxy: only
/// project files are listed/opened, the Save As default is the current
/// file's name, the preview/Info panes render a project summary, and the
/// accent matches yppxy's ribbon (yellow).
impl backstage::BackstageHost for App {
    fn extensions(&self) -> &'static [&'static str] {
        &["xml", "yppx", "mpp"]
    }

    fn default_save_name(&self) -> String {
        self.path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled.yppx".to_string())
    }

    /// Render a quick summary of the highlighted project.
    fn preview_lines(&self, path: &std::path::Path, _width: usize) -> Vec<String> {
        match load(&path.to_string_lossy()) {
            Ok(p) => {
                let s = schedule(&p);
                project_preview(&p, &s)
            }
            Err(e) => vec![format!("(cannot preview: {e})")],
        }
    }

    fn info_lines(&self) -> Vec<Line<'static>> {
        project_preview(self.ed.project(), self.ed.schedule())
            .into_iter()
            .map(Line::from)
            .collect()
    }

    fn accent(&self) -> Color {
        Color::Yellow
    }
}

// ---- TUI loop ---------------------------------------------------------------

fn run_tui(proj: Project, path: Option<String>, vim: bool) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(proj, path, vim);
    let mut title = String::new();

    // Bring up the agent control surface. Best-effort: if the config directory or
    // the loopback bind fails, the editor runs exactly as before, just without a
    // control channel. `ctl_server` is held for the whole session — its Drop
    // removes the discovery file.
    let ctl_instance = ctlcore::instance_name("yppxy");
    let (ctl_server, ctl_rx) = match ctlcore::config_ctl_dir("yppxy") {
        Some(dir) => match ctlcore::serve(&dir, &ctl_instance) {
            Ok((srv, rx)) => (Some(srv), Some(rx)),
            Err(_) => (None, None),
        },
        None => (None, None),
    };

    // One message stream drives the loop: terminal input (read on its own thread
    // so the loop can block cheaply) and control requests. The main thread stays
    // the sole owner of the project, so applying a request needs no locking.
    enum Msg {
        Term(Event),
        Ctl(ctlcore::Request),
    }
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    {
        let tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("yppxy-input".into())
            .spawn(move || {
                while let Ok(ev) = event::read() {
                    if tx.send(Msg::Term(ev)).is_err() {
                        break;
                    }
                }
            });
    }
    if let Some(ctl_rx) = ctl_rx {
        let tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("yppxy-ctl".into())
            .spawn(move || {
                for req in ctl_rx {
                    if tx.send(Msg::Ctl(req)).is_err() {
                        break;
                    }
                }
            });
    }
    drop(tx); // only the reader/forwarder threads keep the channel open now

    let res = loop {
        // Keep the terminal window title in sync: [* ]yppxy - filename.
        let want = window_title(&app.path, app.ed.dirty());
        if want != title {
            let _ = execute!(terminal.backend_mut(), SetTitle(&want));
            title = want;
        }
        if let Err(e) = terminal.draw(|f| draw(f, &mut app)) {
            break Err(e);
        }
        // Block until something arrives, then drain the queue so a burst of
        // agent edits collapses into a single repaint.
        let mut next = match rx.recv() {
            Ok(m) => Some(m),
            Err(_) => break Ok(()), // every input source is gone
        };
        while let Some(msg) = next.take() {
            match msg {
                Msg::Term(Event::Key(k)) if k.kind == KeyEventKind::Press => on_key(&mut app, k),
                Msg::Term(Event::Mouse(m)) => on_mouse(&mut app, m),
                Msg::Term(_) => {}
                Msg::Ctl(req) => match control::dispatch(&mut app, &req.verb, &req.args) {
                    Ok(result) => req.reply_ok(result),
                    Err(e) => req.reply_err(e),
                },
            }
            if app.quit {
                break;
            }
            next = rx.try_recv().ok();
        }
        if app.quit {
            break Ok(());
        }
    };
    drop(ctl_server); // remove the discovery file

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    res
}

fn on_mouse(app: &mut App, m: MouseEvent) {
    // A modal confirmation (Exit) owns the whole screen while open — before
    // the welcome screen or backstage, so it can appear over either.
    if app.confirm.is_some() {
        if m.kind == MouseEventKind::Down(MouseButton::Left) {
            app.confirm_mouse(m.column, m.row);
        }
        return;
    }
    // The welcome screen owns the whole terminal; handle its clicks here so
    // nothing leaks to the hidden schedule behind it. Hovering highlights an
    // item, clicking activates it.
    if app.start_screen {
        let ev = app.start.mouse(m.column, m.row);
        if m.kind == MouseEventKind::Down(MouseButton::Left) {
            if let backstage::StartEvent::Choose(i) = ev {
                if app.start_choose(i) {
                    app.quit = true;
                }
            }
        }
        return;
    }
    // The File backstage is a full-screen surface: it owns the mouse while
    // open, so clicks/scroll never leak through to the schedule underneath.
    if app.backstage.is_some() {
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => app.bs_mouse(m.column, m.row),
            MouseEventKind::ScrollDown => {
                if let Some(b) = app.backstage.as_mut() {
                    b.scroll_preview(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(b) = app.backstage.as_mut() {
                    b.scroll_preview(-3);
                }
            }
            _ => {}
        }
        return;
    }
    let (x, y) = (m.column, m.row);
    match m.kind {
        MouseEventKind::ScrollDown => {
            if x >= app.gantt_x0 {
                app.hscroll += 2;
            } else if app.ed.sel() + 1 < app.ed.project().tasks.len() {
                app.ed.select(app.ed.sel() + 1);
            }
        }
        MouseEventKind::ScrollUp => {
            if x >= app.gantt_x0 {
                app.hscroll -= 2;
            } else {
                app.ed.select(app.ed.sel().saturating_sub(1));
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // Ribbon area (top RIBBON_H rows).
            if y < RIBBON_H {
                match app.ribbon.hit(x, y, true) {
                    ribbon::Hit::Tab(i) => {
                        if app.ribbon.tab_is_file(i) {
                            app.open_backstage();
                        } else {
                            app.ribbon.set_active(i);
                            app.rfocus = ribbon::Focus::Tab(i);
                        }
                    }
                    ribbon::Hit::Button(act) => {
                        app.apply_act(act);
                        app.rfocus = ribbon::Focus::None;
                    }
                    ribbon::Hit::Outside => {}
                }
                return;
            }
            // Click a task row to select it.
            if x < app.list_left_w && y >= app.list_y0 {
                let idx = app.top + (y - app.list_y0) as usize;
                if idx < app.ed.project().tasks.len() {
                    app.ed.select(idx);
                }
            }
        }
        _ => {}
    }
}

fn on_key(app: &mut App, k: KeyEvent) {
    // Prompt mode swallows keys until Enter/Esc.
    if let Some(mut prompt) = app.prompt.take() {
        match k.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                let text = prompt.buf.clone();
                match prompt.kind {
                    PromptKind::Rename => app.rename(&text),
                    PromptKind::Duration => app.set_duration(&text),
                    PromptKind::AddPredecessor => app.add_predecessor(&text),
                    PromptKind::Find => app.find(&text),
                    PromptKind::VimCommand => app.vim_run(&text),
                    PromptKind::Constraint => app.set_constraint(&text),
                    PromptKind::Assign => app.assign_resource(&text),
                    PromptKind::SaveAs => {
                        if !text.trim().is_empty() {
                            app.path = Some(text.trim().to_string());
                            app.save();
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                prompt.buf.pop();
                app.prompt = Some(prompt);
            }
            KeyCode::Char(c) => {
                prompt.buf.push(c);
                app.prompt = Some(prompt);
            }
            _ => app.prompt = Some(prompt),
        }
        return;
    }

    // A modal confirmation (Exit) owns all keys while open — before the
    // welcome screen or backstage, so it can appear over either.
    if app.confirm.is_some() {
        app.confirm_key(k);
        return;
    }

    // Modal surfaces take keys before the editor.
    if app.start_screen {
        start_key(app, k);
        return;
    }
    if app.backstage.is_some() {
        app.backstage_key(k);
        return;
    }
    if app.rfocus != ribbon::Focus::None {
        ribbon_key(app, k);
        return;
    }

    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    app.status.clear();

    // Alt+F opens the File backstage (docxy/xlsxy parity).
    if alt && matches!(k.code, KeyCode::Char('f') | KeyCode::Char('F')) {
        app.open_backstage();
        return;
    }

    // Ctrl combinations first, so plain-letter shortcuts don't shadow them.
    if ctrl {
        match k.code {
            KeyCode::Char('s') => app.save(),
            KeyCode::Char('e') => app.export_md(),
            KeyCode::Char('z') => app.undo(),
            KeyCode::Char('y') | KeyCode::Char('r') => app.redo(),
            KeyCode::Char('f') => {
                app.prompt = Some(Prompt {
                    kind: PromptKind::Find,
                    label: "Find".into(),
                    buf: String::new(),
                });
            }
            KeyCode::Char('q') => app.request_exit(), // Ctrl+Q: confirm even if dirty
            _ => {}
        }
        return;
    }

    match k.code {
        // Any quit key opens the shared confirm (which warns about unsaved
        // changes) — consistent with Ctrl+Q and the other apps.
        KeyCode::Char('q') | KeyCode::Char('Q') => app.request_exit(),
        KeyCode::Up | KeyCode::Char('k') => app.ed.select(app.ed.sel().saturating_sub(1)),
        KeyCode::Down | KeyCode::Char('j') => {
            if app.ed.sel() + 1 < app.ed.project().tasks.len() {
                app.ed.select(app.ed.sel() + 1);
            }
        }
        KeyCode::Home | KeyCode::Char('g') => app.ed.select(0),
        KeyCode::End | KeyCode::Char('G') => app
            .ed
            .select(app.ed.project().tasks.len().saturating_sub(1)),
        KeyCode::Left | KeyCode::Char('h') => app.hscroll -= 1,
        KeyCode::Right | KeyCode::Char('l') => app.hscroll += 1,
        KeyCode::Char('n') | KeyCode::Insert => app.add_task(),
        KeyCode::Delete | KeyCode::Char('x') => app.delete_task(),
        KeyCode::Tab | KeyCode::Char('>') => app.indent(1),
        KeyCode::BackTab | KeyCode::Char('<') => app.indent(-1),
        KeyCode::Enter | KeyCode::F(2) => {
            if let Some(t) = app.ed.project().tasks.get(app.ed.sel()) {
                app.prompt = Some(Prompt {
                    kind: PromptKind::Rename,
                    label: "Rename".into(),
                    buf: t.name.clone(),
                });
            }
        }
        KeyCode::Char('d') => {
            app.prompt = Some(Prompt {
                kind: PromptKind::Duration,
                label: "Duration".into(),
                buf: String::new(),
            });
        }
        KeyCode::Char('p') => {
            app.prompt = Some(Prompt {
                kind: PromptKind::AddPredecessor,
                label: "Predecessor ID".into(),
                buf: String::new(),
            });
        }
        KeyCode::Char('c') => {
            let cur = app
                .ed
                .project()
                .tasks
                .get(app.ed.sel())
                .map(constraint_hint)
                .unwrap_or_default();
            app.prompt = Some(Prompt {
                kind: PromptKind::Constraint,
                label: "Constraint".into(),
                buf: cur,
            });
        }
        KeyCode::Char('b') => app.set_baseline(),
        KeyCode::Char('L') => app.toggle_level(),
        KeyCode::Char('a') => {
            app.prompt = Some(Prompt {
                kind: PromptKind::Assign,
                label: "Assign resource".into(),
                buf: String::new(),
            });
        }
        KeyCode::F(3) => app.find(""), // repeat the last search
        KeyCode::F(9) => app.rfocus = ribbon::Focus::Tab(app.ribbon.active_tab()),
        // Vim niceties (only when launched with --vim).
        KeyCode::Char(':') if app.vim => {
            app.prompt = Some(Prompt {
                kind: PromptKind::VimCommand,
                label: ":".into(),
                buf: String::new(),
            });
        }
        KeyCode::Char('u') if app.vim => app.undo(),
        KeyCode::Char('/') if app.vim => {
            app.prompt = Some(Prompt {
                kind: PromptKind::Find,
                label: "/".into(),
                buf: String::new(),
            });
        }
        _ => {}
    }
}

// ---- start screen, ribbon, backstage key handling ---------------------------

/// Route a key on the welcome screen. Up/Down/Tab move the highlight, a
/// number key or Enter picks it, Esc/`q` quits.
fn start_key(app: &mut App, k: KeyEvent) {
    match app.start.key(k) {
        backstage::StartEvent::Choose(i) => {
            if app.start_choose(i) {
                app.quit = true;
            }
        }
        backstage::StartEvent::Quit => app.quit = true,
        backstage::StartEvent::None => {}
    }
}

fn ribbon_key(app: &mut App, k: KeyEvent) {
    use ribbon::{Dir, Focus};
    match k.code {
        KeyCode::Esc => app.rfocus = Focus::None,
        KeyCode::Left => step_ribbon(app, Dir::Left),
        KeyCode::Right => step_ribbon(app, Dir::Right),
        KeyCode::Up => step_ribbon(app, Dir::Up),
        KeyCode::Down => step_ribbon(app, Dir::Down),
        KeyCode::Enter => match app.rfocus {
            Focus::Tab(t) => {
                if app.ribbon.tab_is_file(t) {
                    app.open_backstage();
                } else {
                    app.ribbon.set_active(t);
                    app.rfocus = app.ribbon.enter_body();
                }
            }
            Focus::Button(_) => {
                if let Some((act, _)) = app.ribbon.focus_act(app.rfocus) {
                    app.apply_act(act);
                    app.rfocus = Focus::None;
                }
            }
            Focus::None => {}
        },
        _ => {}
    }
}

/// Move ribbon focus, keeping the active tab in sync when landing on a tab.
fn step_ribbon(app: &mut App, dir: ribbon::Dir) {
    let nf = app.ribbon.nav(app.rfocus, dir);
    if let ribbon::Focus::Tab(t) = nf {
        if !app.ribbon.tab_is_file(t) {
            app.ribbon.set_active(t);
        }
    }
    app.rfocus = nf;
}

// ---- drawing ----------------------------------------------------------------

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    // A confirmation modal owns the whole screen — no content behind it.
    if let Some(c) = app.confirm.as_mut() {
        f.render_widget(Clear, area);
        c.draw(f, area);
        return;
    }
    // The welcome screen overlays everything when launched with no file.
    if app.start_screen {
        f.render_widget(Clear, area);
        app.start.draw(f, area);
        return;
    }
    // The File backstage takes over the whole screen.
    if app.backstage.is_some() {
        // `backstagecore::draw` clears the full frame and renders the menu +
        // content below row 0 — draw it first, then paint the ribbon tab
        // strip (File highlighted) over row 0 last so it isn't wiped out.
        let mut bs = app.backstage.take();
        if let Some(b) = bs.as_mut() {
            backstage::draw(f, area, b, app);
        }
        app.backstage = bs;
        // Keep the ribbon tab headers visible: clicking another tab leaves
        // the backstage, and clicking File closes it back to the schedule —
        // so the panel can be dismissed entirely with the mouse.
        let dim = Style::default().add_modifier(Modifier::DIM);
        let mut tabline = app.ribbon.render_tabs_as(0); // 0 = File
        tabline
            .spans
            .push(Span::styled("   (click a tab or Esc to leave)", dim));
        let row0 = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        f.render_widget(Paragraph::new(tabline), row0);
        if app.prompt.is_some() {
            draw_prompt(f, area, app);
        }
        return;
    }

    // Reflect toggle state (theme, leveling) in the ribbon.
    let mut toggles = Vec::new();
    if app.light {
        toggles.push(Act::ThemeToggle);
    }
    if app.ed.leveled() {
        toggles.push(Act::Level);
    }
    app.ribbon.set_toggles(toggles);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // ribbon tab strip
            Constraint::Length(6), // ribbon body (border, 2 rows, separator, titles, border)
            Constraint::Length(1), // project header
            Constraint::Min(3),    // tasks | gantt
            Constraint::Length(1), // status / hint
        ])
        .split(area);
    f.render_widget(Paragraph::new(app.ribbon.render_tabs(app.rfocus)), rows[0]);
    f.render_widget(Paragraph::new(app.ribbon.render_body(app.rfocus)), rows[1]);
    draw_header(f, rows[2], app);
    draw_body(f, rows[3], app);
    draw_status(f, rows[4], app);
    if app.prompt.is_some() {
        draw_prompt(f, area, app);
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let fin = app.ed.schedule().project_finish.parts();
    let crit = app
        .ed
        .project()
        .tasks
        .iter()
        .filter(|t| app.ed.schedule().get(t.uid).is_some_and(|r| r.critical) && !t.summary)
        .count();
    let name = if app.ed.project().name.is_empty() {
        "Untitled"
    } else {
        &app.ed.project().name
    };
    let title = format!(
        " {name}{}   finish {:04}-{:02}-{:02}   {} task(s), {crit} critical ",
        if app.ed.dirty() { " *" } else { "" },
        fin.year,
        fin.month,
        fin.day,
        app.ed.project().tasks.len(),
    );
    let mut spans = vec![Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    )];
    // Baseline variance for the selected task.
    if let Some(t) = app.ed.project().tasks.get(app.ed.sel()) {
        if let (Some(bf), Some(r)) = (
            t.baseline(0).and_then(|b| b.finish),
            app.ed.schedule().get(t.uid),
        ) {
            let delta = r.early_finish.day_number() - bf.day_number();
            let (label, color) = match delta.cmp(&0) {
                std::cmp::Ordering::Greater => (format!("▲ {delta}d late"), CRIT),
                std::cmp::Ordering::Less => (format!("▼ {}d early", -delta), ONTRACK),
                std::cmp::Ordering::Equal => ("on baseline".to_string(), Color::Gray),
            };
            spans.push(Span::styled(
                format!("· {}: {label} ", truncate(&t.name, 16)),
                Style::default().fg(color),
            ));
        }
        // Resources assigned to the selected task.
        let res = task_resources(app.ed.project(), t.uid);
        if !res.is_empty() {
            spans.push(Span::styled(
                format!("· 👤 {} ", res.join(", ")),
                Style::default().add_modifier(Modifier::DIM),
            ));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_body(f: &mut Frame, area: Rect, app: &mut App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(46), Constraint::Min(10)])
        .split(area);
    let left = cols[0];
    let right = cols[1];

    // Record geometry so mouse clicks can map to task rows / the gantt.
    app.list_left_w = left.width;
    app.list_y0 = left.y + 2; // border + column-header row
    app.gantt_x0 = right.x + 1;

    // visible task rows (inner height minus borders and the column-header row)
    let inner_h = left.height.saturating_sub(3) as usize; // 2 border + 1 header
    if app.ed.sel() < app.top {
        app.top = app.ed.sel();
    } else if inner_h > 0 && app.ed.sel() >= app.top + inner_h {
        app.top = app.ed.sel() + 1 - inner_h;
    }
    let visible = inner_h.max(1);
    let end = (app.top + visible).min(app.ed.project().tasks.len());

    // ---- left: task table ----
    let mut left_lines: Vec<Line> = Vec::new();
    left_lines.push(Line::from(Span::styled(
        format!(" {:<26} {:>5} {:>6}", "Task", "Dur", "Slack"),
        Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
    )));
    for i in app.top..end {
        let t = &app.ed.project().tasks[i];
        let r = app.ed.schedule().get(t.uid);
        let indent = "  ".repeat((t.outline_level.saturating_sub(1)) as usize);
        let bullet = if t.summary {
            "▾ "
        } else if t.is_milestone() {
            "◆ "
        } else {
            "• "
        };
        let res = task_resources(app.ed.project(), t.uid);
        let base = format!("{indent}{bullet}{}", t.name);
        let full = if res.is_empty() {
            base
        } else {
            // compact resource initials, e.g. "·AB"
            let inits: String = res.iter().filter_map(|r| r.chars().next()).collect();
            format!("{base} ·{inits}")
        };
        let namecol = truncate(&full, 26);
        let dur = if t.summary {
            String::new()
        } else if t.is_milestone() {
            "—".to_string()
        } else {
            fmt_days(app.ed.project().minutes_to_days(t.duration_min))
        };
        let slack = r
            .map(|r| fmt_days(app.ed.project().minutes_to_days(r.total_slack_min)))
            .unwrap_or_else(|| "?".into());
        let crit = r.is_some_and(|r| r.critical);
        let mut style = Style::default();
        if t.summary {
            style = style.add_modifier(Modifier::BOLD);
        }
        if crit && !t.summary {
            style = style.fg(CRIT);
        }
        let mut line = Line::from(vec![
            Span::raw(" "),
            Span::styled(format!("{namecol:<26}"), style),
            Span::styled(format!(" {dur:>5}"), Style::default().fg(Color::Gray)),
            Span::styled(format!(" {slack:>6}"), Style::default().fg(Color::DarkGray)),
        ]);
        if i == app.ed.sel() {
            line.style = Style::default().bg(app.sel_bg());
        }
        left_lines.push(line);
    }
    f.render_widget(
        Paragraph::new(left_lines).block(Block::default().borders(Borders::ALL).title(" Tasks ")),
        left,
    );

    // ---- right: gantt ----
    let gw = right.width.saturating_sub(2) as usize; // inner width
    let origin = gantt_origin_day(app);
    let start = DateTime::from_minutes(origin * 1440).parts();
    let mut right_lines: Vec<Line> = Vec::new();
    right_lines.push(build_scale(gw, app.hscroll, origin));
    for i in app.top..end {
        let t = &app.ed.project().tasks[i];
        let crit = app.ed.schedule().get(t.uid).is_some_and(|r| r.critical);
        let s_day = app
            .disp_start(t.uid)
            .map(|d| d.day_number() - origin)
            .unwrap_or(i64::MAX);
        let e_day = app
            .disp_finish(t.uid)
            .map(|d| d.day_number() - origin)
            .unwrap_or(i64::MIN);
        let mut line = build_gantt_row(
            gw,
            app.hscroll,
            origin,
            s_day,
            e_day,
            crit,
            t.summary,
            t.is_milestone(),
        );
        if i == app.ed.sel() {
            line.style = Style::default().bg(app.sel_bg());
        }
        right_lines.push(line);
    }
    let lev = if app.ed.leveled() { " · leveled" } else { "" };
    let gtitle = format!(
        " Gantt — from {:04}-{:02}-{:02} (◀ ▶ scroll){lev} ",
        start.year, start.month, start.day
    );
    f.render_widget(
        Paragraph::new(right_lines).block(Block::default().borders(Borders::ALL).title(gtitle)),
        right,
    );
}

/// Include linked tasks that schedule before the nominal project start.
fn gantt_origin_day(app: &App) -> i64 {
    app.ed
        .project()
        .tasks
        .iter()
        .filter_map(|t| app.disp_start(t.uid))
        .map(|d| d.day_number())
        .fold(app.ed.schedule().project_start.day_number(), i64::min)
}

/// The date scale row: a `m/d` tick at the start of each week within view.
fn build_scale(width: usize, hscroll: i64, base_day: i64) -> Line<'static> {
    let mut buf: Vec<char> = vec![' '; width];
    for col in 0..width {
        let day = base_day + hscroll + col as i64;
        let dt = DateTime::from_minutes(day * 1440);
        if dt.weekday() == 1 {
            // Monday: stamp "m/d" starting here if it fits.
            let p = dt.parts();
            let label = format!("{}/{}", p.month, p.day);
            for (j, ch) in label.chars().enumerate() {
                if col + j < width {
                    buf[col + j] = ch;
                }
            }
        }
    }
    Line::from(Span::styled(
        buf.into_iter().collect::<String>(),
        Style::default().fg(WEEKEND),
    ))
}

/// One task's bar across the visible day columns. `s_day`/`e_day` are day
/// offsets from the gantt origin (the earliest displayed or project start day).
#[allow(clippy::too_many_arguments)]
fn build_gantt_row(
    width: usize,
    hscroll: i64,
    base_day: i64,
    s_day: i64,
    e_day: i64,
    crit: bool,
    is_summary: bool,
    milestone: bool,
) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::with_capacity(width);
    let milestone = milestone && !is_summary;
    let bar_color = if crit { CRIT } else { ONTRACK };
    for col in 0..width {
        let day = hscroll + col as i64;
        let dt = DateTime::from_minutes((base_day + day) * 1440);
        let weekend = matches!(dt.weekday(), 0 | 6);
        let in_span = day >= s_day && day <= e_day;
        if milestone && day == s_day {
            spans.push(Span::styled(
                "◆",
                Style::default().fg(MILESTONE).add_modifier(Modifier::BOLD),
            ));
        } else if is_summary && in_span {
            // rollup bar: end caps + a thin spine, distinct from task bars
            let ch = if day == s_day || day == e_day {
                "▟"
            } else {
                "▬"
            };
            spans.push(Span::styled(
                ch,
                Style::default().fg(SUMMARY).add_modifier(Modifier::BOLD),
            ));
        } else if !milestone && in_span {
            spans.push(Span::styled("█", Style::default().fg(bar_color)));
        } else if weekend {
            spans.push(Span::styled(
                "·",
                Style::default().fg(WEEKEND).add_modifier(Modifier::DIM),
            ));
        } else {
            spans.push(Span::raw(" "));
        }
    }
    Line::from(spans)
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let help = "n add · d dur · p dep · Tab indent · Enter rename · x del · Ctrl+F find · Ctrl+Z undo · Ctrl+S save · q quit";
    let text = if app.status.is_empty() {
        help.to_string()
    } else {
        app.status.clone()
    };
    let mut spans = Vec::new();
    if app.vim {
        spans.push(Span::styled(
            " -- VIM -- ",
            Style::default().fg(Color::Black).bg(Color::Green),
        ));
    }
    spans.push(Span::styled(
        format!(" {text}"),
        Style::default().fg(Color::Gray),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_prompt(f: &mut Frame, area: Rect, app: &App) {
    let Some(p) = &app.prompt else { return };
    let w = area.width.clamp(20, 60);
    let x = (area.width.saturating_sub(w)) / 2;
    let y = area.height / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: 3,
    };
    f.render_widget(Clear, rect);
    let content = Line::from(vec![
        Span::styled(
            format!(" {}: ", p.label),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(p.buf.clone()),
        Span::styled("▏", Style::default().fg(Color::Gray)),
    ]);
    f.render_widget(
        Paragraph::new(content).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Enter ↵  Esc ✕ "),
        ),
        rect,
    );
}

// ---- small helpers ----------------------------------------------------------

/// Names of the resources assigned to task `uid`, in assignment order.
fn task_resources(proj: &Project, uid: i32) -> Vec<String> {
    proj.assignments
        .iter()
        .filter(|a| a.task_uid == uid)
        .filter_map(|a| {
            proj.resources
                .iter()
                .find(|r| r.uid == a.resource_uid)
                .map(|r| r.name.clone())
        })
        .collect()
}

fn fmt_days(days: f64) -> String {
    if (days.round() - days).abs() < 1e-9 {
        format!("{}d", days.round() as i64)
    } else {
        format!("{days:.1}d")
    }
}

/// Truncate to a display width, adding an ellipsis when it doesn't fit.
fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = UnicodeWidthStr::width(ch.to_string().as_str());
        if w + cw > width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_saves_refuse_mpp_and_other_unsupported_formats() {
        let dir = std::env::temp_dir().join(format!("yppxy-save-formats-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let project = new_project();
        for name in ["plan.mpp", "plan.MPP", "plan.txt", "plan"] {
            let path = dir.join(name);
            std::fs::write(&path, b"original binary schedule").unwrap();
            assert_eq!(
                save_to(&project, path.to_str().unwrap()).unwrap_err(),
                SAVE_FORMAT_ERROR
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"original binary schedule");
            std::fs::remove_file(path).unwrap();
        }
        for name in ["plan.yppx", "plan.YPPX", "plan.xml", "plan.XML"] {
            let path = dir.join(name);
            save_to(&project, path.to_str().unwrap()).unwrap();
            assert_eq!(load(path.to_str().unwrap()).unwrap(), project);
            std::fs::remove_file(path).unwrap();
        }
        let original = dir.join("import.mpp");
        std::fs::write(&original, b"original binary schedule").unwrap();
        let mut app = App::new(project, Some(original.to_str().unwrap().into()), false);
        app.ed.rename(1, "Unsaved edit").unwrap();
        app.save();
        assert!(app.status.contains(SAVE_FORMAT_ERROR));
        assert!(app.ed.dirty());
        app.commit_save_as(dir.clone(), "import.mpp".into());
        assert!(app.status.contains(SAVE_FORMAT_ERROR));
        assert_eq!(app.path.as_deref(), original.to_str());
        assert!(app.ed.dirty());
        assert_eq!(
            std::fs::read(&original).unwrap(),
            b"original binary schedule"
        );
        assert!(!dir.join("import.mpp.yppx").exists());
        app.commit_save_as(dir.clone(), "converted".into());
        assert!(!app.ed.dirty());
        let converted = dir.join("converted.yppx");
        assert_eq!(app.path.as_deref(), converted.to_str());
        assert_eq!(
            load(converted.to_str().unwrap()).unwrap().tasks[0].name,
            "Unsaved edit"
        );
        std::fs::remove_file(original).unwrap();
        std::fs::remove_file(converted).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn exports_refuse_source_aliases() {
        let dir = std::env::temp_dir().join(format!("yppxy-export-alias-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("note.xml");
        let proj = new_project();
        let original = mspdi::write_mspdi(&proj).into_bytes();
        std::fs::write(&source, &original).unwrap();
        assert!(
            write_gantt_md(
                &proj,
                &schedule(&proj),
                source.to_str(),
                dir.join("./note.xml").to_str().unwrap()
            )
            .is_err()
        );
        let md = source.with_extension("md");
        std::fs::hard_link(&source, &md).unwrap();
        let mut app = App::new(proj, Some(source.to_str().unwrap().into()), false);
        app.export_md();
        assert!(app.status.contains("cannot overwrite"));
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(std::fs::read(&md).unwrap(), original);
        std::fs::remove_file(md).unwrap();
        app.save();
        assert!(!app.ed.dirty());
        assert!(load(source.to_str().unwrap()).is_ok());
        std::fs::remove_file(source).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn task_grid_reports_negative_total_slack() {
        use projcore::model::ConstraintType;
        use ratatui::backend::TestBackend;
        let mut proj = projcore::mspdi::read_mspdi(include_str!(
            "../../corpus/mspdi/14-link-sf-before-start.xml"
        ))
        .unwrap();
        proj.tasks[0].name = "Late".into();
        proj.tasks[0].constraint = ConstraintType::FinishNoLaterThan;
        proj.tasks[0].constraint_date = Some(DateTime::from_ymd_hm(2026, 2, 26, 17, 0));
        let mut app = App::new(proj, None, false);
        assert_eq!(app.ed.schedule().get(1).unwrap().total_slack_min, -480);
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| draw_body(f, f.area(), &mut app)).unwrap();
        let buf = term.backend().buffer();
        let row: String = (1..45)
            .map(|x| buf.cell((x, app.list_y0)).unwrap().symbol())
            .collect();
        assert_eq!(
            row.split_whitespace().collect::<Vec<_>>(),
            ["•", "Late", "2d", "-1d"]
        );
    }

    #[test]
    fn gantt_origin_includes_pre_start_sf_bar() {
        let proj = projcore::mspdi::read_mspdi(include_str!(
            "../../corpus/mspdi/14-link-sf-before-start.xml"
        ))
        .unwrap();
        let app = App::new(proj, None, false);
        let origin = gantt_origin_day(&app);
        assert_eq!(
            origin,
            DateTime::from_ymd_hm(2026, 2, 26, 8, 0).day_number()
        );
        let row = build_gantt_row(
            10,
            0,
            origin,
            app.disp_start(1).unwrap().day_number() - origin,
            app.disp_finish(1).unwrap().day_number() - origin,
            false,
            false,
            false,
        );
        assert_eq!(row.spans[0].content, "█");
        let app = App::new(new_project(), None, false);
        assert_eq!(
            gantt_origin_day(&app),
            app.ed.schedule().project_start.day_number()
        );
    }

    #[test]
    fn fresh_tui_project_keeps_its_starter_task() {
        let p = new_project();
        assert_eq!(p.start_date, Some(projcore::editor::default_anchor()));
        assert_eq!(p.tasks.len(), 1);
        let t = &p.tasks[0];
        assert_eq!(
            (&*t.name, t.outline_level, t.duration_min),
            ("New task", 1, 480)
        );
    }

    #[test]
    fn window_title_format() {
        assert_eq!(
            window_title(&Some("/a/plan.yppx".into()), false),
            "yppxy - plan.yppx"
        );
        assert_eq!(
            window_title(&Some("plan.xml".into()), true),
            "* yppxy - plan.xml"
        );
        assert_eq!(window_title(&None, false), "yppxy - untitled");
    }

    #[test]
    fn parse_duration_units() {
        let p = Project::default(); // 8h/day, 40h/week
        assert_eq!(parse_duration("2d", &p), Some(960));
        assert_eq!(parse_duration("3", &p), Some(1440));
        assert_eq!(parse_duration("4h", &p), Some(240));
        assert_eq!(parse_duration("1w", &p), Some(2400)); // 5 working days
        assert_eq!(parse_duration("nope", &p), None);
    }

    #[test]
    fn summaries_follow_outline() {
        let mut app = App::new(new_project(), None, false);
        app.add_task(); // second task at level 1
        // Make the second task a child of the first.
        app.ed.select(1);
        app.indent(1);
        assert!(app.ed.project().tasks[0].summary); // parent became a summary
        assert!(!app.ed.project().tasks[1].summary);
    }

    fn app_with_history_and_preferences() -> App {
        let mut app = App::new(new_project(), Some("old.yppx".into()), false);
        app.toggle_level();
        app.find(" NEW ");
        app.add_task();
        app.rename("Renamed");
        app.undo();
        assert!(app.ed.dirty());
        assert!(app.ed.undo_depth() > 0 && app.ed.redo_depth() > 0);
        app
    }

    fn assert_replaced_session(app: &App) {
        assert!(app.ed.leveled());
        assert_eq!(app.ed.find_query(), "new");
        assert!(!app.ed.dirty());
        assert_eq!(
            (app.ed.sel(), app.ed.undo_depth(), app.ed.redo_depth()),
            (0, 0, 0)
        );
        assert_eq!(app.ed.project().tasks.len(), 1);
    }

    #[test]
    fn open_resets_history_and_retains_leveling_and_find() {
        let mut app = app_with_history_and_preferences();
        let path =
            std::env::temp_dir().join(format!("yppxy-editor-open-{}.yppx", std::process::id()));
        let path_str = path.to_str().unwrap();
        let mut proj = new_project();
        proj.tasks[0].name = "Opened task".into();
        save_to(&proj, path_str).unwrap();
        app.open_file(path_str);
        std::fs::remove_file(&path).unwrap();
        assert_replaced_session(&app);
        assert_eq!(app.ed.project().tasks[0].name, "Opened task");
        assert_eq!(app.path.as_deref(), Some(path_str));
        assert_eq!(app.status, format!("Opened {path_str}"));
        app.undo();
        assert_eq!(app.status, "Nothing to undo");
        assert_eq!(app.ed.project().tasks[0].name, "Opened task");
    }

    #[test]
    fn new_resets_history_and_retains_leveling_and_find() {
        let mut app = app_with_history_and_preferences();
        app.new_schedule();
        assert_replaced_session(&app);
        assert!(app.path.is_none());
        assert_eq!(app.status, "New schedule");
    }

    #[test]
    fn failed_open_preserves_the_editor_session() {
        let mut app = app_with_history_and_preferences();
        let proj = app.ed.project().clone();
        let history = (app.ed.undo_depth(), app.ed.redo_depth());
        let selection = app.ed.sel();
        let dates: Vec<_> = proj
            .tasks
            .iter()
            .map(|t| {
                (
                    t.uid,
                    *app.ed.schedule().get(t.uid).unwrap(),
                    app.disp_start(t.uid),
                    app.disp_finish(t.uid),
                )
            })
            .collect();
        app.open_file("/nonexistent/yppxy/editor-test/missing.yppx");
        assert!(app.status.starts_with("Open failed:"));
        assert_eq!(app.ed.project(), &proj);
        assert_eq!((app.ed.undo_depth(), app.ed.redo_depth()), history);
        assert_eq!(app.ed.sel(), selection);
        assert!(app.ed.leveled() && app.ed.dirty());
        assert_eq!(app.ed.find_query(), "new");
        assert_eq!(app.path.as_deref(), Some("old.yppx"));
        for (uid, result, start, finish) in dates {
            assert_eq!(app.ed.schedule().get(uid), Some(&result));
            assert_eq!(app.disp_start(uid), start);
            assert_eq!(app.disp_finish(uid), finish);
        }
        app.redo();
        assert_eq!(app.ed.project().tasks[1].name, "Renamed");
    }

    #[test]
    fn prompted_edits_keep_status_messages() {
        let mut app = App::new(new_project(), None, false);
        app.status = "unchanged".into();
        app.find("");
        assert_eq!(app.status, "unchanged");
        app.find(" NEW ");
        assert_eq!(app.status, "Found 'new'  (F3 next)");
        app.find("");
        assert_eq!(app.status, "Found 'new'  (F3 next)");
        app.find("missing");
        assert_eq!(app.status, "No task matching 'missing'");
        app.set_duration("banana");
        assert_eq!(
            app.status,
            "Couldn't read duration 'banana' (try 3d, 4h, 2w)"
        );
        app.add_predecessor("abc");
        assert_eq!(app.status, "Predecessor must be a task ID (number)");
        app.add_predecessor("1");
        assert_eq!(app.status, "No other task with ID 1");
        app.add_predecessor("999");
        assert_eq!(app.status, "No other task with ID 999");
        app.add_task();
        app.add_predecessor("1");
        app.add_predecessor("1");
        assert_eq!(app.status, "Already depends on 1");
        app.set_constraint("none");
        assert_eq!(app.status, "Constraint set: NONE");
        app.set_constraint("asap");
        assert_eq!(app.status, "Constraint set: ASAP");
        app.set_constraint("mso");
        assert_eq!(app.status, "MSO needs a date, e.g. mso 2026-03-05");
        app.assign_resource(" Alice ");
        assert_eq!(app.status, "Assigned Alice");
        app.assign_resource("alice");
        assert_eq!(app.status, "alice is already assigned");
        app.assign_resource("");
        assert_eq!(app.status, "Cleared the task's resources");
        app.status = "unchanged".into();
        app.assign_resource("");
        assert_eq!(app.status, "unchanged");
        app.ed.replace_project(Project::default());
        app.find("new");
        assert_eq!(app.status, "unchanged");
        app.add_predecessor("1");
        assert_eq!(app.status, "No task selected");
    }

    #[test]
    #[ignore]
    fn preview_dump() {
        use ratatui::backend::TestBackend;
        let path = std::env::var("YPPXY_PREVIEW").unwrap();
        let proj = load(&path).unwrap();
        let mut app = App::new(proj, Some(path), false);
        let (w, h) = (110u16, 22u16);
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        for y in 0..h {
            let mut line = String::new();
            for x in 0..w {
                if let Some(c) = buf.cell((x, y)) {
                    line.push_str(c.symbol());
                }
            }
            println!("{}", line.trim_end());
        }
    }

    #[test]
    fn renders_a_frame_without_panic() {
        use ratatui::backend::TestBackend;
        let mut proj = new_project();
        proj.tasks.push(Task {
            uid: 2,
            id: 2,
            name: "Build".into(),
            outline_level: 1,
            duration_min: 960,
            predecessors: vec![Predecessor {
                uid: 1,
                link: LinkType::FinishStart,
                lag_min: 0,
            }],
            ..Task::default()
        });
        let mut app = App::new(proj, Some("plan.yppx".into()), false);
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let mut s = String::new();
        for y in 0..20u16 {
            for x in 0..100u16 {
                if let Some(c) = buf.cell((x, y)) {
                    s.push_str(c.symbol());
                }
            }
        }
        assert!(s.contains("Tasks"), "task pane missing");
        assert!(s.contains("Gantt"), "gantt pane missing");
        assert!(s.contains("Build"), "second task not rendered");
        assert!(s.contains('█'), "no gantt bar drawn");
    }

    fn buffer_text(app: &mut App, w: u16, h: u16) -> String {
        use ratatui::backend::TestBackend;
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        let mut s = String::new();
        for y in 0..h {
            for x in 0..w {
                if let Some(c) = buf.cell((x, y)) {
                    s.push_str(c.symbol());
                }
            }
        }
        s
    }

    #[test]
    fn ribbon_renders_tabs_and_groups() {
        let mut proj = new_project();
        proj.tasks.push(Task {
            uid: 2,
            id: 2,
            name: "Build".into(),
            outline_level: 1,
            duration_min: 960,
            ..Task::default()
        });
        let mut app = App::new(proj, Some("plan.yppx".into()), false);
        let s = buffer_text(&mut app, 110, 24);
        assert!(
            s.contains("File")
                && s.contains("Task")
                && s.contains("Schedule")
                && s.contains("View")
        );
        assert!(s.contains("Milestone"), "ribbon body missing");
        assert!(s.contains("Gantt"), "gantt pane missing");
    }

    #[test]
    fn backstage_and_start_render() {
        // start screen
        let mut app = App::new(new_project(), None, false);
        assert!(app.start_screen);
        let s = buffer_text(&mut app, 100, 22);
        assert!(s.contains("yppxy") && s.contains("New schedule"));

        // backstage
        let mut app = App::new(new_project(), Some("plan.yppx".into()), false);
        app.open_backstage();
        let s = buffer_text(&mut app, 100, 22);
        assert!(s.contains("File") && s.contains("Open") && s.contains("Export"));
    }

    #[test]
    fn undo_redo_restores_tasks() {
        let mut app = App::new(new_project(), None, false);
        assert_eq!(app.ed.project().tasks.len(), 1);
        app.add_task();
        app.add_task();
        assert_eq!(app.ed.project().tasks.len(), 3);
        app.undo();
        assert_eq!(app.ed.project().tasks.len(), 2);
        app.undo();
        assert_eq!(app.ed.project().tasks.len(), 1);
        app.redo();
        assert_eq!(app.ed.project().tasks.len(), 2);
        // a fresh edit clears the redo stack
        app.add_task();
        app.redo();
        assert_eq!(app.ed.project().tasks.len(), 3);
    }

    #[test]
    fn find_selects_matching_task_and_wraps() {
        let mut proj = new_project();
        proj.tasks[0].name = "Alpha".into();
        proj.tasks.push(Task {
            uid: 2,
            id: 2,
            name: "Bravo".into(),
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        });
        proj.tasks.push(Task {
            uid: 3,
            id: 3,
            name: "Charlie".into(),
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        });
        let mut app = App::new(proj, None, false);
        app.find("charlie");
        assert_eq!(app.ed.sel(), 2);
        // F3-style repeat from the end wraps back to Alpha (no more Charlie)
        app.find("");
        assert_eq!(app.ed.sel(), 2); // only one Charlie → stays
        app.find("a"); // matches Alpha/Bravo/Charlie — next after sel 2 wraps to 0
        assert_eq!(app.ed.sel(), 0);
    }

    #[test]
    fn vim_commands_save_and_quit() {
        let mut app = App::new(
            new_project(),
            Some("/nonexistent/dir/plan.yppx".into()),
            true,
        );
        app.vim_run("q"); // dirty? no — fresh project isn't dirty
        assert!(app.quit);
        let mut app = App::new(new_project(), None, true);
        app.add_task(); // now dirty
        app.vim_run("q"); // should refuse
        assert!(!app.quit);
        app.vim_run("q!"); // force
        assert!(app.quit);
    }

    #[test]
    fn constraint_snet_delays_start() {
        let mut app = App::new(new_project(), None, false); // anchor Mon 2026-01-05
        app.set_constraint("SNET 2026-01-08"); // Thursday
        let r = app
            .ed
            .schedule()
            .get(app.ed.project().tasks[0].uid)
            .unwrap();
        assert_eq!(r.early_start.parts().day, 8);
    }

    #[test]
    fn baseline_captures_plan_and_variance_shows() {
        let mut app = App::new(new_project(), None, false);
        app.set_baseline();
        let bf = app.ed.project().tasks[0]
            .baseline(0)
            .and_then(|b| b.finish)
            .expect("baseline captured");
        app.set_duration("5d"); // extend past the baseline
        let r = app
            .ed
            .schedule()
            .get(app.ed.project().tasks[0].uid)
            .unwrap();
        assert!(
            r.early_finish.day_number() > bf.day_number(),
            "finish should slip past baseline"
        );
    }

    #[test]
    fn header_variance_uses_slot_zero_only() {
        use ratatui::backend::TestBackend;
        for number in [0, 1] {
            let mut proj = new_project();
            proj.tasks[0].set_baseline_slot(projcore::Baseline {
                number,
                finish: Some(projcore::DateTime::from_ymd_hm(2026, 1, 1, 17, 0)),
                ..projcore::Baseline::default()
            });
            let app = App::new(proj, None, false);
            let mut term = Terminal::new(TestBackend::new(160, 1)).unwrap();
            term.draw(|f| draw_header(f, f.area(), &app)).unwrap();
            let text: String = term
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert_eq!(text.contains("d late"), number == 0, "{text}");
            assert!(
                !text.contains("d early") && !text.contains("on baseline"),
                "{text}"
            );
        }
    }

    #[test]
    fn assign_resource_creates_and_round_trips() {
        let mut app = App::new(new_project(), None, false);
        app.assign_resource("Alice");
        assert_eq!(app.ed.project().resources.len(), 1);
        assert_eq!(app.ed.project().assignments.len(), 1);
        // assigning the same resource again is a no-op
        app.assign_resource("alice");
        assert_eq!(app.ed.project().assignments.len(), 1);
        app.assign_resource("Bob");
        assert_eq!(app.ed.project().resources.len(), 2);
        let names = task_resources(app.ed.project(), app.ed.project().tasks[0].uid);
        assert_eq!(names, vec!["Alice".to_string(), "Bob".to_string()]);

        // resources/assignments survive a MSPDI round-trip
        let xml = mspdi::write_mspdi(app.ed.project());
        let back = mspdi::read_mspdi(&xml).unwrap();
        assert_eq!(task_resources(&back, back.tasks[0].uid), names);

        // clearing removes the task's assignments (resources remain defined)
        app.assign_resource("");
        assert!(app.ed.project().assignments.is_empty());
        assert_eq!(app.ed.project().resources.len(), 2);
    }

    #[test]
    fn level_toggle_delays_shared_resource() {
        let mut proj = new_project(); // task 1 (1d)
        proj.tasks.push(Task {
            uid: 2,
            id: 2,
            name: "B".into(),
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        });
        let mut app = App::new(proj, None, false);
        app.ed.select(0);
        app.assign_resource("Alice");
        app.ed.select(1);
        app.assign_resource("Alice");
        // unleveled: both start the same day
        assert_eq!(
            app.disp_start(1).unwrap().day_number(),
            app.disp_start(2).unwrap().day_number()
        );
        // leveled: task 2 waits for Alice to free up
        app.toggle_level();
        assert!(app.disp_start(2).unwrap().day_number() > app.disp_start(1).unwrap().day_number());
    }

    #[test]
    fn add_and_delete_keep_schedule_consistent() {
        let mut app = App::new(new_project(), None, false);
        app.add_task();
        app.ed.select(1);
        app.add_predecessor("1"); // depend on task 1
        assert_eq!(app.ed.project().tasks[1].predecessors.len(), 1);
        // deleting task 1 drops the dangling link
        app.ed.select(0);
        app.delete_task();
        assert!(
            app.ed
                .project()
                .tasks
                .iter()
                .all(|t| t.predecessors.is_empty())
        );
    }

    #[test]
    fn ctrl_q_opens_the_exit_confirmation() {
        let mut app = App::new(new_project(), Some("plan.yppx".into()), false);
        app.add_task();
        assert!(app.ed.dirty());
        // Ctrl+Q does not quit outright — it opens the Yes/No modal.
        on_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
        );
        assert!(app.confirm.is_some());
        assert!(!app.quit);
        // the prompt warns about unsaved changes
        assert!(app.confirm.as_ref().unwrap().prompt().contains("Unsaved"));
        // confirming with 'y' quits
        on_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        assert!(app.quit);
    }

    #[test]
    fn ctrl_q_confirmation_can_be_cancelled() {
        let mut app = App::new(new_project(), Some("plan.yppx".into()), false);
        // even with no changes, Ctrl+Q asks first
        on_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
        );
        assert!(app.confirm.is_some());
        // No / Esc dismisses without quitting
        on_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.confirm.is_none());
        assert!(!app.quit);
    }
}
