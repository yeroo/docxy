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

use std::io;
use std::process::ExitCode;

mod backstage;
mod ribbon;

use backstage::{Backstage, Item, Pane};
use ribbon::{Act, Ribbon};

use projcore::datetime::DateTime;
use projcore::model::{Assignment, ConstraintType, LinkType, Predecessor, Project, Resource, Task};
use projcore::schedule::{level, schedule, Leveled, Schedule};
use projcore::{gantt, mspdi, yppx};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
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
        if let Err(e) = std::fs::write(out, gantt::to_markdown(&proj, &s)) {
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
    let mut out = Args { input: None, gantt_md: None, save: None, help: false, vim: false };
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

/// Build a partial project from a legacy binary `.mpp`. Decodes the documented
/// metadata (title/author/…) and the **task names** (from the VarMeta/Var2Data
/// container). The numeric fields — dates, durations, links — live in the
/// undocumented Fixed/Var data blocks and aren't parsed yet, so each task gets a
/// default 1-day duration and no dependencies. It opens with the real WBS, and
/// Save As converts it to `.yppx`/MSPDI.
fn project_from_mpp(bytes: &[u8]) -> Result<Project, String> {
    let info = mppread::read_mpp(bytes)?;
    let name = [info.title.clone(), info.subject.clone(), info.company.clone()]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| "Imported project".into());
    let tasks = mppread::mpp::task_names(bytes)
        .into_iter()
        .enumerate()
        .map(|(i, n)| Task {
            uid: i as i32 + 1,
            id: i as i32 + 1,
            name: n,
            outline_level: 1,
            duration_min: 480,
            ..Task::default()
        })
        .collect();
    Ok(Project {
        name,
        title: info.title,
        start_date: Some(next_monday()),
        tasks,
        ..Project::default()
    })
}

fn save_to(proj: &Project, path: &str) -> Result<(), String> {
    if path.ends_with(".yppx") {
        std::fs::write(path, yppx::write_yppx(proj)).map_err(|e| e.to_string())
    } else {
        std::fs::write(path, mspdi::write_mspdi(proj)).map_err(|e| e.to_string())
    }
}

/// A fresh schedule: one task so the chart has something to show.
fn new_project() -> Project {
    let mut p = Project { name: "Untitled".into(), ..Project::default() };
    p.start_date = Some(next_monday());
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

/// A stable, dependency-free "next Monday 08:00" so a new project has a sane
/// anchor. (projcore forbids `Date::now`; we pick a fixed sensible Monday.)
fn next_monday() -> DateTime {
    DateTime::from_ymd_hm(2026, 1, 5, 8, 0) // a Monday
}

/// Path to the view-prefs file: `$XDG_CONFIG_HOME/yppxy/prefs` (or `~/.config`).
fn prefs_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
    Some(base.join("yppxy").join("prefs"))
}

