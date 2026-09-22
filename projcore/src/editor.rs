//! Shared project editing, selection, history and derived scheduling state.
//!
//! Edits address stable task UIDs. Validation precedes the undo snapshot, so a
//! rejected edit leaves all editor state untouched. Hosts own file I/O and UI
//! messages; [`Editor::mark_saved`] acknowledges a successful save.

use crate::datetime::DateTime;
use crate::model::{Assignment, ConstraintType, LinkType, Predecessor, Project, Resource, Task};
use crate::schedule::{Leveled, Schedule, level, schedule};

const UNDO_CAP: usize = 100;

/// Fields updated atomically by an agent's `task.set` command.
#[derive(Default)]
pub struct TaskPatch {
    pub name: Option<String>,
    pub duration_min: Option<i64>,
    pub level: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AssignOutcome {
    Assigned,
    Cleared,
    AlreadyAssigned,
    NothingToClear,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FindOutcome {
    Inactive,
    Found(usize),
    NotFound,
}

/// A project and its editing session. Project access is read-only so mutations
/// cannot bypass validation, history or rescheduling.
pub struct Editor {
    proj: Project,
    sel: usize,
    undo: Vec<Project>,
    redo: Vec<Project>,
    dirty: bool,
    sched: Schedule,
    leveled: bool,
    level: Option<Leveled>,
    last_find: String,
}

impl Editor {
    pub fn new(mut proj: Project) -> Self {
        recompute_summaries(&mut proj);
        let sched = schedule(&proj);
        Self {
            proj,
            sel: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            dirty: false,
            sched,
            leveled: false,
            level: None,
            last_find: String::new(),
        }
    }

    /// Begin a new document session, retaining find and leveling preferences.
    pub fn replace_project(&mut self, proj: Project) {
        self.proj = proj;
        self.sel = 0;
        self.undo.clear();
        self.redo.clear();
        self.dirty = false;
        self.reschedule();
    }

    pub fn project(&self) -> &Project {
        &self.proj
    }
    pub fn sel(&self) -> usize {
        self.sel
    }
    pub fn select(&mut self, index: usize) {
        self.sel = index.min(self.proj.tasks.len().saturating_sub(1));
    }
    pub fn selected_uid(&self) -> Option<i32> {
        self.proj.tasks.get(self.sel).map(|t| t.uid)
    }
    pub fn dirty(&self) -> bool {
        self.dirty
    }
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }
    pub fn schedule(&self) -> &Schedule {
        &self.sched
    }
    pub fn leveled(&self) -> bool {
        self.leveled
    }
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }
    pub fn find_query(&self) -> &str {
        &self.last_find
    }

    pub fn toggle_level(&mut self) {
        self.leveled = !self.leveled;
        self.reschedule();
    }

    pub fn disp_start(&self, uid: i32) -> Option<DateTime> {
        match &self.level {
            Some(lv) => lv.start(uid),
            None => self.sched.get(uid).map(|r| r.early_start),
        }
    }

    pub fn disp_finish(&self, uid: i32) -> Option<DateTime> {
        match &self.level {
            Some(lv) => lv.finish(uid),
            None => self.sched.get(uid).map(|r| r.early_finish),
        }
    }

    fn index(&self, uid: i32) -> Result<usize, String> {
        self.proj
            .tasks
            .iter()
            .position(|t| t.uid == uid)
            .ok_or_else(|| format!("no task with uid {uid}"))
    }

    fn snapshot(&mut self) {
        self.undo.push(self.proj.clone());
        if self.undo.len() > UNDO_CAP {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn reschedule(&mut self) {
        recompute_summaries(&mut self.proj);
        self.sched = schedule(&self.proj);
        self.level = self.leveled.then(|| level(&self.proj));
    }

    fn changed(&mut self) {
        self.dirty = true;
        self.select(self.sel);
        self.reschedule();
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        self.redo.push(std::mem::replace(&mut self.proj, prev));
        self.changed();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.proj, next));
        self.changed();
        true
    }

