//! Project 2024's Report › View Reports, as Markdown.
//!
//! Only the reports the plan's stored data backs are here: Dashboards ›
//! Project Overview, Resources › Resource Overview and Overallocated
//! Resources, Costs › Task Cost Overview and Resource Cost Overview, and In
//! Progress › Critical Tasks, Late Tasks and Milestone Report. The ones that
//! need timephased data, charts or earned value (Burndown, Cash Flow, Cost
//! Overview, Work Overview, Earned Value Report) are not.
//!
//! Every value is the one the sheet shows: task fields go through
//! [`FieldReader`], and costs and work are the stored totals the editor keeps
//! current after each edit, never repriced. Project totals are the project
//! summary row's (UID 0) when the plan has one, else the sum of its outline
//! level 1 rows, so a summary is never counted twice. Blank rows and inactive
//! tasks are left out. Lateness is measured at [`Project::status_date`]; a
//! plan without one says so instead of flagging nothing. Nothing reads the
//! clock, so a report of the same plan is always the same text.

use crate::datetime::DateTime;
use crate::editor::{
    BaselinePart, Editor, Field, FieldReader, FieldValue, format_money, format_project_date,
    format_work, two_decimals,
};
use crate::gantt::{heading_text, table_cell};
use crate::model::{Project, Resource, ResourceType, Task};

/// One View Reports item.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ReportKind {
    ProjectOverview,
    ResourceOverview,
    OverallocatedResources,
    TaskCostOverview,
    ResourceCostOverview,
    CriticalTasks,
    LateTasks,
    MilestoneReport,
}

impl ReportKind {
    /// Every report, in View Reports menu order.
    pub const ALL: [ReportKind; 8] = [
        ReportKind::ProjectOverview,
        ReportKind::ResourceOverview,
        ReportKind::OverallocatedResources,
        ReportKind::TaskCostOverview,
        ReportKind::ResourceCostOverview,
        ReportKind::CriticalTasks,
        ReportKind::LateTasks,
        ReportKind::MilestoneReport,
    ];

    /// The name in file names and on the command line: `late-tasks`.
    pub fn slug(self) -> &'static str {
        match self {
            ReportKind::ProjectOverview => "project-overview",
            ReportKind::ResourceOverview => "resource-overview",
            ReportKind::OverallocatedResources => "overallocated-resources",
            ReportKind::TaskCostOverview => "task-cost-overview",
            ReportKind::ResourceCostOverview => "resource-cost-overview",
            ReportKind::CriticalTasks => "critical-tasks",
            ReportKind::LateTasks => "late-tasks",
            ReportKind::MilestoneReport => "milestone-report",
        }
    }

    /// Project's name for the report: `Late Tasks`.
    pub fn title(self) -> &'static str {
        match self {
            ReportKind::ProjectOverview => "Project Overview",
            ReportKind::ResourceOverview => "Resource Overview",
            ReportKind::OverallocatedResources => "Overallocated Resources",
            ReportKind::TaskCostOverview => "Task Cost Overview",
            ReportKind::ResourceCostOverview => "Resource Cost Overview",
            ReportKind::CriticalTasks => "Critical Tasks",
            ReportKind::LateTasks => "Late Tasks",
            ReportKind::MilestoneReport => "Milestone Report",
        }
    }

    /// The View Reports menu the report is on.
    pub fn menu(self) -> &'static str {
        match self {
            ReportKind::ProjectOverview => "Dashboards",
            ReportKind::ResourceOverview | ReportKind::OverallocatedResources => "Resources",
            ReportKind::TaskCostOverview | ReportKind::ResourceCostOverview => "Costs",
            ReportKind::CriticalTasks | ReportKind::LateTasks | ReportKind::MilestoneReport => {
                "In Progress"
            }
        }
    }

    /// The report a slug names.
    pub fn parse(slug: &str) -> Option<ReportKind> {
        ReportKind::ALL.into_iter().find(|k| k.slug() == slug)
    }

    /// Every slug, comma-separated, for a usage message.
    pub fn slugs() -> String {
        ReportKind::ALL.map(ReportKind::slug).join(", ")
    }

    /// The file this report of the plan named `stem` is written to:
    /// `<stem>-<slug>.md`.
    pub fn file_name(self, stem: &str) -> String {
        format!("{stem}-{}.md", self.slug())
    }
}