/// Load the persisted theme preference (light/dark); defaults to dark.
fn load_theme_pref() -> bool {
    let Some(p) = prefs_path() else { return false };
    let Ok(text) = std::fs::read_to_string(p) else { return false };
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
    let _ = std::fs::write(p, format!("theme={}\n", if light { "light" } else { "dark" }));
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

struct Prompt {
    kind: PromptKind,
    label: String,
    buf: String,
}

struct App {
    proj: Project,
    path: Option<String>,
    dirty: bool,
    sel: usize,     // selected task index
    top: usize,     // first visible task row
    hscroll: i64,   // gantt horizontal scroll in days from project start
    sched: Schedule,
    base_day: i64,  // day-number of project start (gantt column origin)
    prompt: Option<Prompt>,
    status: String,
    quit: bool,
    // ribbon + backstage + chrome
    ribbon: Ribbon,
    rfocus: ribbon::Focus,
    backstage: Option<Backstage>,
    light: bool,
    start: bool,
    // undo/redo, find, vim
    undo: Vec<Project>,
    redo: Vec<Project>,
    find_query: String,
    vim: bool,
    // resource leveling overlay
    leveled: bool,
    level: Option<Leveled>,
    // geometry recorded during draw for mouse hit-testing
    list_y0: u16,   // absolute y of the first task row
    list_left_w: u16, // width of the task pane (left of the gantt)
    gantt_x0: u16,  // absolute x where the gantt inner area begins
}

const RIBBON_H: u16 = 6; // tab strip (1) + body (5: border, 2 rows, separator, titles)

impl App {
    fn new(proj: Project, path: Option<String>, vim: bool) -> App {
        let start = path.is_none();
        let mut app = App {
            proj,
            path,
            dirty: false,
            sel: 0,
            top: 0,
            hscroll: 0,
            sched: empty_schedule(),
            base_day: 0,
            prompt: None,
            status: String::new(),
            quit: false,
            ribbon: Ribbon::new(),
            rfocus: ribbon::Focus::None,
            backstage: None,
            light: load_theme_pref(),
            start,
            list_y0: 0,
            list_left_w: 0,
            gantt_x0: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            find_query: String::new(),
            vim,
            leveled: false,
            level: None,
        };
        app.reschedule();
        app
    }

    /// Push the current project onto the undo stack before a mutation.
    fn snapshot(&mut self) {
        self.undo.push(self.proj.clone());
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.proj, prev));
            self.after_history();
            self.status = "Undo".into();
        } else {
            self.status = "Nothing to undo".into();
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.proj, next));
            self.after_history();
            self.status = "Redo".into();
        } else {
            self.status = "Nothing to redo".into();
        }
    }

    fn after_history(&mut self) {
        self.dirty = true;
        self.sel = self.sel.min(self.proj.tasks.len().saturating_sub(1));
        self.reschedule();
    }

    fn open_backstage(&mut self) {
        let dir = self
            .path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).parent().map(|d| d.to_path_buf()))
            .filter(|d| !d.as_os_str().is_empty())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        self.backstage = Some(Backstage::open(dir));
        self.rfocus = ribbon::Focus::None;
    }

    fn toggle_milestone(&mut self) {
        if self.sel < self.proj.tasks.len() {
            self.snapshot();
            let t = &mut self.proj.tasks[self.sel];
            if t.duration_min == 0 {
                t.duration_min = 480;
                t.milestone = false;
            } else {
                t.duration_min = 0;
                t.milestone = true;
            }
            self.mark_dirty();
            self.reschedule();
        }
    }

    fn theme_toggle(&mut self) {
        self.light = !self.light;
        save_theme_pref(self.light);
    }

    /// Find the next task whose name contains `query` (case-insensitive),
    /// searching from just after the current selection and wrapping around.
    fn find(&mut self, query: &str) {
        let q = query.trim().to_lowercase();
        if !q.is_empty() {
            self.find_query = q;
        }
        if self.find_query.is_empty() || self.proj.tasks.is_empty() {
            return;
        }
        let n = self.proj.tasks.len();
        for step in 1..=n {
            let i = (self.sel + step) % n;
            if self.proj.tasks[i].name.to_lowercase().contains(&self.find_query) {
                self.sel = i;
                self.status = format!("Found '{}'  (F3 next)", self.find_query);
                return;
            }
        }
        self.status = format!("No task matching '{}'", self.find_query);
    }

    /// Set a date constraint on the selected task from text like
    /// `SNET 2026-03-05`, `MSO 2026-03-05`, or `none` / `asap`.
    fn set_constraint(&mut self, text: &str) {
        if self.sel >= self.proj.tasks.len() {
            return;
        }
        let mut it = text.split_whitespace();
        let kind = it.next().unwrap_or("").to_ascii_lowercase();
        let ctype = match kind.as_str() {
            "none" | "asap" => ConstraintType::AsSoonAsPossible,
            "alap" => ConstraintType::AsLateAsPossible,
            "snet" => ConstraintType::StartNoEarlierThan,
            "snlt" => ConstraintType::StartNoLaterThan,
            "fnet" => ConstraintType::FinishNoEarlierThan,
            "fnlt" => ConstraintType::FinishNoLaterThan,
            "mso" => ConstraintType::MustStartOn,
            "mfo" => ConstraintType::MustFinishOn,
            _ => {
                self.status = "Constraint: TYPE [date] — SNET/SNLT/FNET/FNLT/MSO/MFO/ALAP/none".into();
                return;
            }
        };
        // Constraints other than ASAP/ALAP need a date.
        let needs_date = !matches!(ctype, ConstraintType::AsSoonAsPossible | ConstraintType::AsLateAsPossible);
        let date = it.next().and_then(DateTime::parse_mspdi);
        if needs_date && date.is_none() {
            self.status = format!("{} needs a date, e.g. {kind} 2026-03-05", kind.to_uppercase());
            return;
        }
        self.snapshot();
        let t = &mut self.proj.tasks[self.sel];
        t.constraint = ctype;
        t.constraint_date = if needs_date { date } else { None };
        self.mark_dirty();
        self.reschedule();
        self.status = format!("Constraint set: {}", kind.to_uppercase());
    }

    /// Assign a resource (by name, created on first use) to the selected task.
    /// An empty name clears the task's assignments.
    fn assign_resource(&mut self, name: &str) {
        if self.sel >= self.proj.tasks.len() {
            return;
        }
        let name = name.trim().to_string();
        let task_uid = self.proj.tasks[self.sel].uid;

        if name.is_empty() {
            if self.proj.assignments.iter().any(|a| a.task_uid == task_uid) {
                self.snapshot();
                self.proj.assignments.retain(|a| a.task_uid != task_uid);
                self.mark_dirty();
                self.status = "Cleared the task's resources".into();
            }
            return;
        }

        let existing = self.proj.resources.iter().find(|r| r.name.eq_ignore_ascii_case(&name)).map(|r| r.uid);
        if let Some(rid) = existing {
            if self.proj.assignments.iter().any(|a| a.task_uid == task_uid && a.resource_uid == rid) {
                self.status = format!("{name} is already assigned");
                return;
            }
        }
        // A real change will happen — take a single snapshot.
        self.snapshot();
        let rid = existing.unwrap_or_else(|| {
            let uid = self.proj.resources.iter().map(|r| r.uid).max().unwrap_or(0) + 1;
            let id = self.proj.resources.len() as i32 + 1;
            self.proj.resources.push(Resource { uid, id, name: name.clone(), is_work: true, max_units: 1.0, calendar_uid: None });
            uid
        });
        let auid = self.proj.assignments.iter().map(|a| a.uid).max().unwrap_or(0) + 1;
        let work = self.proj.tasks[self.sel].duration_min;
        self.proj.assignments.push(Assignment { uid: auid, task_uid, resource_uid: rid, units: 1.0, work_min: work });
        self.mark_dirty();
        self.status = format!("Assigned {name}");
    }

    /// Snapshot the current computed schedule as the baseline (the saved plan).
    fn set_baseline(&mut self) {
        self.snapshot();
        for t in &mut self.proj.tasks {
            if let Some(r) = self.sched.get(t.uid) {
                t.baseline_start = Some(r.early_start);
                t.baseline_finish = Some(r.early_finish);
            }
        }
        self.mark_dirty();
        self.status = "Baseline set — variance now shows in the header".into();
    }

    /// Run a vim `:` command line (`w`, `q`, `wq`/`x`, `q!`, `e <path>`).
    fn vim_run(&mut self, cmd: &str) {
        match cmd.trim() {
            "w" => self.save(),
            "q" => {
                if self.dirty {
                    self.status = "Unsaved changes — :q! to force, or :wq to save".into();
                } else {
                    self.quit = true;
                }
            }
            "q!" => self.quit = true,
            "wq" | "x" => {
                self.save();
                if !self.dirty {
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
                if let Some(t) = self.proj.tasks.get(self.sel) {
                    self.prompt = Some(Prompt { kind: PromptKind::Rename, label: "Rename".into(), buf: t.name.clone() });
                }
            }
            Act::Duration => {
                self.prompt = Some(Prompt { kind: PromptKind::Duration, label: "Duration".into(), buf: String::new() });
            }
            Act::AddLink => {
                self.prompt = Some(Prompt { kind: PromptKind::AddPredecessor, label: "Predecessor ID".into(), buf: String::new() });
            }
            Act::Constraint => {
                let cur = self.proj.tasks.get(self.sel).map(constraint_hint).unwrap_or_default();
                self.prompt = Some(Prompt { kind: PromptKind::Constraint, label: "Constraint".into(), buf: cur });
            }
            Act::Baseline => self.set_baseline(),
            Act::Assign => {
                self.prompt = Some(Prompt { kind: PromptKind::Assign, label: "Assign resource".into(), buf: String::new() });
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
                self.prompt = Some(Prompt { kind: PromptKind::SaveAs, label: "Save as".into(), buf: self.path.clone().unwrap_or_default() });
            }
            Act::Todo(name) => self.status = format!("{name} — not implemented yet"),
        }
    }

    fn reschedule(&mut self) {
        self.recompute_summaries();
        self.sched = schedule(&self.proj);
        self.base_day = self.sched.project_start.day_number();
        self.level = if self.leveled { Some(level(&self.proj)) } else { None };
    }

    fn toggle_level(&mut self) {
        self.leveled = !self.leveled;
        self.reschedule();
        self.status = if self.leveled {
            "Resource leveling ON — bars delayed to fit resource capacity".into()
        } else {
            "Resource leveling OFF".into()
        };
    }

    /// Displayed start of a task: leveled if leveling is on, else CPM early start.
    fn disp_start(&self, uid: i32) -> Option<DateTime> {
        match &self.level {
            Some(lv) => lv.start(uid),
            None => self.sched.get(uid).map(|r| r.early_start),
        }
    }

    fn disp_finish(&self, uid: i32) -> Option<DateTime> {
        match &self.level {
            Some(lv) => lv.finish(uid),
            None => self.sched.get(uid).map(|r| r.early_finish),
        }
    }

    /// A task is a summary when the row directly below it is deeper in the
    /// outline. Recomputed after any structural edit so rollups stay correct.
    fn recompute_summaries(&mut self) {
        let levels: Vec<u32> = self.proj.tasks.iter().map(|t| t.outline_level).collect();
        for (i, t) in self.proj.tasks.iter_mut().enumerate() {
            t.summary = levels.get(i + 1).is_some_and(|&nl| nl > levels[i]);
        }
    }

    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    // ---- edits ----

    fn add_task(&mut self) {
        self.snapshot();
        let level = self.proj.tasks.get(self.sel).map(|t| t.outline_level).unwrap_or(1);
        let uid = self.proj.tasks.iter().map(|t| t.uid).max().unwrap_or(0) + 1;
        let at = (self.sel + 1).min(self.proj.tasks.len());
        self.proj.tasks.insert(
            at,
            Task { uid, id: uid, name: "New task".into(), outline_level: level, duration_min: 480, ..Task::default() },
        );
        self.sel = at;
        self.mark_dirty();
        self.reschedule();
    }

    fn delete_task(&mut self) {
        if self.proj.tasks.is_empty() {
            return;
        }
        self.snapshot();
        let uid = self.proj.tasks[self.sel].uid;
        self.proj.tasks.remove(self.sel);
        // Drop dangling predecessor links to the removed task.
        for t in &mut self.proj.tasks {
            t.predecessors.retain(|p| p.uid != uid);
        }
        if self.sel >= self.proj.tasks.len() {
            self.sel = self.proj.tasks.len().saturating_sub(1);
        }
        self.mark_dirty();
        self.reschedule();
    }

    fn indent(&mut self, delta: i32) {
        if self.sel < self.proj.tasks.len() {
            self.snapshot();
            let t = &mut self.proj.tasks[self.sel];
            t.outline_level = (t.outline_level as i32 + delta).clamp(1, 20) as u32;
            self.mark_dirty();
            self.reschedule();
        }
    }

    fn set_duration(&mut self, text: &str) {
        if let Some(min) = parse_duration(text, &self.proj) {
            if self.sel < self.proj.tasks.len() {
                self.snapshot();
                let t = &mut self.proj.tasks[self.sel];
                t.duration_min = min;
                t.milestone = min == 0;
                self.mark_dirty();
                self.reschedule();
            }
        } else {
            self.status = format!("Couldn't read duration '{text}' (try 3d, 4h, 2w)");
        }
    }

    fn rename(&mut self, text: &str) {
        if self.sel < self.proj.tasks.len() {
            self.snapshot();
            self.proj.tasks[self.sel].name = text.to_string();
            self.mark_dirty();
        }
    }

    fn add_predecessor(&mut self, text: &str) {
        let Ok(uid) = text.trim().parse::<i32>() else {
            self.status = "Predecessor must be a task ID (number)".into();
            return;
        };
        let self_uid = self.proj.tasks[self.sel].uid;
        if uid == self_uid || !self.proj.tasks.iter().any(|t| t.uid == uid) {
            self.status = format!("No other task with ID {uid}");
            return;
        }
        if self.proj.tasks[self.sel].predecessors.iter().any(|p| p.uid == uid) {
            self.status = format!("Already depends on {uid}");
            return;
        }
        self.snapshot();
        self.proj.tasks[self.sel].predecessors.push(Predecessor { uid, link: LinkType::FinishStart, lag_min: 0 });
        self.mark_dirty();
        self.reschedule();
    }

    fn save(&mut self) {
        match self.path.clone() {
            Some(p) => match save_to(&self.proj, &p) {
                Ok(()) => {
                    self.dirty = false;
                    self.status = format!("Saved {p}");
                }
                Err(e) => self.status = format!("Save failed: {e}"),
            },
            None => {
                self.prompt = Some(Prompt { kind: PromptKind::SaveAs, label: "Save as".into(), buf: String::new() });
            }
        }
    }

    fn export_md(&mut self) {
        let out = self
            .path
            .as_deref()
            .map(|p| format!("{}.md", p.rsplit_once('.').map(|(a, _)| a).unwrap_or(p)))
            .unwrap_or_else(|| "schedule.md".into());
        match std::fs::write(&out, gantt::to_markdown(&self.proj, &self.sched)) {
            Ok(()) => self.status = format!("Exported Gantt to {out}"),
            Err(e) => self.status = format!("Export failed: {e}"),
        }
    }

    /// Load a project from disk into the app, replacing the current one.
    fn open_file(&mut self, path: &str) {
        match load(path) {
            Ok(p) => {
                self.proj = p;
                self.path = Some(path.to_string());
                self.dirty = false;
                self.sel = 0;
                self.top = 0;
                self.hscroll = 0;
                self.backstage = None;
                self.start = false;
                self.reschedule();
                let is_mpp = path.to_ascii_lowercase().ends_with(".mpp");
                self.status = if is_mpp && !self.proj.tasks.is_empty() {
                    format!("Opened {path} — {} task names decoded (.mpp dates/links pending)", self.proj.tasks.len())
                } else if is_mpp {
                    format!("Opened {path} — .mpp metadata only (no task table found)")
                } else {
                    format!("Opened {path}")
                };
            }
            Err(e) => self.status = format!("Open failed: {e}"),
        }
    }

    /// Refresh the backstage preview for the highlighted browser file.
    fn bs_update_preview(&mut self) {
        let Some(path) = self.backstage.as_ref().and_then(|b| b.selected_file()) else {
            if let Some(b) = self.backstage.as_mut() {
                b.preview.clear();
                b.preview_path = None;
            }
            return;
        };
        let already = self
            .backstage
            .as_ref()
            .map(|b| b.preview_path.as_deref() == Some(path.as_path()))
            .unwrap_or(false);
        if already {
            return;
        }
        let lines = match load(&path.to_string_lossy()) {
            Ok(p) => {
                let s = schedule(&p);
                project_preview(&p, &s)
            }
            Err(e) => vec![format!("(cannot preview: {e})")],
        };
        if let Some(b) = self.backstage.as_mut() {
            b.preview = lines;
            b.preview_path = Some(path);
            b.preview_scroll = 0;
        }
    }

    /// Activate the highlighted backstage menu item.
    fn bs_activate_item(&mut self) {
        let Some(item) = self.backstage.as_ref().map(|b| b.item) else { return };
        match item {
            Item::New => {
                self.proj = new_project();
                self.path = None;
                self.dirty = false;
                self.sel = 0;
                self.top = 0;
                self.hscroll = 0;
                self.backstage = None;
                self.reschedule();
                self.status = "New schedule".into();
            }
            Item::Open => {
                if let Some(b) = self.backstage.as_mut() {
                    b.pane = Pane::Browser;
                }
                self.bs_update_preview();
            }
            Item::Info => {
                let lines = project_preview(&self.proj, &self.sched);
                if let Some(b) = self.backstage.as_mut() {
                    b.preview = lines;
                    b.preview_path = None;
                    b.pane = Pane::Preview;
                }
            }
            Item::Save => {
                self.backstage = None;
                self.save();
            }
            Item::SaveAs => {
                let cur = self
                    .path
                    .as_deref()
                    .and_then(|p| std::path::Path::new(p).file_name().map(|f| f.to_string_lossy().into_owned()))
                    .unwrap_or_default();
                if let Some(b) = self.backstage.as_mut() {
                    b.name_input = cur;
                    b.pane = Pane::SaveAs;
                }
            }
            Item::Export => {
                self.backstage = None;
                self.export_md();
            }
            Item::Exit => self.quit = true,
        }
    }
}

