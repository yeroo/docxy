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
//!                                      legacy .mpp (validated task tables)
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
use projcore::editor::{
    AssignOutcome, Editor, FindOutcome, constraint_hint, format_resource_names, parse_duration,
};
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
// A manual summary's subtasks running past its own finish (Project's warning).
const WARNING: Color = Color::Rgb(220, 50, 47);

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
            eprintln!(
                "usage: yppxy [file.(xml|yppx|mpp)] [--gantt-md <out>] [--save <out.(yppx|xml)>]"
            );
            return ExitCode::from(2);
        }
    };
    if parsed.help {
        println!("usage: yppxy [file.(xml|yppx|mpp)] [--gantt-md <out>] [--save <out.(yppx|xml)>]");
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
    // Each headless mode exits after its own output, so a combination would
    // silently skip one of them.
    if out.gantt_md.is_some() && out.save.is_some() {
        return Err("--gantt-md and --save cannot be combined".into());
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

fn save_to(proj: &Project, path: &str) -> Result<std::path::PathBuf, String> {
    let path = yppx::save_target(Path::new(path))?;
    let bytes = if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("yppx"))
    {
        yppx::write_yppx(proj)
    } else {
        mspdi::write_mspdi(proj).into_bytes()
    };
    write_atomic(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path)
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
    // Tests toggle the theme; never let them rewrite the user's prefs.
    if cfg!(test) {
        return;
    }
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
    /// Delete this task and its subtasks.
    DeleteTask(i32),
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
    /// The shared Yes/No modal, open while asking whether to quit (Ctrl+Q /
    /// File ▸ Exit) or to delete a summary with its subtasks.
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
    screen_w: u16,    // terminal width, for the tab-strip Theme button
    /// The status line's `New Tasks: …` segment: its row and end column.
    new_tasks_hit: Option<(u16, u16)>,
}

const RIBBON_H: u16 = 7; // tab strip (1) + body (6: border, 2 rows, separator, titles, border)

/// The Theme button at the right end of the ribbon tab strip — yppxy's
/// counterpart of the suite's title-bar button (Microsoft Project has no
/// ribbon command for it). `T` toggles the theme too.
const THEME_BTN: &str = "◐ Theme";

/// Columns `[a, b)` of [`THEME_BTN`] on a tab strip `width` wide whose tabs end
/// at `tabs_right`: right-aligned with a one-column margin, or `None` when it
/// would come within two columns of the tabs. Drawing and clicks both use it.
fn theme_btn_cols(width: u16, tabs_right: u16) -> Option<(u16, u16)> {
    let w = THEME_BTN.width() as u16;
    let b = width.checked_sub(1)?;
    let a = b.checked_sub(w)?;
    (a >= tabs_right + 2).then_some((a, b))
}

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
            screen_w: 0,
            new_tasks_hit: None,
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
        // A text prompt takes keys before the modal would, so it must not
        // stay open underneath it.
        self.prompt = None;
        self.confirm = Some(self.exit_confirm());
    }

    /// The Exit question, warning about unsaved changes when there are any.
    fn exit_confirm(&self) -> backstage::Confirm<ConfirmAction> {
        let prompt = if self.ed.dirty() {
            "Exit yppxy? Unsaved changes will be lost."
        } else {
            "Exit yppxy?"
        };
        backstage::Confirm::new(prompt, ConfirmAction::Exit, Color::Yellow)
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
                    ConfirmAction::DeleteTask(uid) => self.delete_subtree(uid),
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

    /// Switch the selected task between Manually and Auto Scheduled.
    fn set_manual(&mut self, manual: bool) {
        let Some(uid) = self.ed.selected_uid() else {
            return;
        };
        self.status = match self.ed.set_manual(uid, manual) {
            Ok(()) => mode_name(manual).into(),
            Err(message) => message,
        };
    }

    /// `m`: the selected task's other mode (a blank row becomes manual).
    fn toggle_manual(&mut self) {
        if let Some(t) = self.ed.project().tasks.get(self.ed.sel()) {
            let manual = t.is_null || !t.manual;
            self.set_manual(manual);
        }
    }

    /// `M` and a click on the status line's segment: the plan's mode for new
    /// tasks, as Project's status bar switches it.
    fn toggle_new_tasks_manual(&mut self) {
        let manual = !self.ed.project().new_tasks_are_manual;
        self.ed.set_new_tasks_manual(manual);
        self.status = format!("New tasks: {}", mode_name(manual));
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
            Act::ManuallySchedule => self.set_manual(true),
            Act::AutoSchedule => self.set_manual(false),
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
            Act::Find => self.find_prompt(),
            Act::Baseline => self.set_baseline(),
            Act::CalculateProject => {
                // The scheduler reruns on every edit, so there is nothing
                // pending; say so, in the suite's words.
                self.status = "Rescheduled (automatic on every edit)".into();
            }
            Act::LevelAll => self.set_level(true),
            Act::ClearLeveling => self.set_level(false),
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
        }
    }

    /// Open the Find prompt (Ctrl+F and Task › Editing › Find).
    fn find_prompt(&mut self) {
        self.prompt = Some(Prompt {
            kind: PromptKind::Find,
            label: "Find".into(),
            buf: String::new(),
        });
    }

    /// `L`: flip resource leveling.
    fn toggle_level(&mut self) {
        self.set_level(!self.ed.leveled());
    }

    /// Resource › Level › Level All (`on`) / Clear Leveling (`!on`). Idempotent;
    /// the status reports the resulting state either way, as the suite does.
    fn set_level(&mut self, on: bool) {
        if self.ed.leveled() != on {
            self.ed.toggle_level();
        }
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

    /// Delete the selected task; a summary asks first, because its subtasks
    /// go with it.
    fn delete_task(&mut self) {
        let Some(uid) = self.ed.selected_uid() else {
            return;
        };
        match self.ed.subtree_len(uid) {
            Ok(0) => self.delete_subtree(uid),
            Ok(n) => {
                // As in `request_exit`: no hidden prompt may take the modal's keys.
                self.prompt = None;
                let name = self.ed.project().task(uid).map_or("", |t| &t.name);
                let noun = if n == 1 { "subtask" } else { "subtasks" };
                self.confirm = Some(backstage::Confirm::new(
                    format!("Delete '{name}' and its {n} {noun}?"),
                    ConfirmAction::DeleteTask(uid),
                    Color::Yellow,
                ));
            }
            Err(message) => self.status = message,
        }
    }

    /// Bring an open question up to date after an agent edit or a reload.
    /// A summary-delete question is dropped: its task, count and project were
    /// read before the change. An Exit question gets its unsaved-changes
    /// warning updated in place: the user's Yes/No choice is kept, and
    /// nothing else is closed (the modal already owns the input).
    fn refresh_confirm(&mut self) {
        let Some(c) = &self.confirm else {
            return;
        };
        match c.action() {
            ConfirmAction::DeleteTask(_) => self.confirm = None,
            ConfirmAction::Exit => {
                let next = self.exit_confirm();
                if next.prompt() != c.prompt() {
                    let yes = c.yes_selected();
                    self.confirm = Some(if yes { next } else { next.default_no() });
                }
            }
        }
    }

    fn delete_subtree(&mut self, uid: i32) {
        if let Err(message) = self.ed.delete_task(uid) {
            self.status = message;
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

    /// Publish the resolved binding and clean state only after a successful save.
    fn save_to_path(&mut self, path: &str) -> Result<(), String> {
        let path = save_to(self.ed.project(), path)?
            .to_string_lossy()
            .into_owned();
        self.ed.mark_saved();
        self.status = format!("Saved {path}");
        self.path = Some(path);
        Ok(())
    }

    fn save(&mut self) {
        match self.path.clone() {
            Some(p) => {
                if let Err(e) = self.save_to_path(&p) {
                    self.status = format!("Save failed: {e}");
                }
            }
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
                        "Opened {path} — {} .mpp tasks imported",
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
        let path = dir.join(name).to_string_lossy().into_owned();
        self.backstage = None;
        if let Err(e) = self.save_to_path(&path) {
            self.status = format!("Save failed: {e}");
        }
    }
}

/// A few summary lines for the backstage preview / Info pane.
fn project_preview(proj: &Project, sched: &Schedule) -> Vec<String> {
    let fin = sched.project_finish.parts();
    let start = sched.project_start.parts();
    // Blank rows (#80) are not tasks.
    let leaves = proj
        .tasks
        .iter()
        .filter(|t| !t.summary && !t.is_null)
        .count();
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
        if t.is_null {
            out.push(String::new());
            continue;
        }
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
    // A modal confirmation (Exit, summary delete) owns the whole screen while open — before
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
                if y == 0 {
                    if let Some((a, b)) = theme_btn_cols(app.screen_w, app.ribbon.width()) {
                        if x >= a && x < b {
                            app.theme_toggle();
                            return;
                        }
                    }
                }
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
            if app
                .new_tasks_hit
                .is_some_and(|(row, end)| y == row && x < end)
            {
                app.toggle_new_tasks_manual();
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
                            if let Err(e) = app.save_to_path(text.trim()) {
                                app.status = format!("Save failed: {e}");
                            }
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

    // A modal confirmation (Exit, summary delete) owns all keys while open — before the
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
            KeyCode::Char('f') => app.find_prompt(),
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
        KeyCode::Char('m') => app.toggle_manual(),
        KeyCode::Char('M') => app.toggle_new_tasks_manual(),
        KeyCode::Char('T') => app.theme_toggle(),
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

    // Reflect leveling in the ribbon (Level All drawn as an active toggle).
    let mut toggles = Vec::new();
    if app.ed.leveled() {
        toggles.push(Act::LevelAll);
    }
    // The selected task's mode, as Project highlights it.
    if let Some(t) = app.ed.project().tasks.get(app.ed.sel()) {
        if !t.is_null {
            toggles.push(if t.manual {
                Act::ManuallySchedule
            } else {
                Act::AutoSchedule
            });
        }
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
    app.screen_w = area.width;
    if let Some((a, b)) = theme_btn_cols(area.width, app.ribbon.width()) {
        let btn = Rect {
            x: area.x + a,
            y: rows[0].y,
            width: b - a,
            height: 1,
        };
        let style = Style::default().fg(Color::Yellow);
        f.render_widget(Paragraph::new(Span::styled(THEME_BTN, style)), btn);
    }
    f.render_widget(Paragraph::new(app.ribbon.render_body(app.rfocus)), rows[1]);
    draw_header(f, rows[2], app);
    draw_body(f, rows[3], app);
    app.new_tasks_hit = Some((rows[4].y, rows[4].x + new_tasks_label(app).width() as u16));
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
        let res = format_resource_names(app.ed.project(), t.uid);
        if !res.is_empty() {
            spans.push(Span::styled(
                format!("· 👤 {res} "),
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
        format!(" {:<2}{:<24} {:>5} {:>6}", "", "Task", "Dur", "Slack"),
        Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
    )));
    for i in app.top..end {
        let t = &app.ed.project().tasks[i];
        if t.is_null {
            // A blank row (#80) is not a task: an empty, still selectable line.
            let mut line = Line::from(" ");
            if i == app.ed.sel() {
                line.style = Style::default().bg(app.sel_bg());
            }
            left_lines.push(line);
            continue;
        }
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
        let namecol = truncate(&full, 24);
        let dur = if t.summary {
            // The stored summary duration is stale; derive it from the shown dates.
            app.ed.disp_duration_min(t.uid).map_or_else(
                || "?".into(),
                |min| fmt_days(app.ed.project().minutes_to_days(min)),
            )
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
            // The Task Mode cell, its own span: `format!` pads by chars,
            // and the pin is two columns wide.
            Span::styled(mode_marker(t), Style::default().fg(Color::Yellow)),
            Span::styled(format!("{namecol:<24}"), style),
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
        // A manual summary keeps its own dates; its subtasks' span is drawn
        // beside them.
        let rollup = app
            .ed
            .disp_rollup(t.uid)
            .filter(|_| t.manual_summary_dates().is_some())
            .map(|(s, f)| (s.day_number() - origin, f.day_number() - origin));
        let mut line = build_gantt_row(
            gw,
            app.hscroll,
            origin,
            s_day,
            e_day,
            crit,
            t.summary,
            t.is_milestone(),
            rollup,
            t.summary && app.ed.summary_warning(t.uid),
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
/// `rollup` is a manual summary's rolled-up span in the same offsets: the days
/// of it outside the summary's own span are marked, those past its finish in
/// the warning colour. A `warning` with no rollup day past the finish (an
/// overrun within the finish day, or a finish past a manual parent's) puts the
/// finish day's cell in the warning colour instead.
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
    rollup: Option<(i64, i64)>,
    warning: bool,
) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::with_capacity(width);
    let warn_finish = warning && !rollup.is_some_and(|(_, r_e)| r_e > e_day);
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
            let color = if warn_finish && day == e_day {
                WARNING
            } else {
                SUMMARY
            };
            spans.push(Span::styled(
                ch,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));
        } else if rollup.is_some_and(|(r_s, r_e)| day >= r_s && day <= r_e) {
            let style = if day > e_day {
                Style::default().fg(WARNING).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SUMMARY).add_modifier(Modifier::DIM)
            };
            spans.push(Span::styled("╍", style));
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

/// A task's mode as Project names it.
fn mode_name(manual: bool) -> &'static str {
    if manual {
        "Manually Scheduled"
    } else {
        "Auto Scheduled"
    }
}

/// The Task Mode cell: a pin for a manually scheduled task, two columns
/// either way (the pin is a wide character).
fn mode_marker(t: &Task) -> &'static str {
    if t.manual { "📌" } else { "  " }
}

/// The status line's first segment, as Project's status bar shows it; a
/// click on it (or `M`) switches the mode for new tasks.
fn new_tasks_label(app: &App) -> String {
    format!(
        " New Tasks: {} │",
        mode_name(app.ed.project().new_tasks_are_manual)
    )
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let help = "n add · d dur · p dep · m manual/auto · Tab indent · Enter rename · x del · Ctrl+F find · Ctrl+Z undo · Ctrl+S save · T theme · q quit";
    let text = if app.status.is_empty() {
        help.to_string()
    } else {
        app.status.clone()
    };
    let mut spans = vec![Span::styled(
        new_tasks_label(app),
        Style::default().fg(Color::Yellow),
    )];
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
    fn save_as_prompt_retains_binding_on_failure_and_adds_native_extension() {
        let dir = std::env::temp_dir().join(format!("yppxy-save-prompt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // The prompt only opens for an untitled plan (Save on an unbound file);
        // backstage Save As with a bound path is covered by
        // `project_saves_refuse_mpp_and_other_unsupported_formats`.
        let mut app = App::new(new_project(), None, false);
        app.ed.rename(1, "Unsaved change").unwrap();
        app.save();
        app.prompt.as_mut().unwrap().buf = dir.join("plan.mpp").to_string_lossy().into_owned();
        on_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.path, None);
        assert!(app.ed.dirty());
        assert!(app.status.contains("Project schedules can only be saved"));
        assert!(!dir.join("plan.mpp").exists());
        app.save();
        let target = dir.join("untitled");
        app.prompt.as_mut().unwrap().buf = target.to_string_lossy().into_owned();
        on_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let actual = target.with_extension("yppx");
        assert_eq!(app.path.as_deref(), actual.to_str());
        assert!(!app.ed.dirty());
        assert_eq!(
            load(actual.to_str().unwrap()).unwrap().tasks[0].name,
            "Unsaved change"
        );
        assert!(!target.exists());
        std::fs::remove_file(actual).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn project_saves_refuse_mpp_and_other_unsupported_formats() {
        const SAVE_FORMAT_ERROR: &str =
            "Project schedules can only be saved as .yppx or .xml (MSPDI)";
        let dir = std::env::temp_dir().join(format!("yppxy-save-formats-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let project = new_project();
        for name in ["plan.mpp", "plan.MPP", "plan.txt"] {
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
        let extensionless = dir.join("plan");
        let actual = save_to(&project, extensionless.to_str().unwrap()).unwrap();
        assert_eq!(actual, extensionless.with_extension("yppx"));
        assert_eq!(load(actual.to_str().unwrap()).unwrap(), project);
        assert!(!extensionless.exists());
        std::fs::remove_file(actual).unwrap();
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
    fn task_grid_shows_derived_summary_duration() {
        use ratatui::backend::TestBackend;
        let proj =
            projcore::mspdi::read_mspdi(include_str!("../../corpus/mspdi/10-summary.xml")).unwrap();
        let mut app = App::new(proj, None, false);
        let b = app.ed.project().tasks[2].uid;
        app.ed.set_duration_min(b, 1440).unwrap();
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| draw_body(f, f.area(), &mut app)).unwrap();
        let buf = term.backend().buffer();
        let row: String = (1..45)
            .map(|x| buf.cell((x, app.list_y0)).unwrap().symbol())
            .collect();
        assert_eq!(
            row.split_whitespace().collect::<Vec<_>>(),
            ["▾", "Phase", "4d", "0d"]
        );
    }

    #[test]
    fn task_grid_and_gantt_leave_a_blank_row_empty() {
        use ratatui::backend::TestBackend;
        let proj =
            projcore::mspdi::read_mspdi(include_str!("../../corpus/mspdi/20-task-fields.xml"))
                .unwrap();
        assert!(proj.tasks[2].is_null);
        let mut app = App::new(proj, None, false);
        let mut term = Terminal::new(TestBackend::new(100, 20)).unwrap();
        term.draw(|f| draw_body(f, f.area(), &mut app)).unwrap();
        let buf = term.backend().buffer();
        let row = |y, xs: std::ops::Range<u16>| -> String {
            xs.map(|x| buf.cell((x, y)).unwrap().symbol()).collect()
        };
        let (above, blank) = (app.list_y0 + 1, app.list_y0 + 2);
        assert_eq!(
            row(above, 1..45).split_whitespace().collect::<Vec<_>>(),
            ["•", "Excavate", "2d", "0d"]
        );
        assert_eq!(row(blank, 1..45).trim(), "");
        // No bar or milestone in the Gantt; weekend shading only.
        let gantt = row(blank, app.gantt_x0..99);
        assert!(gantt.chars().all(|c| c == ' ' || c == '·'), "{gantt}");
    }

    #[test]
    fn preview_does_not_count_or_draw_a_blank_row() {
        let proj =
            projcore::mspdi::read_mspdi(include_str!("../../corpus/mspdi/20-task-fields.xml"))
                .unwrap();
        let sched = projcore::schedule::schedule(&proj);
        let lines = project_preview(&proj, &sched);
        assert!(
            lines.contains(&"Tasks:   3 (2 critical)".to_string()),
            "{lines:?}"
        );
        let rows: Vec<_> = lines.iter().skip(5).map(|l| l.trim()).collect();
        assert_eq!(rows, ["▾ Phase", "• Excavate", "", "• Pour", "• Inspect"]);
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
            None,
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
    fn a_manual_summary_row_marks_its_rollup_outside_its_own_span() {
        // Monday 2026-01-05 origin; the summary spans days 2..=3, its
        // subtasks days 1..=6.
        let origin = DateTime::from_ymd_hm(2026, 1, 5, 8, 0).day_number();
        let row = build_gantt_row(9, 0, origin, 2, 3, false, true, false, Some((1, 6)), true);
        let cells: Vec<&str> = row.spans.iter().map(|s| &*s.content).collect();
        assert_eq!(cells, [" ", "╍", "▟", "▟", "╍", "╍", "╍", " ", " "]);
        let fg = |col: usize| row.spans[col].style.fg;
        assert_eq!(fg(1), Some(SUMMARY), "before its start: no warning");
        for col in 4..=6 {
            assert_eq!(fg(col), Some(WARNING), "day {col} runs past its finish");
        }
        // Weekend days inside the late rollup are marked too.
        assert_ne!(cells[5], "·");
        // An auto summary (or one within its own span) draws no rollup.
        let row = build_gantt_row(9, 0, origin, 1, 6, false, true, false, None, false);
        assert!(row.spans.iter().all(|s| s.style.fg != Some(WARNING)));
        // An overrun within the finish day warns on that day's cell.
        let row = build_gantt_row(9, 0, origin, 2, 3, false, true, false, Some((2, 3)), true);
        let warned: Vec<usize> = (0..9)
            .filter(|&col| row.spans[col].style.fg == Some(WARNING))
            .collect();
        assert_eq!(warned, [3]);
        assert_eq!(row.spans[3].content, "▟");
        // Unwarned, the same row is clean.
        let row = build_gantt_row(9, 0, origin, 2, 3, false, true, false, Some((2, 3)), false);
        assert!(row.spans.iter().all(|s| s.style.fg != Some(WARNING)));
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
            predecessors: vec![Predecessor::fs(1)],
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
        for tab in ["File", "Task", "Resource", "Report", "Project", "View"] {
            assert!(s.contains(tab), "tab {tab} missing");
        }
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
    fn status_line_shows_partial_units_while_initials_use_plain_names() {
        let mut proj = new_project();
        proj.resources.push(projcore::model::Resource {
            uid: 1,
            id: 1,
            name: "Bob".into(),
            max_units: 0.5,
            ..Default::default()
        });
        let mut app = App::new(proj, Some("plan.yppx".into()), false);
        app.assign_resource("Bob");
        app.assign_resource("Zed[25%]");
        let s = buffer_text(&mut app, 110, 24);
        assert!(s.contains("Bob[50%], Zed[25%]"), "{s}");
        assert!(s.contains("·BZ"), "{s}");
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
    fn n_on_a_summary_inserts_its_first_child() {
        let mut app = App::new(new_project(), Some("plan.yppx".into()), false);
        app.add_task();
        app.indent(1); // task 2 under task 1
        app.ed.select(0);
        on_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        );
        let rows: Vec<_> = app
            .ed
            .project()
            .tasks
            .iter()
            .map(|t| (t.uid, t.outline_level, t.summary))
            .collect();
        assert_eq!(rows, [(1, 1, true), (3, 2, false), (2, 2, false)]);
        assert_eq!(app.ed.selected_uid(), Some(3));
    }

    #[test]
    fn deleting_a_summary_asks_first_and_takes_its_subtasks() {
        let mut app = App::new(new_project(), Some("plan.yppx".into()), false);
        app.add_task();
        app.add_task();
        for row in [1, 2] {
            app.ed.select(row);
            app.indent(1); // tasks 2 and 3 under task 1
        }
        let before = app.ed.project().clone();
        let depth = app.ed.undo_depth();
        let delete = || KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE);

        // A leaf goes at once.
        on_key(&mut app, delete());
        assert!(app.confirm.is_none());
        assert_eq!(app.ed.project().tasks.len(), 2);
        assert!(app.ed.undo());

        // A summary asks; No/Esc keeps everything, with no undo entry.
        app.ed.select(0);
        on_key(&mut app, delete());
        let prompt = app.confirm.as_ref().unwrap().prompt().to_string();
        assert_eq!(prompt, "Delete 'New task' and its 2 subtasks?");
        on_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.confirm.is_none());
        assert_eq!(app.ed.project(), &before);
        assert_eq!(
            (app.ed.undo_depth(), app.ed.redo_depth()),
            (depth, 1),
            "history holds only the undone leaf delete"
        );

        // Yes deletes the summary and its subtasks in one undo step.
        on_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        on_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
        );
        assert!(app.confirm.is_none());
        assert!(!app.quit);
        assert!(app.ed.project().tasks.is_empty());
        assert!(app.ed.undo());
        assert_eq!(app.ed.project(), &before);
    }

    #[test]
    fn a_summary_delete_closes_any_text_prompt_under_its_modal() {
        let mut app = App::new(new_project(), Some("plan.yppx".into()), false);
        app.add_task();
        app.indent(1);
        app.ed.select(0);
        let before = app.ed.project().clone();
        // Ribbon clicks reach `apply_act` while a text prompt is open.
        app.apply_act(Act::Rename);
        assert!(app.prompt.is_some());
        app.apply_act(Act::DeleteTask);
        assert!(
            app.prompt.is_none(),
            "the prompt would take the modal's keys"
        );
        on_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.confirm.is_none(), "Esc reaches the visible question");
        assert_eq!(app.ed.project(), &before);

        // File ▸ Exit over a prompt is the same case.
        app.apply_act(Act::Rename);
        app.request_exit();
        assert!(app.prompt.is_none());
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

    fn click(app: &mut App, x: u16, y: u16) {
        on_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            },
        );
    }

    /// Row `y` of a `w`-wide frame, as text.
    fn frame_row(app: &mut App, w: u16, y: u16) -> String {
        buffer_text(app, w, 22)
            .chars()
            .skip(y as usize * w as usize)
            .take(w as usize)
            .collect()
    }

    #[test]
    fn calculate_project_reports_automatic_scheduling() {
        let mut app = App::new(new_project(), Some("plan.yppx".into()), false);
        app.apply_act(Act::CalculateProject);
        assert_eq!(app.status, "Rescheduled (automatic on every edit)");
    }

    #[test]
    fn level_all_and_clear_leveling_are_idempotent() {
        let mut app = App::new(new_project(), Some("plan.yppx".into()), false);
        let on = "Resource leveling ON — bars delayed to fit resource capacity";
        for (act, leveled, status) in [
            (Act::LevelAll, true, on),
            (Act::LevelAll, true, on),
            (Act::ClearLeveling, false, "Resource leveling OFF"),
            (Act::ClearLeveling, false, "Resource leveling OFF"),
        ] {
            app.apply_act(act);
            assert_eq!(app.ed.leveled(), leveled, "{act:?}");
            assert_eq!(app.status, status, "{act:?}");
        }
        on_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT),
        );
        assert!(app.ed.leveled(), "L still toggles");
        buffer_text(&mut app, 110, 22);
        assert!(
            app.ribbon.toggle_on(Act::LevelAll),
            "Level All drawn active"
        );
    }

    #[test]
    fn tab_strip_theme_button_toggles_theme() {
        let mut app = App::new(new_project(), Some("plan.yppx".into()), false);
        let light = app.light;
        let row0 = frame_row(&mut app, 100, 0);
        let (a, b) = theme_btn_cols(100, app.ribbon.width()).unwrap();
        assert_eq!(
            row0.chars().skip(a as usize).collect::<String>().trim_end(),
            THEME_BTN
        );
        click(&mut app, a, 0);
        assert_eq!(app.light, !light);
        click(&mut app, b - 1, 0);
        assert_eq!(app.light, light);
        // A tab click still switches tabs and leaves the theme alone.
        let x = (0..100)
            .find(|&x| matches!(app.ribbon.hit(x, 0, false), ribbon::Hit::Tab(2)))
            .unwrap();
        click(&mut app, x, 0);
        assert_eq!(app.ribbon.active_tab(), 2);
        assert_eq!(app.light, light);
    }

    #[test]
    fn theme_button_hidden_when_narrow() {
        let mut app = App::new(new_project(), Some("plan.yppx".into()), false);
        // Six tabs end at column 50: the button needs 60 columns.
        assert!(theme_btn_cols(60, app.ribbon.width()).is_some());
        assert_eq!(theme_btn_cols(59, app.ribbon.width()), None);
        let light = app.light;
        assert!(!frame_row(&mut app, 59, 0).contains("Theme"));
        for x in 51..59 {
            click(&mut app, x, 0);
        }
        assert_eq!(app.light, light);
    }

    #[test]
    fn t_key_toggles_theme() {
        let mut app = App::new(new_project(), Some("plan.yppx".into()), false);
        let light = app.light;
        on_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('T'), KeyModifiers::SHIFT),
        );
        assert_eq!(app.light, !light);
        on_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('T'), KeyModifiers::SHIFT),
        );
        assert_eq!(app.light, light);
    }

    #[test]
    fn ribbon_find_opens_find_prompt() {
        let mut app = App::new(new_project(), Some("plan.yppx".into()), false);
        app.apply_act(Act::Find);
        let p = app.prompt.as_ref().expect("Find prompt");
        assert!(matches!(p.kind, PromptKind::Find));
        assert_eq!(p.label, "Find");
    }

    fn press(app: &mut App, c: char) {
        let m = if c.is_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        on_key(app, KeyEvent::new(KeyCode::Char(c), m));
    }

    fn two_tasks() -> App {
        let mut proj = new_project();
        proj.tasks.push(Task {
            uid: 2,
            id: 2,
            name: "Build".into(),
            outline_level: 1,
            duration_min: 960,
            ..Task::default()
        });
        App::new(proj, Some("plan.yppx".into()), false)
    }

    #[test]
    fn m_switches_the_selected_task_between_manual_and_auto() {
        let mut app = two_tasks();
        app.ed.select(1);
        let start = app.ed.disp_start(2);
        press(&mut app, 'm');
        let t = app.ed.project().task(2).unwrap();
        assert!(t.manual);
        assert_eq!(t.manual_start, start);
        assert_eq!(app.status, "Manually Scheduled");
        assert_eq!(app.ed.undo_depth(), 1);
        press(&mut app, 'm');
        assert!(!app.ed.project().task(2).unwrap().manual);
        assert_eq!(app.status, "Auto Scheduled");
        app.undo();
        assert!(app.ed.project().task(2).unwrap().manual);
        assert!(!app.ed.project().task(1).unwrap().manual);
    }

    #[test]
    fn ribbon_schedules_the_task_and_shows_its_mode() {
        let mut app = two_tasks();
        app.apply_act(Act::ManuallySchedule);
        assert!(app.ed.project().task(1).unwrap().manual);
        buffer_text(&mut app, 110, 24);
        assert!(app.ribbon.toggle_on(Act::ManuallySchedule));
        assert!(!app.ribbon.toggle_on(Act::AutoSchedule));
        app.apply_act(Act::AutoSchedule);
        assert!(!app.ed.project().task(1).unwrap().manual);
        buffer_text(&mut app, 110, 24);
        assert!(app.ribbon.toggle_on(Act::AutoSchedule));
        assert!(!app.ribbon.toggle_on(Act::ManuallySchedule));
    }

    #[test]
    fn a_manual_task_shows_a_pin_and_the_columns_stay_aligned() {
        let mut app = two_tasks();
        app.ed.set_manual(2, true).unwrap();
        let (w, h) = (110u16, 22u16);
        let text = buffer_text(&mut app, w, h);
        let rows: Vec<String> = (0..h as usize)
            .map(|y| text.chars().skip(y * w as usize).take(w as usize).collect())
            .collect();
        let row = |needle: &str| rows.iter().find(|r| r.contains(needle)).unwrap().clone();
        let (auto, manual) = (row("New task"), row("Build"));
        assert!(manual.contains("📌") && !auto.contains("📌"));
        // The name starts in the same column either way, and the Dur column
        // lines up with its header. A buffer cell holds one symbol, so a
        // char index is a column here.
        let col = |r: &str, s: &str| r.find(s).map(|b| r[..b].chars().count()).unwrap();
        let header = row("Slack");
        assert_eq!(col(&auto, "New task"), col(&manual, "Build"));
        assert_eq!(col(&header, "Task"), col(&auto, "New task") - 2);
        assert_eq!(col(&auto, "1d"), col(&manual, "2d"));
        // Right-aligned: "Dur" and "1d" end in the same column.
        assert_eq!(col(&header, "Dur") + 3, col(&auto, "1d") + 2);
    }

    #[test]
    fn the_status_line_shows_and_switches_the_new_task_mode() {
        let mut app = two_tasks();
        let (w, h) = (110u16, 22u16);
        assert!(frame_row(&mut app, w, h - 1).starts_with(" New Tasks: Auto Scheduled"));
        press(&mut app, 'M');
        assert!(app.ed.project().new_tasks_are_manual);
        assert_eq!(app.ed.undo_depth(), 1);
        assert!(frame_row(&mut app, w, h - 1).starts_with(" New Tasks: Manually Scheduled"));
        // A new task follows the plan's mode.
        app.add_task();
        assert!(app.ed.project().tasks[app.ed.sel()].manual);
        // A click on the segment switches it back.
        frame_row(&mut app, w, h - 1);
        click(&mut app, 3, h - 1);
        assert!(!app.ed.project().new_tasks_are_manual);
        // A click past it does not.
        frame_row(&mut app, w, h - 1);
        click(&mut app, w - 2, h - 1);
        assert!(!app.ed.project().new_tasks_are_manual);
    }
}