/// Render one report of the editor's plan as Markdown.
pub fn render(ed: &Editor, kind: ReportKind) -> String {
    let proj = ed.project();
    let name = heading_text(&proj.title)
        .or_else(|| heading_text(&proj.name))
        .unwrap_or_else(|| "Project".into());
    let mut out = format!("# {name}: {}\n\n", kind.title());
    let r = Report {
        ed,
        proj,
        fields: FieldReader::new(ed),
        active: proj.effective_activity(),
    };
    match kind {
        ReportKind::ProjectOverview => r.project_overview(&mut out),
        ReportKind::ResourceOverview => r.resource_overview(&mut out),
        ReportKind::OverallocatedResources => r.overallocated(&mut out),
        ReportKind::TaskCostOverview => r.task_costs(&mut out),
        ReportKind::ResourceCostOverview => r.resource_costs(&mut out),
        ReportKind::CriticalTasks => r.critical(&mut out),
        ReportKind::LateTasks => r.late(&mut out),
        ReportKind::MilestoneReport => r.milestones(&mut out),
    }
    // One newline at the end, whatever the last section left.
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

struct Report<'a> {
    ed: &'a Editor,
    proj: &'a Project,
    fields: FieldReader<'a>,
    /// Each row's effective activity: a task under an inactive summary is
    /// dormant whatever its own flag says.
    active: Vec<bool>,
}

/// The columns of a task list in the In Progress reports.
const TASK_COLUMNS: [Field; 5] = [
    Field::Start,
    Field::Finish,
    Field::Duration,
    Field::PercentComplete,
    Field::ResourceNames,
];