/// A few summary lines for the backstage preview / Info pane.
fn project_preview(proj: &Project, sched: &Schedule) -> Vec<String> {
    let fin = sched.project_finish.parts();
    let start = sched.project_start.parts();
    let leaves = proj.tasks.iter().filter(|t| !t.summary).count();
    let crit = proj.tasks.iter().filter(|t| !t.summary && sched.get(t.uid).is_some_and(|r| r.critical)).count();
    let mut out = vec![
        format!("Project: {}", if proj.name.is_empty() { "Untitled" } else { &proj.name }),
        format!("Start:   {:04}-{:02}-{:02}", start.year, start.month, start.day),
        format!("Finish:  {:04}-{:02}-{:02}", fin.year, fin.month, fin.day),
        format!("Tasks:   {} ({crit} critical)", leaves),
        String::new(),
    ];
    for t in proj.tasks.iter().take(16) {
        let indent = "  ".repeat(t.outline_level.saturating_sub(1) as usize);
        let bullet = if t.summary { "▾" } else if t.is_milestone() { "◆" } else { "•" };
        out.push(format!("{indent}{bullet} {}", t.name));
    }
    if proj.tasks.len() > 16 {
        out.push(format!("  … {} more", proj.tasks.len() - 16));
    }
    out
}

fn empty_schedule() -> Schedule {
    // A placeholder replaced immediately by reschedule().
    schedule(&Project::default())
}

