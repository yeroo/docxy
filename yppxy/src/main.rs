//! `yppxy` — a terminal viewer/**editor** for project schedules.
//!
//! The project-management sibling of `xlsxy`/`docxy`: where those sit on
//! `gridcore`/`docxcore`, this is the TUI shell over the pure `projcore` engine
//! — a task outline on the left, a live terminal Gantt chart on the right, and a
//! Critical Path Method reschedule after every edit.
//!
//! Usage:
//!   yppxy                              start a new schedule
//!   yppxy <file.(xml|yppx)>            open MSPDI XML or a native .yppx package
//!   yppxy <in> --gantt-md <out.md>     headless: export a Markdown Gantt chart
//!   yppxy <in> --save <out.(yppx|xml)> headless: convert/save and exit

use std::io;
use std::process::ExitCode;

use projcore::datetime::DateTime;
use projcore::model::{LinkType, Predecessor, Project, Task};
use projcore::schedule::{schedule, Schedule};
use projcore::{gantt, mspdi, yppx};

use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
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
            eprintln!("usage: yppxy [file.(xml|yppx)] [--gantt-md <out>] [--save <out>]");
            return ExitCode::from(2);
        }
    };
    if parsed.help {
        println!("usage: yppxy [file.(xml|yppx)] [--gantt-md <out>] [--save <out>]");
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

    match run_tui(proj, parsed.input) {
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
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut out = Args { input: None, gantt_md: None, save: None, help: false };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => out.help = true,
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
    if path.ends_with(".yppx") {
        yppx::read_yppx(&bytes)
    } else {
        let xml = String::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())?;
        mspdi::read_mspdi(&xml)
    }
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
}

impl App {
    fn new(proj: Project, path: Option<String>) -> App {
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
        };
        app.reschedule();
        app
    }

    fn reschedule(&mut self) {
        self.recompute_summaries();
        self.sched = schedule(&self.proj);
        self.base_day = self.sched.project_start.day_number();
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
        if let Some(t) = self.proj.tasks.get_mut(self.sel) {
            let lvl = (t.outline_level as i32 + delta).clamp(1, 20) as u32;
            t.outline_level = lvl;
            self.mark_dirty();
            self.reschedule();
        }
    }

    fn set_duration(&mut self, text: &str) {
        if let Some(min) = parse_duration(text, &self.proj) {
            if let Some(t) = self.proj.tasks.get_mut(self.sel) {
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
        if let Some(t) = self.proj.tasks.get_mut(self.sel) {
            t.name = text.to_string();
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
        let t = &mut self.proj.tasks[self.sel];
        if t.predecessors.iter().any(|p| p.uid == uid) {
            self.status = format!("Already depends on {uid}");
            return;
        }
        t.predecessors.push(Predecessor { uid, link: LinkType::FinishStart, lag_min: 0 });
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

fn run_tui(proj: Project, path: Option<String>) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(proj, path);
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
            Ok(_) => {}
            Err(e) => break Err(e),
        }
        if app.quit {
            break Ok(());
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    res
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

    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    app.status.clear();

    // Ctrl combinations first, so plain-letter shortcuts don't shadow them.
    if ctrl {
        match k.code {
            KeyCode::Char('s') => app.save(),
            KeyCode::Char('e') => app.export_md(),
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
        _ => {}
    }
}

// ---- drawing ----------------------------------------------------------------

fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)])
        .split(area);
    draw_header(f, rows[0], app);
    draw_body(f, rows[1], app);
    draw_status(f, rows[2], app);
    if app.prompt.is_some() {
        draw_prompt(f, area, app);
    }
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
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(title, Style::default().add_modifier(Modifier::BOLD)))),
        area,
    );
}

fn draw_body(f: &mut Frame, area: Rect, app: &mut App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(46), Constraint::Min(10)])
        .split(area);
    let left = cols[0];
    let right = cols[1];

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
        let namecol = truncate(&format!("{indent}{bullet}{}", t.name), 26);
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
            line.style = Style::default().bg(Color::Rgb(38, 48, 58));
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
        let r = app.sched.get(t.uid);
        let mut line = build_gantt_row(gw, app.hscroll, app.base_day, t, r);
        if i == app.sel {
            line.style = Style::default().bg(Color::Rgb(38, 48, 58));
        }
        right_lines.push(line);
    }
    let gtitle = format!(" Gantt — from {:04}-{:02}-{:02} (◀ ▶ scroll) ", start.year, start.month, start.day);
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

/// One task's bar across the visible day columns.
fn build_gantt_row(
    width: usize,
    hscroll: i64,
    base_day: i64,
    task: &Task,
    r: Option<&projcore::schedule::TaskResult>,
) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::with_capacity(width);
    let is_summary = task.summary;
    let milestone = task.is_milestone() && !is_summary;
    let (s_day, e_day, crit) = match r {
        Some(r) => (
            r.early_start.day_number() - base_day,
            r.early_finish.day_number() - base_day,
            r.critical,
        ),
        None => (i64::MAX, i64::MIN, false),
    };
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
    let help = "n add · d dur · p dep · Tab indent · Enter rename · x del · Ctrl+S save · Ctrl+E export · q quit";
    let text = if app.status.is_empty() { help.to_string() } else { app.status.clone() };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {text}"), Style::default().fg(Color::Gray)))),
        area,
    );
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
        let mut app = App::new(new_project(), None);
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
        let mut app = App::new(proj, Some(path));
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
        let mut app = App::new(proj, Some("plan.yppx".into()));
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

    #[test]
    fn add_and_delete_keep_schedule_consistent() {
        let mut app = App::new(new_project(), None);
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