    /// Find the next matching row, wrapping after the selected row. An empty
    /// query repeats the previous search. Searching never changes history.
    pub fn find(&mut self, query: &str) -> FindOutcome {
        let query = query.trim().to_lowercase();
        if !query.is_empty() {
            self.last_find = query;
        }
        let n = self.proj.tasks.len();
        if self.last_find.is_empty() || n == 0 {
            return FindOutcome::Inactive;
        }
        for step in 1..=n {
            let i = (self.sel + step) % n;
            if self.proj.tasks[i]
                .name
                .to_lowercase()
                .contains(&self.last_find)
            {
                self.sel = i;
                return FindOutcome::Found(i);
            }
        }
        FindOutcome::NotFound
    }

    /// Insert after a UID, or append; inherit the preceding row's outline level.
    /// The returned row is not automatically selected.
    pub fn add_task(
        &mut self,
        after: Option<i32>,
        name: &str,
        duration_min: i64,
    ) -> Result<usize, String> {
        let at = match after {
            Some(uid) => self.index(uid)? + 1,
            None => self.proj.tasks.len(),
        };
        let outline_level = at
            .checked_sub(1)
            .and_then(|i| self.proj.tasks.get(i))
            .map(|t| t.outline_level)
            .unwrap_or(1);
        let uid = self
            .proj
            .tasks
            .iter()
            .map(|t| t.uid)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("No task IDs available")?;
        self.snapshot();
        self.proj.tasks.insert(
            at,
            Task {
                uid,
                id: uid,
                name: name.into(),
                outline_level,
                duration_min,
                milestone: duration_min == 0,
                ..Task::default()
            },
        );
        self.changed();
        Ok(at)
    }

    pub fn delete_task(&mut self, uid: i32) -> Result<(), String> {
        let i = self.index(uid)?;
        self.snapshot();
        self.proj.tasks.remove(i);
        for t in &mut self.proj.tasks {
            t.predecessors.retain(|p| p.uid != uid);
        }
        self.changed();
        Ok(())
    }

    pub fn indent(&mut self, uid: i32, delta: i32) -> Result<(), String> {
        let i = self.index(uid)?;
        self.snapshot();
        let t = &mut self.proj.tasks[i];
        t.outline_level = (i64::from(t.outline_level) + i64::from(delta)).clamp(1, 20) as u32;
        self.changed();
        Ok(())
    }

    pub fn rename(&mut self, uid: i32, name: &str) -> Result<(), String> {
        self.update_task(
            uid,
            TaskPatch {
                name: Some(name.into()),
                ..TaskPatch::default()
            },
        )
    }

    pub fn set_duration(&mut self, uid: i32, text: &str) -> Result<(), String> {
        let min = parse_duration(text, &self.proj)
            .ok_or_else(|| format!("Couldn't read duration '{text}' (try 3d, 4h, 2w)"))?;
        self.set_duration_min(uid, min)
    }

    pub fn set_duration_min(&mut self, uid: i32, min: i64) -> Result<(), String> {
        self.update_task(
            uid,
            TaskPatch {
                duration_min: Some(min),
                ..TaskPatch::default()
            },
        )
    }

    pub fn update_task(&mut self, uid: i32, patch: TaskPatch) -> Result<(), String> {
        let i = self.index(uid)?;
        if patch.name.is_none() && patch.duration_min.is_none() && patch.level.is_none() {
            return Err("task.set needs at least one of 'name', 'duration', 'level'".into());
        }
        if patch.level.is_some_and(|lv| !(1..=20).contains(&lv)) {
            return Err("'level' must be 1..=20".into());
        }
        self.snapshot();
        let t = &mut self.proj.tasks[i];
        if let Some(name) = patch.name {
            t.name = name;
        }
        if let Some(min) = patch.duration_min {
            t.duration_min = min;
            t.milestone = min == 0;
        }
        if let Some(lv) = patch.level {
            t.outline_level = lv;
        }
        self.changed();
        Ok(())
    }

    pub fn add_predecessor(
        &mut self,
        uid: i32,
        pred: i32,
        link: LinkType,
        lag_min: i64,
    ) -> Result<(), String> {
        let i = self.index(uid)?;
        if uid == pred || self.index(pred).is_err() {
            return Err(format!("No other task with ID {pred}"));
        }
        if self.proj.tasks[i]
            .predecessors
            .iter()
            .any(|p| p.uid == pred)
        {
            return Err(format!("Already depends on {pred}"));
        }
        self.snapshot();
        self.proj.tasks[i].predecessors.push(Predecessor {
            uid: pred,
            link,
            lag_min,
        });
        self.changed();
        Ok(())
    }