/// Parse a duration like `3`, `3d`, `4h`, `2w` into working minutes.
fn parse_duration(text: &str, proj: &Project) -> Option<i64> {
    let t = text.trim().to_lowercase();
    let (num, unit) = t
        .strip_suffix(['d', 'h', 'w', 'm'])
        .map(|n| (n, t.chars().last().unwrap()))
        .unwrap_or((t.as_str(), 'd'));
    let v: f64 = num.trim().parse().ok()?;
    Some(match unit {
        'h' => (v * 60.0).round() as i64,
        'w' => proj.days_to_minutes(v * (proj.hours_per_week / proj.hours_per_day)),
        'm' => v.round() as i64,
        _ => proj.days_to_minutes(v),
    })
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

    let res = loop {
        // Keep the terminal window title in sync: [* ]yppxy - filename.
        let want = window_title(&app.path, app.dirty);
        if want != title {
            let _ = execute!(terminal.backend_mut(), SetTitle(&want));
            title = want;
        }
        if let Err(e) = terminal.draw(|f| draw(f, &mut app)) {
            break Err(e);
        }
        match event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => on_key(&mut app, k),
            Ok(Event::Mouse(m)) => on_mouse(&mut app, m),
            Ok(_) => {}
            Err(e) => break Err(e),
        }
        if app.quit {
            break Ok(());
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    res
}

fn on_mouse(app: &mut App, m: MouseEvent) {
    let (x, y) = (m.column, m.row);
    match m.kind {
        MouseEventKind::ScrollDown => {
            if x >= app.gantt_x0 {
                app.hscroll += 2;
            } else if app.sel + 1 < app.proj.tasks.len() {
                app.sel += 1;
            }
        }
        MouseEventKind::ScrollUp => {
            if x >= app.gantt_x0 {
                app.hscroll -= 2;
            } else {
                app.sel = app.sel.saturating_sub(1);
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if app.start || app.backstage.is_some() {
                return; // modal surfaces are keyboard-driven for now
            }
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
                if idx < app.proj.tasks.len() {
                    app.sel = idx;
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

    // Modal surfaces take keys before the editor.
    if app.start {
        start_key(app, k);
        return;
    }
    if app.backstage.is_some() {
        backstage_key(app, k);
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
                app.prompt = Some(Prompt { kind: PromptKind::Find, label: "Find".into(), buf: String::new() });
            }
            KeyCode::Char('q') => app.quit = true, // Ctrl+Q: quit even if dirty
            _ => {}
        }
        return;
    }

    match k.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => {
            if app.dirty {
                app.status = "Unsaved changes — Ctrl+S to save, or Ctrl+Q to quit anyway".into();
            } else {
                app.quit = true;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => app.sel = app.sel.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            if app.sel + 1 < app.proj.tasks.len() {
                app.sel += 1;
            }
        }
        KeyCode::Home | KeyCode::Char('g') => app.sel = 0,
        KeyCode::End | KeyCode::Char('G') => app.sel = app.proj.tasks.len().saturating_sub(1),
        KeyCode::Left | KeyCode::Char('h') => app.hscroll -= 1,
        KeyCode::Right | KeyCode::Char('l') => app.hscroll += 1,
        KeyCode::Char('n') | KeyCode::Insert => app.add_task(),
        KeyCode::Delete | KeyCode::Char('x') => app.delete_task(),
        KeyCode::Tab | KeyCode::Char('>') => app.indent(1),
        KeyCode::BackTab | KeyCode::Char('<') => app.indent(-1),
        KeyCode::Enter | KeyCode::F(2) => {
            if let Some(t) = app.proj.tasks.get(app.sel) {
                app.prompt = Some(Prompt { kind: PromptKind::Rename, label: "Rename".into(), buf: t.name.clone() });
            }
        }
        KeyCode::Char('d') => {
            app.prompt = Some(Prompt { kind: PromptKind::Duration, label: "Duration".into(), buf: String::new() });
        }
        KeyCode::Char('p') => {
            app.prompt = Some(Prompt { kind: PromptKind::AddPredecessor, label: "Predecessor ID".into(), buf: String::new() });
        }
        KeyCode::Char('c') => {
            let cur = app.proj.tasks.get(app.sel).map(constraint_hint).unwrap_or_default();
            app.prompt = Some(Prompt { kind: PromptKind::Constraint, label: "Constraint".into(), buf: cur });
        }
        KeyCode::Char('b') => app.set_baseline(),
        KeyCode::Char('L') => app.toggle_level(),
        KeyCode::Char('a') => {
            app.prompt = Some(Prompt { kind: PromptKind::Assign, label: "Assign resource".into(), buf: String::new() });
        }
        KeyCode::F(3) => app.find(""), // repeat the last search
        KeyCode::F(9) => app.rfocus = ribbon::Focus::Tab(app.ribbon.active_tab()),
        // Vim niceties (only when launched with --vim).
        KeyCode::Char(':') if app.vim => {
            app.prompt = Some(Prompt { kind: PromptKind::VimCommand, label: ":".into(), buf: String::new() });
        }
        KeyCode::Char('u') if app.vim => app.undo(),
        KeyCode::Char('/') if app.vim => {
            app.prompt = Some(Prompt { kind: PromptKind::Find, label: "/".into(), buf: String::new() });
        }
        _ => {}
    }
}

// ---- start screen, ribbon, backstage key handling ---------------------------

fn start_key(app: &mut App, k: KeyEvent) {
    match k.code {
        KeyCode::Char('o') | KeyCode::Char('O') => {
            app.start = false;
            app.open_backstage();
        }
        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => app.quit = true,
        _ => app.start = false, // Enter / N / any other key: start editing
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

fn backstage_key(app: &mut App, k: KeyEvent) {
    // Read pane/item without holding a borrow across app-method calls.
    let Some((pane, item)) = app.backstage.as_ref().map(|b| (b.pane, b.item)) else { return };
    match pane {
        Pane::SaveAs => match k.code {
            KeyCode::Esc => set_pane(app, Pane::Menu),
            KeyCode::Enter => {
                let name = app.backstage.as_ref().map(|b| b.name_input.trim().to_string()).unwrap_or_default();
                if !name.is_empty() {
                    let dir = app.backstage.as_ref().map(|b| b.dir.clone());
                    if let Some(dir) = dir {
                        app.path = Some(dir.join(&name).to_string_lossy().into_owned());
                        app.backstage = None;
                        app.save();
                    }
                }
            }
            KeyCode::Backspace => {
                if let Some(b) = app.backstage.as_mut() {
                    b.name_input.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(b) = app.backstage.as_mut() {
                    b.name_input.push(c);
                }
            }
            _ => {}
        },
        Pane::Browser => match k.code {
            KeyCode::Esc | KeyCode::Left => set_pane(app, Pane::Menu),
            KeyCode::Up => {
                if let Some(b) = app.backstage.as_mut() {
                    b.move_sel(false);
                }
                app.bs_update_preview();
            }
            KeyCode::Down => {
                if let Some(b) = app.backstage.as_mut() {
                    b.move_sel(true);
                }
                app.bs_update_preview();
            }
            KeyCode::Backspace => {
                if let Some(b) = app.backstage.as_mut() {
                    b.go_up();
                }
                app.bs_update_preview();
            }
            KeyCode::Enter => {
                let opened = app.backstage.as_mut().and_then(|b| b.enter());
                match opened {
                    Some(path) => app.open_file(&path.to_string_lossy()),
                    None => app.bs_update_preview(),
                }
            }
            _ => {}
        },
        Pane::Menu | Pane::Preview => match k.code {
            KeyCode::Esc => app.backstage = None, // back to the schedule
            KeyCode::Up => {
                if let Some(b) = app.backstage.as_mut() {
                    let i = backstage::ITEMS.iter().position(|&x| x == b.item).unwrap_or(0);
                    b.item = backstage::ITEMS[i.saturating_sub(1)];
                }
            }
            KeyCode::Down => {
                if let Some(b) = app.backstage.as_mut() {
                    let i = backstage::ITEMS.iter().position(|&x| x == b.item).unwrap_or(0);
                    b.item = backstage::ITEMS[(i + 1).min(backstage::ITEMS.len() - 1)];
                }
            }
            KeyCode::Right if item == Item::Open => {
                set_pane(app, Pane::Browser);
                app.bs_update_preview();
            }
            KeyCode::Enter => app.bs_activate_item(),
            _ => {}
        },
    }
}

fn set_pane(app: &mut App, pane: Pane) {
    if let Some(b) = app.backstage.as_mut() {
        b.pane = pane;
    }
}

// ---- drawing ----------------------------------------------------------------

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    if app.start {
        draw_start(f, area);
        return;
    }
    if app.backstage.is_some() {
        draw_backstage(f, area, app);
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
    if app.leveled {
        toggles.push(Act::Level);
    }
    app.ribbon.set_toggles(toggles);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // ribbon tab strip
            Constraint::Length(5), // ribbon body (border, 2 rows, separator, titles)
            Constraint::Length(1), // project header
            Constraint::Min(3),    // tasks | gantt
            Constraint::Length(1), // status / hint
        ])
        .split(area);
    f.render_widget(Paragraph::new(app.ribbon.render_tabs(app.rfocus)), rows[0]);
    f.render_widget(Paragraph::new(app.ribbon.render_body(app.rfocus)), rows[1]);
    draw_header(f, rows[2], app);
    draw_body(f, rows[3], app);
    if app.rfocus != ribbon::Focus::None {
        f.render_widget(Paragraph::new(app.ribbon.render_hint(app.rfocus, area.width)), rows[4]);
    } else {
        draw_status(f, rows[4], app);
    }
    if app.prompt.is_some() {
        draw_prompt(f, area, app);
    }
}

fn draw_start(f: &mut Frame, area: Rect) {
    f.render_widget(Clear, area);
    let dim = Style::default().add_modifier(Modifier::DIM);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled("   yppxy", bold)),
        Line::from(Span::styled("   terminal project scheduler — Critical Path Method + live Gantt", dim)),
        Line::from(""),
        Line::from("   [N] or Enter    New schedule"),
        Line::from("   [O]             Open a project  (.xml · .yppx · .mpp)"),
        Line::from("   [Q] or Esc      Quit"),
        Line::from(""),
        Line::from(Span::styled("   Tip: press F9 for the ribbon, Alt+F for the File menu.", dim)),
    ];
    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Welcome ")),
        area,
    );
}

fn draw_backstage(f: &mut Frame, area: Rect, app: &App) {
    let Some(bs) = app.backstage.as_ref() else { return };
    f.render_widget(Clear, area);
    let outer = Block::default().borders(Borders::ALL).title(" File ");
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(16), Constraint::Min(20)])
        .split(inner);

    // Left: the vertical menu.
    let mut menu: Vec<Line> = Vec::new();
    for &it in backstage::ITEMS.iter() {
        let selected = it == bs.item;
        let style = if selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        menu.push(Line::from(Span::styled(format!(" {} ", it.label()), style)));
    }
    f.render_widget(Paragraph::new(menu), cols[0]);

    // Right: content depends on the pane / item.
    let body = cols[1];
    match bs.pane {
        Pane::Browser => draw_bs_browser(f, body, bs),
        Pane::SaveAs => {
            let dir = bs.dir.to_string_lossy();
            let lines = vec![
                Line::from(Span::styled("Save As", Style::default().add_modifier(Modifier::BOLD))),
                Line::from(""),
                Line::from(format!("Folder: {dir}")),
                Line::from(vec![
                    Span::raw("Name:   "),
                    Span::raw(bs.name_input.clone()),
                    Span::styled("▏", Style::default().fg(Color::Gray)),
                ]),
                Line::from(""),
                Line::from(Span::styled("Use .yppx (native) or .xml (MSPDI). Enter saves · Esc back", Style::default().add_modifier(Modifier::DIM))),
            ];
            f.render_widget(Paragraph::new(lines), body);
        }
        Pane::Menu | Pane::Preview => {
            if bs.item == Item::Open {
                draw_bs_browser(f, body, bs);
            } else if bs.item == Item::Info || bs.pane == Pane::Preview {
                let lines: Vec<Line> = bs.preview.iter().map(|s| Line::from(s.clone())).collect();
                f.render_widget(Paragraph::new(lines), body);
            } else {
                let hint = match bs.item {
                    Item::New => "Start a fresh schedule.",
                    Item::Save => "Save to the current file.",
                    Item::SaveAs => "Save under a new name.",
                    Item::Export => "Export a Markdown/Mermaid Gantt chart.",
                    Item::Exit => "Leave yppxy.",
                    _ => "",
                };
                f.render_widget(
                    Paragraph::new(vec![
                        Line::from(Span::styled(bs.item.label(), Style::default().add_modifier(Modifier::BOLD))),
                        Line::from(""),
                        Line::from(hint),
                        Line::from(""),
                        Line::from(Span::styled("Enter to apply · Esc to close", Style::default().add_modifier(Modifier::DIM))),
                    ]),
                    body,
                );
            }
        }
    }
}

