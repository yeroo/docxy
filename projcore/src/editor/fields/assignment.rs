//! Assignment fields by Project's own names (#395), the assignment
//! counterpart of the task registry, read with its conventions: `text` as
//! Project shows it, `value` underneath, and [`FieldValue::Null`] for a stored
//! quantity the assignment does not have (its text is what Project shows for
//! it, `0 hrs`, `$0.00`, `0%`). Work shows in hours (a material's as its
//! quantity and label), money as currency units, the delay in working days.
//! Peak and the budget fields read their stored values; nothing computes them
//! yet.
use super::*;
use crate::editor::cells::{format_quantity, format_units, shown_label};

/// One readable assignment field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AssignmentField {
    UniqueId,
    TaskId,
    TaskName,
    ResourceName,
    Units,
    Work,
    RegularWork,
    OvertimeWork,
    ActualWork,
    RemainingWork,
    BaselineWork,
    Cost,
    ActualCost,
    RemainingCost,
    BaselineCost,
    PercentWorkComplete,
    Start,
    Finish,
    Delay,
    CostRateTable,
    WorkContour,
    Peak,
    BudgetWork,
    BudgetCost,
}

const NAMES: &[(AssignmentField, &str)] = &[
    (AssignmentField::UniqueId, "Unique ID"),
    (AssignmentField::TaskId, "Task ID"),
    (AssignmentField::TaskName, "Task Name"),
    (AssignmentField::ResourceName, "Resource Name"),
    (AssignmentField::Units, "Units"),
    (AssignmentField::Work, "Work"),
    (AssignmentField::RegularWork, "Regular Work"),
    (AssignmentField::OvertimeWork, "Overtime Work"),
    (AssignmentField::ActualWork, "Actual Work"),
    (AssignmentField::RemainingWork, "Remaining Work"),
    (AssignmentField::BaselineWork, "Baseline Work"),
    (AssignmentField::Cost, "Cost"),
    (AssignmentField::ActualCost, "Actual Cost"),
    (AssignmentField::RemainingCost, "Remaining Cost"),
    (AssignmentField::BaselineCost, "Baseline Cost"),
    (AssignmentField::PercentWorkComplete, "% Work Complete"),
    (AssignmentField::Start, "Start"),
    (AssignmentField::Finish, "Finish"),
    (AssignmentField::Delay, "Delay"),
    (AssignmentField::CostRateTable, "Cost Rate Table"),
    (AssignmentField::WorkContour, "Work Contour"),
    (AssignmentField::Peak, "Peak"),
    (AssignmentField::BudgetWork, "Budget Work"),
    (AssignmentField::BudgetCost, "Budget Cost"),
];

impl AssignmentField {
    /// Every readable field, in registry order.
    pub fn all() -> Vec<AssignmentField> {
        NAMES.iter().map(|&(f, _)| f).collect()
    }

    /// Project's name for the field (`% Work Complete`).
    pub fn name(self) -> &'static str {
        NAMES
            .iter()
            .find(|(f, _)| *f == self)
            .map_or("", |(_, name)| name)
    }

    /// The field with this name, trimmed and matched without regard to ASCII
    /// case; the error names it as given.
    pub fn parse(name: &str) -> Result<AssignmentField, String> {
        let wanted = name.trim();
        NAMES
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(wanted))
            .map(|&(f, _)| f)
            .ok_or_else(|| format!("unknown assignment field '{name}'"))
    }
}

/// Every readable assignment field name, in registry order.
pub fn assignment_field_names() -> Vec<String> {
    NAMES.iter().map(|(_, n)| (*n).to_string()).collect()
}

/// A cost rate table's letter, `A` for 0 (and for an absent table).
pub fn rate_table_letter(table: Option<u8>) -> char {
    char::from(b'A' + table.unwrap_or(0).min(4))
}

/// A work contour as Project names it; an unknown code reads as Flat.
pub fn work_contour_name(code: Option<u8>) -> &'static str {
    match code.unwrap_or(0) {
        1 => "Back Loaded",
        2 => "Front Loaded",
        3 => "Double Peak",
        4 => "Early Peak",
        5 => "Late Peak",
        6 => "Bell",
        7 => "Turtle",
        8 => "Contoured",
        _ => "Flat",
    }
}