    pub fn remove_predecessor(&mut self, uid: i32, pred: i32) -> Result<(), String> {
        let i = self.index(uid)?;
        if !self.proj.tasks[i]
            .predecessors
            .iter()
            .any(|p| p.uid == pred)
        {
            return Err(format!("task {uid} has no predecessor {pred}"));
        }
        self.snapshot();
        self.proj.tasks[i].predecessors.retain(|p| p.uid != pred);
        self.changed();
        Ok(())
    }

    pub fn set_constraint(&mut self, uid: i32, text: &str) -> Result<(), String> {
        let i = self.index(uid)?;
        let (constraint, constraint_date) = parse_constraint(text)?;
        self.snapshot();
        self.proj.tasks[i].constraint = constraint;
        self.proj.tasks[i].constraint_date = constraint_date;
        self.changed();
        Ok(())
    }

    pub fn assign_resource(&mut self, uid: i32, name: &str) -> Result<AssignOutcome, String> {
        let i = self.index(uid)?;
        let name = name.trim();
        if name.is_empty() {
            if !self.proj.assignments.iter().any(|a| a.task_uid == uid) {
                return Ok(AssignOutcome::NothingToClear);
            }
            self.snapshot();
            self.proj.assignments.retain(|a| a.task_uid != uid);
            self.changed();
            return Ok(AssignOutcome::Cleared);
        }
        let existing = self
            .proj
            .resources
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(name))
            .map(|r| r.uid);
        if existing.is_some_and(|rid| {
            self.proj
                .assignments
                .iter()
                .any(|a| a.task_uid == uid && a.resource_uid == rid)
        }) {
            return Ok(AssignOutcome::AlreadyAssigned);
        }
        let rid = match existing {
            Some(rid) => rid,
            None => self
                .proj
                .resources
                .iter()
                .map(|r| r.uid)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or("No resource IDs available")?,
        };
        let auid = self
            .proj
            .assignments
            .iter()
            .map(|a| a.uid)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("No assignment IDs available")?;
        self.snapshot();
        if existing.is_none() {
            self.proj.resources.push(Resource {
                uid: rid,
                id: self.proj.resources.len() as i32 + 1,
                name: name.into(),
                is_work: true,
                max_units: 1.0,
                calendar_uid: None,
            });
        }
        self.proj.assignments.push(Assignment {
            uid: auid,
            task_uid: uid,
            resource_uid: rid,
            units: 1.0,
            work_min: self.proj.tasks[i].duration_min,
        });
        self.changed();
        Ok(AssignOutcome::Assigned)
    }

    pub fn set_baseline(&mut self) {
        self.snapshot();
        for t in &mut self.proj.tasks {
            if let Some(r) = self.sched.get(t.uid) {
                t.baseline_start = Some(r.early_start);
                t.baseline_finish = Some(r.early_finish);
            }
        }
        self.changed();
    }

    pub fn toggle_milestone(&mut self, uid: i32) -> Result<(), String> {
        let i = self.index(uid)?;
        let min = if self.proj.tasks[i].duration_min == 0 {
            480
        } else {
            0
        };
        self.set_duration_min(uid, min)
    }
}

fn recompute_summaries(proj: &mut Project) {
    let levels: Vec<u32> = proj.tasks.iter().map(|t| t.outline_level).collect();
    for (i, t) in proj.tasks.iter_mut().enumerate() {
        t.summary = levels.get(i + 1).is_some_and(|&nl| nl > levels[i]);
    }
}

/// Parse the TUI's duration units using the project's working-day length.
pub fn parse_duration(text: &str, proj: &Project) -> Option<i64> {
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

/// Parse `TYPE [date]`, retaining the TUI's input and rejection conventions.
pub fn parse_constraint(text: &str) -> Result<(ConstraintType, Option<DateTime>), String> {
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
        _ => return Err("Constraint: TYPE [date] — SNET/SNLT/FNET/FNLT/MSO/MFO/ALAP/none".into()),
    };
    let needs_date = !matches!(
        ctype,
        ConstraintType::AsSoonAsPossible | ConstraintType::AsLateAsPossible
    );
    let date = it.next().and_then(DateTime::parse_mspdi);
    if needs_date && date.is_none() {
        return Err(format!(
            "{} needs a date, e.g. {kind} 2026-03-05",
            kind.to_uppercase()
        ));
    }
    Ok((ctype, if needs_date { date } else { None }))
}