fn draw_bs_browser(f: &mut Frame, area: Rect, bs: &Backstage) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    let focused = bs.pane == Pane::Browser;

    // File list.
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        format!("Open — {}", bs.dir.to_string_lossy()),
        Style::default().add_modifier(Modifier::DIM),
    ))];
    let h = cols[0].height.saturating_sub(1) as usize;
    let start = bs.sel.saturating_sub(h.saturating_sub(1));
    for (i, e) in bs.entries.iter().enumerate().skip(start).take(h) {
        let icon = if e.is_parent { "⬑ " } else if e.is_dir { "▸ " } else { "  " };
        let size = e.size_str();
        let label = format!("{icon}{}", e.name);
        let text = if size.is_empty() { label } else { format!("{label:<28} {size:>8}") };
        let mut line = Line::from(text);
        if i == bs.sel && focused {
            line.style = Style::default().add_modifier(Modifier::REVERSED);
        } else if e.locked {
            line.style = Style::default().add_modifier(Modifier::DIM);
        }
        lines.push(line);
    }
    f.render_widget(Paragraph::new(lines), cols[0]);

    // Preview of the highlighted project.
    let mut prev: Vec<Line> = vec![Line::from(Span::styled("Preview", Style::default().add_modifier(Modifier::DIM)))];
    prev.extend(bs.preview.iter().map(|s| Line::from(s.clone())));
    f.render_widget(Paragraph::new(prev).block(Block::default().borders(Borders::LEFT)), cols[1]);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let fin = app.sched.project_finish.parts();
    let crit = app.proj.tasks.iter().filter(|t| app.sched.get(t.uid).is_some_and(|r| r.critical) && !t.summary).count();
    let name = if app.proj.name.is_empty() { "Untitled" } else { &app.proj.name };
    let title = format!(
        " {name}{}   finish {:04}-{:02}-{:02}   {} task(s), {crit} critical ",
        if app.dirty { " *" } else { "" },
        fin.year, fin.month, fin.day,
        app.proj.tasks.len(),
    );
    let mut spans = vec![Span::styled(title, Style::default().add_modifier(Modifier::BOLD))];
    // Baseline variance for the selected task.
    if let Some(t) = app.proj.tasks.get(app.sel) {
        if let (Some(bf), Some(r)) = (t.baseline_finish, app.sched.get(t.uid)) {
            let delta = r.early_finish.day_number() - bf.day_number();
            let (label, color) = match delta.cmp(&0) {
                std::cmp::Ordering::Greater => (format!("▲ {delta}d late"), CRIT),
                std::cmp::Ordering::Less => (format!("▼ {}d early", -delta), ONTRACK),
                std::cmp::Ordering::Equal => ("on baseline".to_string(), Color::Gray),
            };
            spans.push(Span::styled(format!("· {}: {label} ", truncate(&t.name, 16)), Style::default().fg(color)));
        }
        // Resources assigned to the selected task.
        let res = task_resources(&app.proj, t.uid);
        if !res.is_empty() {
            spans.push(Span::styled(format!("· 👤 {} ", res.join(", ")), Style::default().add_modifier(Modifier::DIM)));
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
    if app.sel < app.top {
        app.top = app.sel;
    } else if inner_h > 0 && app.sel >= app.top + inner_h {
        app.top = app.sel + 1 - inner_h;
    }
    let visible = inner_h.max(1);
    let end = (app.top + visible).min(app.proj.tasks.len());

    // ---- left: task table ----
    let mut left_lines: Vec<Line> = Vec::new();
    left_lines.push(Line::from(Span::styled(
        format!(" {:<26} {:>5} {:>6}", "Task", "Dur", "Slack"),
        Style::default().fg(Color::Gray).add_modifier(Modifier::DIM),
    )));
    for i in app.top..end {
        let t = &app.proj.tasks[i];
        let r = app.sched.get(t.uid);
        let indent = "  ".repeat((t.outline_level.saturating_sub(1)) as usize);
        let bullet = if t.summary { "▾ " } else if t.is_milestone() { "◆ " } else { "• " };
        let res = task_resources(&app.proj, t.uid);
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
            fmt_days(app.proj.minutes_to_days(t.duration_min))
        };
        let slack = r.map(|r| fmt_days(app.proj.minutes_to_days(r.total_slack_min.max(0)))).unwrap_or_else(|| "?".into());
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
        if i == app.sel {
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
    let start = app.sched.project_start.parts();
    let mut right_lines: Vec<Line> = Vec::new();
    right_lines.push(build_scale(gw, app.hscroll, app.base_day));
    for i in app.top..end {
        let t = &app.proj.tasks[i];
        let crit = app.sched.get(t.uid).is_some_and(|r| r.critical);
        let s_day = app.disp_start(t.uid).map(|d| d.day_number() - app.base_day).unwrap_or(i64::MAX);
        let e_day = app.disp_finish(t.uid).map(|d| d.day_number() - app.base_day).unwrap_or(i64::MIN);
        let mut line = build_gantt_row(gw, app.hscroll, app.base_day, s_day, e_day, crit, t.summary, t.is_milestone());
        if i == app.sel {
            line.style = Style::default().bg(app.sel_bg());
        }
        right_lines.push(line);
    }
    let lev = if app.leveled { " · leveled" } else { "" };
    let gtitle = format!(" Gantt — from {:04}-{:02}-{:02} (◀ ▶ scroll){lev} ", start.year, start.month, start.day);
    f.render_widget(
        Paragraph::new(right_lines).block(Block::default().borders(Borders::ALL).title(gtitle)),
        right,
    );
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
    Line::from(Span::styled(buf.into_iter().collect::<String>(), Style::default().fg(WEEKEND)))
}

/// One task's bar across the visible day columns. `s_day`/`e_day` are day
/// offsets from the gantt origin (the project start day).
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
            spans.push(Span::styled("◆", Style::default().fg(MILESTONE).add_modifier(Modifier::BOLD)));
        } else if is_summary && in_span {
            // rollup bar: end caps + a thin spine, distinct from task bars
            let ch = if day == s_day || day == e_day { "▟" } else { "▬" };
            spans.push(Span::styled(ch, Style::default().fg(SUMMARY).add_modifier(Modifier::BOLD)));
        } else if !milestone && in_span {
            spans.push(Span::styled("█", Style::default().fg(bar_color)));
        } else if weekend {
            spans.push(Span::styled("·", Style::default().fg(WEEKEND).add_modifier(Modifier::DIM)));
        } else {
            spans.push(Span::raw(" "));
        }
    }
    Line::from(spans)
}

