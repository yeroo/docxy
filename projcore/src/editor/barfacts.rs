//! What the chart's bar rows ask of a task (#1696): the categories `Show For
//! ... Tasks` names, the date fields From and To read and the task fields a
//! bar's text shows. Pure reads of the plan; the rules are
//! [`crate::formatting::bars`].
use super::fields::CustomKind;
use super::*;
use crate::formatting::Facts;

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
        let name = name.trim();
        let compact: String = name
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        let field = match compact.as_str() {
            "" => default,
            "taskstart" => Field::Start,
            "taskfinish" => Field::Finish,
            "completethrough" => return self.complete_through(reader, task),
            _ => Field::parse(name).ok()?,
        };
        match reader.read(task, field).value {
            FieldValue::Date(d) => Some(d),
            _ => None,
        }
    }

    fn complete_through(&self, reader: &FieldReader, task: &Task) -> Option<DateTime> {
        if let Some(stop) = task.stop {
            return Some(stop);
        }
        let date = |f| match reader.read(task, f).value {
            FieldValue::Date(d) => Some(d),
            _ => None,
        };
        let (start, finish) = (date(Field::Start)?, date(Field::Finish)?);
        let percent = i64::from(task.percent_complete.unwrap_or(0).min(100));
        Some(start.add_minutes((finish.minutes() - start.minutes()) * percent / 100))
    }

    /// A bar's text field for a task as the sheet shows it; empty when the
    /// name is unknown.
    pub fn bar_text(&self, reader: &FieldReader, task: &Task, field: &str) -> String {
        Field::parse(field).map_or_else(|_| String::new(), |f| reader.read(task, f).text)
    }
}