impl Report<'_> {
    /// The rows a report covers: no blank row, no inactive task (nor one
    /// under an inactive summary), and not the project summary row, which a
    /// report shows as its totals.
    fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.proj
            .tasks
            .iter()
            .zip(&self.active)
            .filter(|(t, active)| **active && !t.is_project_summary())
            .map(|(t, _)| t)
    }

    fn leaves(&self) -> impl Iterator<Item = &Task> {
        self.tasks().filter(|t| !t.summary)
    }

    fn resources(&self) -> impl Iterator<Item = &Resource> {
        self.proj
            .resources
            .iter()
            .filter(|r| r.is_null != Some(true))
    }

    fn text(&self, task: &Task, field: Field) -> String {
        table_cell(&self.fields.read(task, field).text).unwrap_or_default()
    }

    /// A task's name cell, bold for a summary.
    fn name(&self, task: &Task) -> String {
        let name = table_cell(&task.name).unwrap_or_else(|| format!("Task {}", task.id));
        if task.summary {
            format!("**{name}**")
        } else {
            name
        }
    }

    fn row(&self, task: &Task, fields: &[Field]) -> Vec<String> {
        std::iter::once(self.name(task))
            .chain(fields.iter().map(|&f| self.text(task, f)))
            .collect()
    }

    fn project_summary(&self) -> Option<&Task> {
        self.proj
            .tasks
            .iter()
            .zip(&self.active)
            .find(|(t, active)| **active && t.is_project_summary())
            .map(|(t, _)| t)
    }

    /// A project total of a money or work field: the project summary row's,
    /// else the sum over the outline level 1 rows. Fixed Cost does not roll
    /// up (a summary's is its own), so its total is every row's.
    fn total(&self, field: Field) -> String {
        if field == Field::FixedCost {
            let units: f64 = self
                .tasks()
                .chain(self.project_summary())
                .filter_map(|t| t.fixed_cost.as_ref().and_then(|c| c.to_f64()))
                .sum::<f64>()
                / 100.0;
            return format_money(units);
        }
        if let Some(sum) = self.project_summary() {
            return self.fields.read(sum, field).text;
        }
        let (mut money, mut minutes) = (0.0, 0);
        for t in self.tasks().filter(|t| t.outline_level == 1) {
            match self.fields.read(t, field).value {
                FieldValue::Money(units) => money += units,
                FieldValue::Minutes(min) => minutes += min,
                _ => {}
            }
        }
        match field {
            Field::Work | Field::ActualWork | Field::RemainingWork => format_work(minutes),
            _ => format_money(money),
        }
    }

    /// The earliest start or latest finish shown for the tasks a report
    /// covers (leveled while leveling is on), so a dormant task's span is
    /// not the project's; nor is an active summary's that rolls up only
    /// dormant tasks, which the scheduler treats as dormant too.
    fn bound(
        &self,
        pick: fn(DateTime, DateTime) -> DateTime,
        date: impl Fn(i32) -> Option<DateTime>,
    ) -> String {
        let dormant = crate::schedule::dormant_uids(self.proj);
        self.tasks()
            .filter(|t| !dormant.contains(&t.uid))
            .filter_map(|t| date(t.uid))
            .reduce(pick)
            .map_or_else(|| "NA".into(), format_project_date)
    }

    /// The project's % Complete: the summary row's, else the leaves' %
    /// Complete weighted by duration, as Project rolls a summary up.
    fn percent_complete(&self) -> String {
        if let Some(sum) = self.project_summary() {
            return self.fields.read(sum, Field::PercentComplete).text;
        }
        // In whole numbers, so a leaf at 29% reads 29%, not a float's 28.99.
        let (mut done, mut all) = (0i128, 0i128);
        for t in self.leaves() {
            let min = i128::from(t.duration_min.max(0));
            done += min * i128::from(t.percent_complete.unwrap_or(0));
            all += min;
        }
        format!("{}%", if all > 0 { done / all } else { 0 })
    }

    /// The plan's status date line, or why lateness is not computed.
    fn status_date(&self, out: &mut String) -> bool {
        match self.proj.status_date() {
            Some(d) => {
                out.push_str(&format!("Status date: {}\n\n", format_project_date(d)));
                true
            }
            None => {
                out.push_str("No status date: lateness not computed.\n\n");
                false
            }
        }
    }

    fn is_late(&self, task: &Task) -> bool {
        self.fields.read(task, Field::Status).text == "Late"
    }

    fn is_milestone(&self, task: &Task) -> bool {
        !task.summary && self.fields.read(task, Field::Milestone).value == FieldValue::Bool(true)
    }

    fn project_overview(&self, out: &mut String) {
        if self.tasks().next().is_none() {
            out.push_str("No tasks.\n");
            return;
        }
        let has_status = self.status_date(out);
        table(
            out,
            &["Start", "Finish", "% Complete", "Work", "Cost"],
            vec![vec![
                self.bound(DateTime::min, |uid| self.ed.disp_start(uid)),
                self.bound(DateTime::max, |uid| self.ed.disp_finish(uid)),
                self.percent_complete(),
                self.total(Field::Work),
                self.total(Field::Cost),
            ]],
        );
        out.push_str("## % Complete\n\n");
        let top: Vec<_> = self
            .tasks()
            .filter(|t| t.outline_level == 1)
            .map(|t| self.row(t, &[Field::PercentComplete]))
            .collect();
        table(out, &["Task", "% Complete"], top);
        out.push_str("## Milestones Due\n\n");
        let due: Vec<_> = self
            .leaves()
            .filter(|t| self.is_milestone(t) && t.percent_complete != Some(100))
            .map(|t| self.row(t, &[Field::Finish]))
            .collect();
        table_or(out, &["Milestone", "Finish"], due, "No milestones due.");
        out.push_str("## Late Tasks\n\n");
        if has_status {
            self.late_table(out);
        } else {
            out.push_str("No status date: lateness not computed.\n");
        }
    }

    fn late_table(&self, out: &mut String) {
        let late: Vec<_> = self
            .leaves()
            .filter(|t| self.is_late(t))
            .map(|t| self.row(t, &TASK_COLUMNS))
            .collect();
        table_or(out, &task_headers("Task"), late, "No late tasks.");
    }

    fn late(&self, out: &mut String) {
        if self.tasks().next().is_none() {
            out.push_str("No tasks.\n");
            return;
        }
        if self.status_date(out) {
            self.late_table(out);
        }
    }

    fn critical(&self, out: &mut String) {
        if self.tasks().next().is_none() {
            out.push_str("No tasks.\n");
            return;
        }
        let rows: Vec<_> = self
            .leaves()
            .filter(|t| t.percent_complete != Some(100))
            .filter(|t| self.fields.read(t, Field::Critical).value == FieldValue::Bool(true))
            .map(|t| self.row(t, &TASK_COLUMNS))
            .collect();
        table_or(
            out,
            &task_headers("Task"),
            rows,
            "No critical tasks remain.",
        );
    }

    fn milestones(&self, out: &mut String) {
        if self.tasks().next().is_none() {
            out.push_str("No tasks.\n");
            return;
        }
        self.status_date(out);
        let rows: Vec<_> = self
            .leaves()
            .filter(|t| self.is_milestone(t))
            .map(|t| self.row(t, &[Field::Finish, Field::Deadline, Field::Status]))
            .collect();
        table_or(
            out,
            &["Milestone", "Finish", "Deadline", "Status"],
            rows,
            "No milestones.",
        );
    }

    fn task_costs(&self, out: &mut String) {
        const COLUMNS: [Field; 6] = [
            Field::FixedCost,
            Field::ActualCost,
            Field::RemainingCost,
            Field::Cost,
            Field::Baseline(0, BaselinePart::Cost),
            Field::CostVariance,
        ];
        if self.tasks().next().is_none() {
            out.push_str("No tasks.\n");
            return;
        }
        let mut rows: Vec<_> = self.tasks().map(|t| self.row(t, &COLUMNS)).collect();
        rows.push(
            std::iter::once("**Total**".to_string())
                .chain(COLUMNS.iter().map(|&f| self.total(f)))
                .collect(),
        );
        table(
            out,
            &[
                "Task",
                "Fixed Cost",
                "Actual Cost",
                "Remaining Cost",
                "Cost",
                "Baseline Cost",
                "Cost Variance",
            ],
            rows,
        );
    }

    fn resource_overview(&self, out: &mut String) {
        let rows: Vec<_> = self
            .resources()
            .map(|r| {
                vec![
                    resource_name(r),
                    type_name(r).into(),
                    quantity(r, r.work_min),
                    quantity(r, r.actual_work_min),
                    quantity(r, r.remaining_work_min),
                    r.start.map_or_else(|| "NA".into(), format_project_date),
                    r.finish.map_or_else(|| "NA".into(), format_project_date),
                ]
            })
            .collect();
        table_or(
            out,
            &[
                "Resource",
                "Type",
                "Work",
                "Actual Work",
                "Remaining Work",
                "Start",
                "Finish",
            ],
            rows,
            "No resources.",
        );
    }

    fn resource_costs(&self, out: &mut String) {
        let money = |rate: Option<&crate::model::Rate>| {
            rate.and_then(|r| r.to_f64()).unwrap_or(0.0) / 100.0
        };
        let mut sums = [0.0; 3];
        let mut rows: Vec<Vec<String>> = Vec::new();
        for r in self.resources() {
            let costs = [
                money(r.actual_cost.as_ref()),
                money(r.remaining_cost.as_ref()),
                money(r.cost.as_ref()),
            ];
            for (sum, c) in sums.iter_mut().zip(costs) {
                *sum += c;
            }
            rows.push(
                [resource_name(r), type_name(r).into()]
                    .into_iter()
                    .chain(costs.map(format_money))
                    .collect(),
            );
        }
        if rows.is_empty() {
            out.push_str("No resources.\n");
            return;
        }
        rows.push(
            ["**Total**".to_string(), String::new()]
                .into_iter()
                .chain(sums.map(format_money))
                .collect(),
        );
        table(
            out,
            &["Resource", "Type", "Actual Cost", "Remaining Cost", "Cost"],
            rows,
        );
    }

    /// Work resources booked past their capacity, as
    /// [`crate::schedule::overbooked`] finds them on the shown schedule.
    fn overallocated(&self, out: &mut String) {
        if self.resources().next().is_none() {
            out.push_str("No resources.\n");
            return;
        }
        let over = crate::schedule::overbooked(self.proj, |uid| {
            Some((self.ed.disp_start(uid)?, self.ed.disp_finish(uid)?))
        });
        let rows: Vec<_> = self
            .resources()
            .filter_map(|r| {
                let peak = over.get(&r.uid)?;
                Some(vec![
                    resource_name(r),
                    percent(max_units(r)),
                    percent(*peak),
                    format_work(r.work_min.unwrap_or(0)),
                ])
            })
            .collect();
        table_or(
            out,
            &["Resource", "Max Units", "Peak Units", "Work"],
            rows,
            "No overallocated resources.",
        );
    }
}

