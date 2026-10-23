//! What the chart's bar rows ask of a task (#1696): the categories `Show For
//! ... Tasks` names, the date fields From and To read and the task fields a
//! bar's text shows. Pure reads of the plan; the rules are
//! [`crate::formatting::bars`].
use super::fields::CustomKind;
use super::*;
use crate::formatting::Facts;

/// How a bar row's From or To date lands on the day scale: a start-type date
/// is the start of its day and a finish-type one the end of it (the
/// chart's whole-day task bars), `CompleteThrough` is an instant placed
/// within its day by [`Editor::bar_day_fraction`], anything else the instant
/// itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BarDateKind {
    Start,
    Finish,
    Through,
    Instant,
}

/// What a From or To name stands for.
enum Named {
    Field(Field),
    Through,
}

/// The field (or `CompleteThrough`) a From or To `name` reads, `default`
/// when it is empty; `None` for an unknown name.
fn named(name: &str, default: Field) -> Option<Named> {
    let compact: String = name
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    Some(Named::Field(match compact.as_str() {
        "" => default,
        "taskstart" => Field::Start,
        "taskfinish" => Field::Finish,
        "completethrough" => return Some(Named::Through),
        _ => Field::parse(name).ok()?,
    }))
}

impl BarDateKind {
    /// The kind of the field a From or To `name` reads (`default` when it is
    /// empty): `Start`, `Task Start`, `Actual Start`, `Early/Late Start`,
    /// `Baseline<n> Start` and `Start1..10` start; the Finish twins finish;
    /// `CompleteThrough` is the progress; `Deadline` and the other dates are
    /// instants.
    pub fn of(name: &str, default: Field) -> BarDateKind {
        match named(name, default) {
            Some(Named::Through) => BarDateKind::Through,
            Some(Named::Field(
                Field::Start
                | Field::ActualStart
                | Field::EarlyStart
                | Field::LateStart
                | Field::Baseline(_, BaselinePart::Start)
                | Field::Custom(CustomKind::Start, _),
            )) => BarDateKind::Start,
            Some(Named::Field(
                Field::Finish
                | Field::ActualFinish
                | Field::EarlyFinish
                | Field::LateFinish
                | Field::Baseline(_, BaselinePart::Finish)
                | Field::Custom(CustomKind::Finish, _),
            )) => BarDateKind::Finish,
            _ => BarDateKind::Instant,
        }
    }
}

impl Editor {
    /// The task's categories. `flags` reads Flag1 to Flag20 (twenty field
    /// reads), which a host asks for only when a row names one
    /// ([`crate::formatting::Formatting::bars_use_flags`]).
    pub fn bar_facts(&self, reader: &FieldReader, task: &Task, flags: bool) -> Facts {
        let read = |field| reader.read(task, field).value;
        let mut facts = Facts {
            summary: task.summary,
            milestone: matches!(read(Field::Milestone), FieldValue::Bool(true)),
            critical: matches!(read(Field::Critical), FieldValue::Bool(true)),
            active: task.is_active(),
            manual: task.manual,
            external: task.external_task == Some(true),
            late: reader.read(task, Field::Status).text == "Late",
            flags: 0,
        };
        if flags {
            for n in 1..=20u8 {
                if matches!(
                    read(Field::Custom(CustomKind::Flag, n)),
                    FieldValue::Bool(true)
                ) {
                    facts.flags |= 1 << (n - 1);
                }
            }
        }
        facts
    }

    /// The date a bar row's From or To names for a task: a task date field
    /// (`Start`, `Finish`, `Actual Start`, `Deadline`, `Baseline1 Start`,
    /// `Start1`, ...), the `Task Start` / `Task Finish` aliases, or
    /// `CompleteThrough` (the task's `stop`, else its start plus its percent
    /// complete of its span: unmeasured against Project). An empty name is
    /// `default`; `None` when the name is unknown or the date is not set.
    pub fn bar_date(
        &self,
        reader: &FieldReader,
        task: &Task,
        name: &str,
        default: Field,
    ) -> Option<DateTime> {
        match named(name, default)? {
            Named::Through => self.complete_through(reader, task),
            Named::Field(field) => match reader.read(task, field).value {
                FieldValue::Date(d) => Some(d),
                _ => None,
            },
        }
    }

    /// Where a bar row's `CompleteThrough` is: the task's `stop`, else its
    /// start plus its percent complete of the working time between its start
    /// and finish on its calendar (unmeasured against Project), within start
    /// to finish. `None` without a start and finish.
    fn complete_through(&self, reader: &FieldReader, task: &Task) -> Option<DateTime> {
        let date = |f| match reader.read(task, f).value {
            FieldValue::Date(d) => Some(d),
            _ => None,
        };
        let (start, finish) = (date(Field::Start)?, date(Field::Finish)?);
        let through = match task.stop {
            Some(stop) => stop,
            None => {
                let proj = self.project();
                let work = crate::schedule::working_minutes_between_on(
                    proj,
                    task.calendar_uid,
                    start,
                    finish,
                );
                let done = work * i64::from(task.percent_complete.unwrap_or(0).min(100)) / 100;
                self.working_time_after(task.uid, start, done)
                    .unwrap_or(start)
            }
        };
        Some(through.max(start).min(finish))
    }

    /// How far through its working day `at` is on `task`'s calendar, 0 to 1:
    /// 0 at the day's first working minute (or before), 1 at its last (or
    /// after), linear between. A day without working time reads as 8:00 to
    /// 17:00, as a typed date does.
    pub fn bar_day_fraction(&self, task: &Task, at: DateTime) -> f64 {
        let proj = self.project();
        let first = super::cells::day_start(proj, task, at);
        let last = super::cells::day_end(proj, task, at);
        let span = (last.minutes() - first.minutes()) as f64;
        if at >= last {
            1.
        } else if at <= first || span <= 0. {
            0.
        } else {
            (at.minutes() - first.minutes()) as f64 / span
        }
    }

    /// A bar's text field for a task as the sheet shows it; empty when the
    /// name is unknown.
    pub fn bar_text(&self, reader: &FieldReader, task: &Task, field: &str) -> String {
        Field::parse(field).map_or_else(|_| String::new(), |f| reader.read(task, f).text)
    }
}