/// Prompt prefill; ASAP intentionally uses an empty string.
pub fn constraint_hint(t: &Task) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::TaskResult;

    fn editor() -> Editor {
        Editor::new(Project {
            start_date: Some(DateTime::from_ymd_hm(2026, 1, 5, 8, 0)),
            tasks: (1..=2)
                .map(|uid| Task {
                    uid,
                    id: uid,
                    name: format!("Task {uid}"),
                    outline_level: 1,
                    duration_min: 480,
                    ..Task::default()
                })
                .collect(),
            ..Project::default()
        })
    }

    fn results(sched: &Schedule) -> Vec<TaskResult> {
        let mut results: Vec<_> = sched.results().copied().collect();
        results.sort_by_key(|r| r.uid);
        results
    }

    fn assert_schedule(ed: &Editor) {
        let expected = schedule(ed.project());
        assert_eq!(results(ed.schedule()), results(&expected));
        assert_eq!(ed.schedule().project_start, expected.project_start);
        assert_eq!(ed.schedule().project_finish, expected.project_finish);
        if ed.leveled() {
            let expected = level(ed.project());
            for t in &ed.project().tasks {
                assert_eq!(ed.disp_start(t.uid), expected.start(t.uid));
                assert_eq!(ed.disp_finish(t.uid), expected.finish(t.uid));
            }
            assert_eq!(
                ed.level.as_ref().unwrap().project_finish,
                expected.project_finish
            );
        }
    }

    // Compare all state, including actual history contents and derived dates.
    fn assert_unchanged(ed: &mut Editor, action: impl FnOnce(&mut Editor)) {
        let proj = ed.proj.clone();
        let undo = ed.undo.clone();
        let redo = ed.redo.clone();
        let flags = (ed.dirty, ed.sel, ed.leveled, ed.last_find.clone());
        let sched = ed.sched.clone();
        let dates: Vec<_> = ed
            .proj
            .tasks
            .iter()
            .map(|t| (t.uid, ed.disp_start(t.uid), ed.disp_finish(t.uid)))
            .collect();
        let level_finish = ed.level.as_ref().map(|l| l.project_finish);
        action(ed);
        assert_eq!(ed.proj, proj);
        assert_eq!(ed.undo, undo);
        assert_eq!(ed.redo, redo);
        assert_eq!((ed.dirty, ed.sel, ed.leveled, ed.last_find.clone()), flags);
        assert_eq!(results(&ed.sched), results(&sched));
        assert_eq!(ed.sched.project_start, sched.project_start);
        assert_eq!(ed.sched.project_finish, sched.project_finish);
        assert_eq!(ed.level.as_ref().map(|l| l.project_finish), level_finish);
        for (uid, start, finish) in dates {
            assert_eq!(ed.disp_start(uid), start);
            assert_eq!(ed.disp_finish(uid), finish);
        }
    }

    fn assert_edit(ed: &mut Editor, action: impl FnOnce(&mut Editor)) {
        let before = ed.project().clone();
        let depth = ed.undo_depth();
        action(ed);
        let after = ed.project().clone();
        assert_ne!(before, after);
        assert!(ed.dirty());
        assert_eq!(ed.undo_depth(), depth + 1);
        assert_eq!(ed.redo_depth(), 0);
        assert_schedule(ed);
        assert!(ed.undo());
        assert_eq!(ed.project(), &before);
        assert_schedule(ed);
        assert!(ed.redo());
        assert_eq!(ed.project(), &after);
        assert_schedule(ed);
    }

    #[test]
    fn every_operation_is_one_undoable_rescheduled_edit() {
        let mut ed = editor();
        ed.toggle_level();
        assert_edit(&mut ed, |e| {
            e.add_task(Some(1), "Inserted", 960).unwrap();
        });
        assert_eq!(ed.project().tasks[1].name, "Inserted");
        assert_edit(&mut ed, |e| {
            e.rename(1, "Design").unwrap();
        });
        assert_edit(&mut ed, |e| {
            e.set_duration(1, "3d").unwrap();
        });
        assert_eq!(ed.project().tasks[0].duration_min, 1440);
        assert_edit(&mut ed, |e| {
            e.indent(3, 1).unwrap();
        });
        assert!(ed.project().tasks[0].summary);
        assert_edit(&mut ed, |e| {
            e.add_predecessor(2, 3, LinkType::FinishStart, 60).unwrap();
        });
        assert!(ed.disp_start(2).unwrap() > ed.disp_finish(3).unwrap());
        assert_edit(&mut ed, |e| {
            e.set_constraint(3, "SNET 2026-01-08").unwrap();
        });
        assert_eq!(ed.schedule().get(3).unwrap().early_start.parts().day, 8);
        assert_edit(&mut ed, |e| {
            assert_eq!(
                e.assign_resource(3, "Alice").unwrap(),
                AssignOutcome::Assigned
            );
        });
        assert_edit(&mut ed, |e| e.set_baseline());
        let baseline = ed.project().tasks[1].baseline_finish.unwrap();
        assert_edit(&mut ed, |e| {
            e.set_duration_min(3, 4800).unwrap();
        });
        assert!(ed.schedule().get(3).unwrap().early_finish > baseline);
        assert_edit(&mut ed, |e| {
            e.toggle_milestone(3).unwrap();
        });
        assert!(ed.project().tasks[1].milestone);
        assert_eq!(ed.project().tasks[1].duration_min, 0);
        assert_edit(&mut ed, |e| {
            e.toggle_milestone(3).unwrap();
        });
        assert!(!ed.project().tasks[1].milestone);
        assert_eq!(ed.project().tasks[1].duration_min, 480);
        assert_edit(&mut ed, |e| {
            e.remove_predecessor(2, 3).unwrap();
        });
        assert!(ed.project().tasks[2].predecessors.is_empty());
        assert_edit(&mut ed, |e| {
            e.update_task(
                2,
                TaskPatch {
                    name: Some("Release".into()),
                    duration_min: Some(0),
                    level: Some(2),
                },
            )
            .unwrap();
        });
        let task = &ed.project().tasks[2];
        assert_eq!(
            (
                &*task.name,
                task.duration_min,
                task.outline_level,
                task.milestone
            ),
            ("Release", 0, 2, true)
        );
        assert_edit(&mut ed, |e| {
            e.delete_task(3).unwrap();
        });
        assert_eq!(ed.project().tasks.len(), 2);
    }

    #[test]
    fn rejected_edits_preserve_everything_clean_and_dirty() {
        type Edit = fn(&mut Editor) -> Result<(), String>;
        let bad: &[Edit] = &[
            |e| e.add_task(Some(999), "bad", 480).map(|_| ()),
            |e| e.delete_task(999),
            |e| e.indent(999, 1),
            |e| e.rename(999, "bad"),
            |e| e.set_duration(999, "1d"),
            |e| e.set_duration(1, "banana"),
            |e| e.set_duration_min(999, 480),
            |e| e.add_predecessor(999, 1, LinkType::FinishStart, 0),
            |e| e.add_predecessor(1, 999, LinkType::FinishStart, 0),
            |e| e.add_predecessor(1, 1, LinkType::FinishStart, 0),
            |e| e.add_predecessor(2, 1, LinkType::FinishStart, 0),
            |e| e.remove_predecessor(999, 1),
            |e| e.remove_predecessor(1, 2),
            |e| e.set_constraint(999, "none"),
            |e| e.set_constraint(1, "MSO"),
            |e| e.set_constraint(1, "SNET banana"),
            |e| e.set_constraint(1, "what"),
            |e| e.assign_resource(999, "Alice").map(|_| ()),
            |e| e.toggle_milestone(999),
            |e| {
                e.update_task(
                    999,
                    TaskPatch {
                        name: Some("bad".into()),
                        ..TaskPatch::default()
                    },
                )
            },
            |e| e.update_task(1, TaskPatch::default()),
            |e| {
                e.update_task(
                    1,
                    TaskPatch {
                        name: Some("bad".into()),
                        level: Some(0),
                        ..TaskPatch::default()
                    },
                )
            },
            |e| {
                e.update_task(
                    1,
                    TaskPatch {
                        duration_min: Some(0),
                        level: Some(21),
                        ..TaskPatch::default()
                    },
                )
            },
        ];
        for clean in [false, true] {
            let mut ed = editor();
            ed.add_predecessor(2, 1, LinkType::FinishStart, 0).unwrap();
            ed.assign_resource(1, "Alice").unwrap();
            ed.assign_resource(2, "Alice").unwrap();
            ed.toggle_level();
            ed.find("task 2");
            ed.rename(1, "Changed").unwrap();
            ed.undo();
            if clean {
                ed.mark_saved();
            }
            assert!(ed.redo_depth() > 0);
            for action in bad {
                assert_unchanged(&mut ed, |e| assert!(action(e).is_err()));
            }
        }
    }

    #[test]
    fn append_and_insert_inherit_preceding_level_without_selecting() {
        let mut ed = editor();
        ed.indent(1, 1).unwrap();
        ed.indent(2, 2).unwrap();
        ed.select(1);
        let at = ed.add_task(None, "Append", 480).unwrap();
        assert_eq!(at, 2);
        assert_eq!(ed.project().tasks[at].outline_level, 3);
        let at = ed.add_task(Some(1), "Insert", 480).unwrap();
        assert_eq!(at, 1);
        assert_eq!(ed.project().tasks[at].outline_level, 2);
        assert_eq!(ed.sel(), 1);
        ed.replace_project(Project::default());
        assert_eq!(ed.add_task(None, "First", 0).unwrap(), 0);
        assert_eq!(ed.project().tasks[0].outline_level, 1);
        assert!(ed.project().tasks[0].milestone);
    }

    #[test]
    fn deletion_drops_links_and_clamps_selection() {
        let mut ed = editor();
        ed.add_predecessor(1, 2, LinkType::FinishStart, 0).unwrap();
        ed.select(1);
        assert_edit(&mut ed, |e| {
            e.delete_task(2).unwrap();
        });
        assert_eq!(ed.sel(), 0);
        assert!(ed.project().tasks[0].predecessors.is_empty());
        ed.delete_task(1).unwrap();
        assert_eq!(ed.sel(), 0);
        assert_eq!(ed.selected_uid(), None);
        ed.select(usize::MAX);
        assert_eq!(ed.sel(), 0);
    }

    #[test]
    fn indent_clamps_and_recomputes_summaries() {
        let mut ed = editor();
        ed.indent(2, i32::MAX).unwrap();
        assert_eq!(ed.project().tasks[1].outline_level, 20);
        assert!(ed.project().tasks[0].summary);
        ed.indent(2, i32::MIN).unwrap();
        assert_eq!(ed.project().tasks[1].outline_level, 1);
        assert!(!ed.project().tasks[0].summary);
    }

    #[test]
    fn assignment_and_clear_refresh_an_enabled_leveling_overlay() {
        let mut ed = editor();
        ed.toggle_level();
        ed.assign_resource(1, " Alice ").unwrap();
        assert_eq!(ed.project().resources[0].name, "Alice");
        let before = ed.disp_start(2).unwrap();
        assert_edit(&mut ed, |e| {
            e.assign_resource(2, "alice").unwrap();
        });
        assert_eq!(ed.project().resources.len(), 1);
        assert!(ed.disp_start(2).unwrap() > before);
        assert_edit(&mut ed, |e| {
            assert_eq!(e.assign_resource(2, "").unwrap(), AssignOutcome::Cleared);
        });
        assert_eq!(ed.disp_start(2).unwrap(), before);
        ed.toggle_level();
        assert_eq!(
            ed.disp_start(2),
            Some(ed.schedule().get(2).unwrap().early_start)
        );
    }

    #[test]
    fn assignment_noops_preserve_history_and_dirty_flag() {
        let mut ed = editor();
        ed.assign_resource(1, "Alice").unwrap();
        ed.rename(2, "Redo this").unwrap();
        ed.undo();
        for clean in [false, true] {
            if clean {
                ed.mark_saved();
            }
            assert_unchanged(&mut ed, |e| {
                assert_eq!(
                    e.assign_resource(1, "ALICE").unwrap(),
                    AssignOutcome::AlreadyAssigned
                )
            });
            assert_unchanged(&mut ed, |e| {
                assert_eq!(
                    e.assign_resource(2, " ").unwrap(),
                    AssignOutcome::NothingToClear
                )
            });
        }
    }

    #[test]
    fn history_cap_redo_branch_and_selection_clamping() {
        let mut ed = editor();
        assert!(!ed.undo());
        assert!(!ed.redo());
        let at = ed.add_task(None, "Third", 480).unwrap();
        ed.select(at);
        assert!(ed.undo());
        assert_eq!(ed.sel(), 1);
        ed.rename(1, "Fresh edit").unwrap();
        assert_eq!(ed.redo_depth(), 0);
        ed.replace_project(editor().proj);
        for i in 0..101 {
            ed.rename(1, &format!("Edit {i}")).unwrap();
        }
        assert_eq!(ed.undo_depth(), 100);
        for _ in 0..100 {
            assert!(ed.undo());
        }
        assert!(!ed.undo());
        assert_eq!(ed.project().tasks[0].name, "Edit 0");
        for _ in 0..100 {
            assert!(ed.redo());
        }
        assert!(!ed.redo());
        assert_eq!(ed.undo_depth(), 100);
        assert_eq!(ed.project().tasks[0].name, "Edit 100");
    }

    #[test]
    fn replace_project_resets_document_state_and_keeps_preferences() {
        let mut ed = editor();
        ed.toggle_level();
        ed.find("TASK 2");
        ed.rename(1, "Renamed").unwrap();
        ed.rename(2, "Renamed again").unwrap();
        ed.undo();
        ed.replace_project(Project::default());
        assert!(ed.leveled());
        assert_eq!(ed.find_query(), "task 2");
        assert_eq!((ed.undo_depth(), ed.redo_depth(), ed.sel()), (0, 0, 0));
        assert!(!ed.dirty());
        assert_schedule(&ed);
    }

    #[test]
    fn find_normalizes_repeats_wraps_and_handles_inactive_search() {
        let mut ed = editor();
        assert_eq!(ed.find(""), FindOutcome::Inactive);
        assert_eq!(ed.find(" TASK "), FindOutcome::Found(1));
        assert_eq!(ed.find_query(), "task");
        assert_eq!(ed.find(""), FindOutcome::Found(0));
        assert_eq!(ed.find("missing"), FindOutcome::NotFound);
        assert_eq!(ed.sel(), 0);
        assert!(!ed.dirty());
        assert_eq!(ed.undo_depth(), 0);
        ed.replace_project(Project::default());
        assert_eq!(ed.find("task"), FindOutcome::Inactive);
    }

    #[test]
    fn duration_units_and_constraint_prefills() {
        let proj = Project::default();
        for (text, min) in [
            ("2d", 960),
            ("3", 1440),
            ("4h", 240),
            ("1w", 2400),
            ("30m", 30),
            (" 2D ", 960),
            ("-4h", -240),
        ] {
            assert_eq!(parse_duration(text, &proj), Some(min));
        }
        assert_eq!(parse_duration("nope", &proj), None);
        for code in ["SNET", "SNLT", "FNET", "FNLT", "MSO", "MFO", "ALAP"] {
            let text = if code == "ALAP" {
                code.to_string()
            } else {
                format!("{code} 2026-03-05")
            };
            let (constraint, constraint_date) = parse_constraint(&text).unwrap();
            let task = Task {
                constraint,
                constraint_date,
                ..Task::default()
            };
            assert_eq!(constraint_hint(&task), text);
            assert_eq!(
                parse_constraint(&constraint_hint(&task)).unwrap(),
                (constraint, constraint_date)
            );
        }
        for text in ["none", "asap"] {
            assert_eq!(
                parse_constraint(text).unwrap(),
                (ConstraintType::AsSoonAsPossible, None)
            );
        }
        assert_eq!(constraint_hint(&Task::default()), "");
        assert!(parse_constraint("").is_err());
    }
}