/// A resource's Max. Units, 100% when unusable (as the leveler reads it).
fn max_units(r: &Resource) -> f64 {
    if r.max_units > 0.0 { r.max_units } else { 1.0 }
}

fn percent(units: f64) -> String {
    format!("{}%", two_decimals(units * 100.0).0)
}

fn resource_name(r: &Resource) -> String {
    table_cell(&r.name).unwrap_or_else(|| format!("Resource {}", r.id))
}

fn type_name(r: &Resource) -> &'static str {
    match r.kind {
        ResourceType::Work => "Work",
        ResourceType::Material => "Material",
        ResourceType::Cost => "Cost",
    }
}

/// A resource's work as the Resource Sheet shows it: hours for a work
/// resource, a material's quantity with its label, nothing for a cost
/// resource.
fn quantity(r: &Resource, min: Option<i64>) -> String {
    let min = min.unwrap_or(0);
    match r.kind {
        ResourceType::Work => format_work(min),
        ResourceType::Material => {
            let qty = two_decimals(min as f64 / 60.0).0;
            match r.material_label.as_deref().and_then(table_cell) {
                Some(label) => format!("{qty} {label}"),
                None => qty,
            }
        }
        ResourceType::Cost => String::new(),
    }
}

fn task_headers(first: &'static str) -> [&'static str; 6] {
    [
        first,
        "Start",
        "Finish",
        "Duration",
        "% Complete",
        "Resource Names",
    ]
}

fn table(out: &mut String, headers: &[&str], rows: Vec<Vec<String>>) {
    out.push_str(&format!("| {} |\n", headers.join(" | ")));
    out.push_str(&format!(
        "|{}|\n",
        headers
            .iter()
            .map(|h| "-".repeat(h.len() + 2))
            .collect::<Vec<_>>()
            .join("|")
    ));
    for row in rows {
        out.push_str(&format!("| {} |\n", row.join(" | ")));
    }
    out.push('\n');
}

/// A table, or `empty` when it has no rows.
fn table_or(out: &mut String, headers: &[&str], rows: Vec<Vec<String>>, empty: &str) {
    if rows.is_empty() {
        out.push_str(empty);
        out.push_str("\n\n");
    } else {
        table(out, headers, rows);
    }
}

#[cfg(test)]
mod tests;