/// An assignment's own start and finish: the stored dates, which every edit
/// of its inputs refreshes, else the span its task's schedule gives it.
pub fn assignment_dates(ed: &Editor, a: &Assignment) -> Option<(DateTime, DateTime)> {
    a.start
        .zip(a.finish)
        .or_else(|| crate::assign::assignment_span(ed.project(), ed.schedule(), a))
}

/// Read one field of assignment `a` in editor `ed`.
pub fn read_assignment_field(ed: &Editor, a: &Assignment, field: AssignmentField) -> FieldRead {
    let proj = ed.project();
    let task = proj.task(a.task_uid);
    let resource = proj.resources.iter().find(|r| r.uid == a.resource_uid);
    let material = resource.filter(|r| r.kind == ResourceType::Material);
    // A material's work is its quantity, in hours, shown with its label.
    let work = |min: Option<i64>| match material {
        Some(r) => {
            let qty = format_quantity(min.unwrap_or(0) as f64 / 60.0);
            let text = match shown_label(r) {
                Some(label) => format!("{qty} {label}"),
                None => qty,
            };
            FieldRead::new(text, minutes(min))
        }
        None => super::work(min),
    };
    let baseline = a.baseline(0);
    match field {
        AssignmentField::UniqueId => int(i64::from(a.uid)),
        AssignmentField::TaskId => match task {
            Some(t) => int(i64::from(t.id)),
            None => FieldRead::new("", FieldValue::Null),
        },
        AssignmentField::TaskName => optional_text(task.map(|t| t.name.as_str())),
        AssignmentField::ResourceName => optional_text(resource.map(|r| r.name.as_str())),
        AssignmentField::Units => match resource.map(|r| r.kind) {
            Some(ResourceType::Cost) => FieldRead::new("", FieldValue::Null),
            Some(ResourceType::Material) => {
                let qty = format_quantity(a.units);
                let text = match material.and_then(shown_label) {
                    Some(label) => format!("{qty} {label}"),
                    None => qty,
                };
                FieldRead::new(text, FieldValue::Number(a.units))
            }
            _ => FieldRead::new(format_units(a.units), FieldValue::Number(a.units)),
        },
        AssignmentField::Work => work(Some(a.work_min)),
        AssignmentField::RegularWork => work(a.regular_work_min),
        AssignmentField::OvertimeWork => work(a.overtime_work_min),
        AssignmentField::ActualWork => work(a.actual_work_min),
        AssignmentField::RemainingWork => work(a.remaining_work_min),
        AssignmentField::BaselineWork => work(baseline.and_then(|b| b.work_min)),
        AssignmentField::Cost => money(a.cost.as_ref()),
        AssignmentField::ActualCost => money(a.actual_cost.as_ref()),
        AssignmentField::RemainingCost => money(a.remaining_cost.as_ref()),
        AssignmentField::BaselineCost => money(baseline.and_then(|b| b.cost.as_ref())),
        AssignmentField::PercentWorkComplete => percent(a.percent_work_complete),
        AssignmentField::Start => date(assignment_dates(ed, a).map(|(s, _)| s)),
        AssignmentField::Finish => date(assignment_dates(ed, a).map(|(_, f)| f)),
        AssignmentField::Delay => {
            let min = a.delay_min();
            FieldRead::new(
                format_duration_field(proj, min, DurationUnit::DAYS, false),
                FieldValue::Minutes(min),
            )
        }
        AssignmentField::CostRateTable => text(rate_table_letter(a.cost_rate_table).to_string()),
        AssignmentField::WorkContour => text(work_contour_name(a.work_contour)),
        AssignmentField::Peak => {
            let peak = a.peak_units.as_ref().and_then(Rate::to_f64);
            FieldRead::new(
                format_units(peak.unwrap_or(0.0)),
                peak.map_or(FieldValue::Null, FieldValue::Number),
            )
        }
        AssignmentField::BudgetWork => work(a.budget_work_min),
        AssignmentField::BudgetCost => money(a.budget_cost.as_ref()),
    }
}

#[cfg(test)]
mod tests;