fn draw_status(f: &mut Frame, area: Rect, app: &App) {
    let help = "n add · d dur · p dep · Tab indent · Enter rename · x del · Ctrl+F find · Ctrl+Z undo · Ctrl+S save · q quit";
    let text = if app.status.is_empty() { help.to_string() } else { app.status.clone() };
    let mut spans = Vec::new();
    if app.vim {
        spans.push(Span::styled(" -- VIM -- ", Style::default().fg(Color::Black).bg(Color::Green)));
    }
    spans.push(Span::styled(format!(" {text}"), Style::default().fg(Color::Gray)));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_prompt(f: &mut Frame, area: Rect, app: &App) {
    let Some(p) = &app.prompt else { return };
    let w = area.width.clamp(20, 60);
    let x = (area.width.saturating_sub(w)) / 2;
    let y = area.height / 2;
    let rect = Rect { x, y, width: w, height: 3 };
    f.render_widget(Clear, rect);
    let content = Line::from(vec![
        Span::styled(format!(" {}: ", p.label), Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(p.buf.clone()),
        Span::styled("▏", Style::default().fg(Color::Gray)),
    ]);
    f.render_widget(
        Paragraph::new(content).block(Block::default().borders(Borders::ALL).title(" Enter ↵  Esc ✕ ")),
        rect,
    );
}

// ---- small helpers ----------------------------------------------------------

/// Names of the resources assigned to task `uid`, in assignment order.
fn task_resources(proj: &Project, uid: i32) -> Vec<String> {
    proj.assignments
        .iter()
        .filter(|a| a.task_uid == uid)
        .filter_map(|a| proj.resources.iter().find(|r| r.uid == a.resource_uid).map(|r| r.name.clone()))
        .collect()
}

/// Prefill text for the constraint prompt from a task's current constraint.
fn constraint_hint(t: &Task) -> String {
    let code = match t.constraint {
        ConstraintType::AsSoonAsPossible => return String::new(),
        ConstraintType::AsLateAsPossible => "ALAP",
        ConstraintType::StartNoEarlierThan => "SNET",
        ConstraintType::StartNoLaterThan => "SNLT",
        ConstraintType::FinishNoEarlierThan => "FNET",
        ConstraintType::FinishNoLaterThan => "FNLT",
        ConstraintType::MustStartOn => "MSO",
        ConstraintType::MustFinishOn => "MFO",
    };
    match t.constraint_date {
        Some(d) => {
            let p = d.parts();
            format!("{code} {:04}-{:02}-{:02}", p.year, p.month, p.day)
        }
        None => code.to_string(),
    }
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
    fn window_title_format() {
        assert_eq!(window_title(&Some("/a/plan.yppx".into()), false), "yppxy - plan.yppx");
        assert_eq!(window_title(&Some("plan.xml".into()), true), "* yppxy - plan.xml");
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
        app.sel = 1;
        app.indent(1);
        app.recompute_summaries();
        assert!(app.proj.tasks[0].summary); // parent became a summary
        assert!(!app.proj.tasks[1].summary);
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
            predecessors: vec![Predecessor { uid: 1, link: LinkType::FinishStart, lag_min: 0 }],
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
        proj.tasks.push(Task { uid: 2, id: 2, name: "Build".into(), outline_level: 1, duration_min: 960, ..Task::default() });
        let mut app = App::new(proj, Some("plan.yppx".into()), false);
        let s = buffer_text(&mut app, 110, 24);
        assert!(s.contains("File") && s.contains("Task") && s.contains("Schedule") && s.contains("View"));
        assert!(s.contains("Milestone"), "ribbon body missing");
        assert!(s.contains("Gantt"), "gantt pane missing");
    }

    #[test]
    fn backstage_and_start_render() {
        // start screen
        let mut app = App::new(new_project(), None, false);
        assert!(app.start);
        let s = buffer_text(&mut app, 100, 22);
        assert!(s.contains("Welcome") && s.contains("New schedule"));

        // backstage
        let mut app = App::new(new_project(), Some("plan.yppx".into()), false);
        app.open_backstage();
        let s = buffer_text(&mut app, 100, 22);
        assert!(s.contains("File") && s.contains("Open") && s.contains("Export Gantt"));
    }

    #[test]
    fn undo_redo_restores_tasks() {
        let mut app = App::new(new_project(), None, false);
        assert_eq!(app.proj.tasks.len(), 1);
        app.add_task();
        app.add_task();
        assert_eq!(app.proj.tasks.len(), 3);
        app.undo();
        assert_eq!(app.proj.tasks.len(), 2);
        app.undo();
        assert_eq!(app.proj.tasks.len(), 1);
        app.redo();
        assert_eq!(app.proj.tasks.len(), 2);
        // a fresh edit clears the redo stack
        app.add_task();
        app.redo();
        assert_eq!(app.proj.tasks.len(), 3);
    }

    #[test]
    fn find_selects_matching_task_and_wraps() {
        let mut proj = new_project();
        proj.tasks[0].name = "Alpha".into();
        proj.tasks.push(Task { uid: 2, id: 2, name: "Bravo".into(), outline_level: 1, duration_min: 480, ..Task::default() });
        proj.tasks.push(Task { uid: 3, id: 3, name: "Charlie".into(), outline_level: 1, duration_min: 480, ..Task::default() });
        let mut app = App::new(proj, None, false);
        app.find("charlie");
        assert_eq!(app.sel, 2);
        // F3-style repeat from the end wraps back to Alpha (no more Charlie)
        app.find("");
        assert_eq!(app.sel, 2); // only one Charlie → stays
        app.find("a"); // matches Alpha/Bravo/Charlie — next after sel 2 wraps to 0
        assert_eq!(app.sel, 0);
    }

    #[test]
    fn vim_commands_save_and_quit() {
        let mut app = App::new(new_project(), Some("/nonexistent/dir/plan.yppx".into()), true);
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
        let r = app.sched.get(app.proj.tasks[0].uid).unwrap();
        assert_eq!(r.early_start.parts().day, 8);
    }

    #[test]
    fn baseline_captures_plan_and_variance_shows() {
        let mut app = App::new(new_project(), None, false);
        app.set_baseline();
        let bf = app.proj.tasks[0].baseline_finish.expect("baseline captured");
        app.set_duration("5d"); // extend past the baseline
        let r = app.sched.get(app.proj.tasks[0].uid).unwrap();
        assert!(r.early_finish.day_number() > bf.day_number(), "finish should slip past baseline");
    }

    #[test]
    fn assign_resource_creates_and_round_trips() {
        let mut app = App::new(new_project(), None, false);
        app.assign_resource("Alice");
        assert_eq!(app.proj.resources.len(), 1);
        assert_eq!(app.proj.assignments.len(), 1);
        // assigning the same resource again is a no-op
        app.assign_resource("alice");
        assert_eq!(app.proj.assignments.len(), 1);
        app.assign_resource("Bob");
        assert_eq!(app.proj.resources.len(), 2);
        let names = task_resources(&app.proj, app.proj.tasks[0].uid);
        assert_eq!(names, vec!["Alice".to_string(), "Bob".to_string()]);

        // resources/assignments survive a MSPDI round-trip
        let xml = mspdi::write_mspdi(&app.proj);
        let back = mspdi::read_mspdi(&xml).unwrap();
        assert_eq!(task_resources(&back, back.tasks[0].uid), names);

        // clearing removes the task's assignments (resources remain defined)
        app.assign_resource("");
        assert!(app.proj.assignments.is_empty());
        assert_eq!(app.proj.resources.len(), 2);
    }

    #[test]
    fn level_toggle_delays_shared_resource() {
        let mut proj = new_project(); // task 1 (1d)
        proj.tasks.push(Task { uid: 2, id: 2, name: "B".into(), outline_level: 1, duration_min: 480, ..Task::default() });
        let mut app = App::new(proj, None, false);
        app.sel = 0;
        app.assign_resource("Alice");
        app.sel = 1;
        app.assign_resource("Alice");
        // unleveled: both start the same day
        assert_eq!(app.disp_start(1).unwrap().day_number(), app.disp_start(2).unwrap().day_number());
        // leveled: task 2 waits for Alice to free up
        app.toggle_level();
        assert!(app.disp_start(2).unwrap().day_number() > app.disp_start(1).unwrap().day_number());
    }

    #[test]
    fn opens_mpp_metadata_as_partial_project() {
        // Build a minimal .mpp: a SummaryInformation property set with a title,
        // plus a stub task-data stream, wrapped in the CFB container.
        let title = "Bridge Retrofit";
        let mut sval: Vec<u8> = title.bytes().collect();
        sval.push(0);
        while !sval.len().is_multiple_of(4) {
            sval.push(0);
        }
        let mut values = Vec::new();
        values.extend_from_slice(&30u32.to_le_bytes()); // VT_LPSTR
        values.extend_from_slice(&((title.len() + 1) as u32).to_le_bytes());
        values.extend_from_slice(&sval);
        let mut index = Vec::new();
        index.extend_from_slice(&2u32.to_le_bytes()); // PID_TITLE
        index.extend_from_slice(&16u32.to_le_bytes()); // value offset within section
        let cb = 8 + index.len() + values.len();
        let mut sec = Vec::new();
        sec.extend_from_slice(&(cb as u32).to_le_bytes());
        sec.extend_from_slice(&1u32.to_le_bytes());
        sec.extend_from_slice(&index);
        sec.extend_from_slice(&values);
        let mut summary = Vec::new();
        summary.extend_from_slice(&0xFFFEu16.to_le_bytes());
        summary.extend_from_slice(&0u16.to_le_bytes());
        summary.extend_from_slice(&0u32.to_le_bytes());
        summary.extend_from_slice(&[0u8; 16]);
        summary.extend_from_slice(&1u32.to_le_bytes());
        summary.extend_from_slice(&[0u8; 16]);
        summary.extend_from_slice(&48u32.to_le_bytes());
        summary.extend_from_slice(&sec);

        let mpp = mppread::write_cfb(&[
            ("\u{5}SummaryInformation", summary),
            ("Props", vec![0u8; 12]),
        ]);
        let proj = project_from_mpp(&mpp).unwrap();
        assert_eq!(proj.name, "Bridge Retrofit");
        assert!(proj.tasks.is_empty()); // task decoding not implemented yet
    }

    #[test]
    fn add_and_delete_keep_schedule_consistent() {
        let mut app = App::new(new_project(), None, false);
        app.add_task();
        app.sel = 1;
        app.add_predecessor("1"); // depend on task 1
        assert_eq!(app.proj.tasks[1].predecessors.len(), 1);
        // deleting task 1 drops the dangling link
        app.sel = 0;
        app.delete_task();
        assert!(app.proj.tasks.iter().all(|t| t.predecessors.is_empty()));
    }
}
